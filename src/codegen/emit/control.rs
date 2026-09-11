use super::arrays::{
    array_element_addr, array_len, array_raw, array_version, emit_iterator_abort, make_iterator,
};
use super::clone::emit_deep_clone_value;
use super::cells::{emit_local_cell, emit_temporary_cell, ensure_current, pop_scope, push_scope};
use super::expr::emit_expr;
use super::destruction::{
    emit_cleanup_to_depth, emit_destroy_value, mark_owner_present, owner_presence,
    register_local_cleanup, register_temporary_cleanup,
};
use super::places::{emit_place_addr, emit_place_value};
use super::value::ExprValue;
use crate::codegen::abi::{norm_store, UserReturnPassing, VTy};
use crate::codegen::funcgen::emit_funclit_value_typed;
use crate::codegen::layout::{
    ITERATOR_ARRAY_OFFSET, ITERATOR_INDEX_OFFSET, ITERATOR_VERSION_OFFSET,
};
use crate::codegen::{invariant_violation, native_err, Compiler, Frame};
use crate::sema::hir::{
    AssignmentOperation, BindKind, BindingId, Body, Expr, PreviousOwner, ReturnPass, Stmt,
    StorageRelation,
};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, Block, BlockArg, InstBuilder, MemFlagsData};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;

/// `frame.terminated` 描述的是当前 Cranelift builder cursor 是否已经发出 terminator，
/// 不是源语言层面的“函数是否永远终止”。每次切到新可达 block 都必须重置它；遗漏
/// 会让后续语句被当成死代码，反向误清零则可能在已终止 block 后继续发指令。
pub(crate) fn emit_body<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    body: &Body,
    ret_block: Block,
) -> AliasResult<()> {
    match body {
        Body::Single(stmt) => emit_stmt(c, bcx, frame, stmt, ret_block)?,
        Body::Block(stmts) => {
            for s in stmts {
                ensure_current(bcx, frame);
                emit_stmt(c, bcx, frame, s, ret_block)?;
            }
        }
    }
    Ok(())
}

pub(super) fn emit_return_value<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    value: &Expr,
) -> AliasResult<ExprValue> {
    match value
        .info()
        .return_pass
        .as_deref()
        .unwrap_or_else(|| invariant_violation("return value 缺少 resolved ReturnPass"))
    {
        ReturnPass::Inline | ReturnPass::OwnedValue | ReturnPass::BorrowValue { .. } => {
            emit_expr(c, bcx, frame, value)
        }
        ReturnPass::OwnedTransfer { source } => {
            let (value, _) = emit_place_value(c, bcx, frame, source)?;
            if let crate::sema::hir::Place::Local { binding_id, .. } = source {
                super::destruction::mark_owner_moved(bcx, frame, *binding_id);
            }
            Ok(value)
        }
        ReturnPass::BorrowPlace { source, origin } => {
            let _ = origin;
            emit_place_addr(c, bcx, frame, source)
                .map(|(address, _)| ExprValue::scalar(address))
        }
    }
}

pub(super) fn emit_return_jump<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    value: ExprValue,
    ret_block: Block,
) -> AliasResult<()> {
    // The operand may already have returned through every match arm. Its placeholder carrier is
    // not a return value: do not normalize/store it or append a second terminator to that block.
    if frame.terminated {
        return Ok(());
    }
    emit_cleanup_to_depth(c, bcx, frame, 0)?;
    let ret_vty = frame
        .ret_vty
        .as_ref()
        .unwrap_or_else(|| invariant_violation("return 位于函数帧内"));
    match frame.return_passing {
        UserReturnPassing::Unit => invariant_violation("unit return 不应携带返回值"),
        UserReturnPassing::Direct(machine_type) => {
            let value = value.into_scalar("direct function return 必须是单 lane value");
            let value = norm_store(bcx, value, ret_vty);
            if bcx.func.dfg.value_type(value) != machine_type {
                invariant_violation("direct return machine type 与 canonical ABI 漂移")
            }
            bcx.ins().jump(ret_block, &[BlockArg::Value(value)]);
        }
        UserReturnPassing::ExplicitSRet => {
            let area = frame
                .sret
                .unwrap_or_else(|| invariant_violation("ExplicitSRet frame 缺少 return area"));
            value.store(bcx, area, 0, ret_vty);
            bcx.ins().jump(ret_block, &[]);
        }
    }
    frame.terminated = true;
    Ok(())
}

pub(crate) fn emit_stmt<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    s: &Stmt,
    ret_block: Block,
) -> AliasResult<()> {
    match s {
        Stmt::Binding(b) => {
            let binding_operation = b.operation.unwrap_or_else(|| {
                invariant_violation("binding 初始化缺少 resolved BindingOperation")
            });
            let relation = Some(binding_operation.storage_relation());
            let (cell, vty) = if b.kind == BindKind::Func {
                let Expr::FuncLit {
                    params,
                    body,
                    captures,
                    ..
                } = &b.value
                else {
                    return Err(native_err(b.span, "函数绑定必须由函数字面量初始化"));
                };
                let function_vty = c.vty(&b.ty);
                let VTy::Func { ret: ret_vty, .. } = &function_vty else {
                    invariant_violation("局部 func 绑定携带完整函数类型")
                };
                let ret_vty = (**ret_vty).clone();
                let v = emit_funclit_value_typed(
                    c,
                    bcx,
                    frame,
                    params,
                    body,
                    captures,
                    ret_vty.clone(),
                )?;
                let cell = emit_local_cell(
                    c,
                    bcx,
                    frame,
                    super::value::ExprValue::scalar(v),
                    function_vty,
                    b.binding_id,
                    relation,
                )?;
                (cell, c.vty(&b.ty))
            } else {
                let vty = c.vty(&b.ty);
                let v = emit_expr(c, bcx, frame, &b.value)?;
                let cell = emit_local_cell(c, bcx, frame, v, vty.clone(), b.binding_id, relation)?;
                (cell, vty)
            };
            let plan = b.destroy_plan.as_deref().unwrap_or_else(|| {
                invariant_violation("binding 缺少 resolved destruction plan")
            });
            register_local_cleanup(
                bcx,
                frame,
                b.binding_id,
                cell,
                vty,
                binding_operation.storage_relation(),
                plan.clone(),
            );
            Ok(())
        }
        Stmt::Assign {
            target,
            value,
            operation,
            previous_owner,
            destroy_plan,
        } => {
            // The frozen operation owns replacement-vs-rebind semantics. Runtime order still has
            // to evaluate the complete RHS before the target projection; reversing it would make
            // overlapping Place replacement observe the wrong source state.
            let operation = operation.unwrap_or_else(|| {
                invariant_violation("assignment 缺少 resolved ownership operation")
            });
            let previous_owner = previous_owner.unwrap_or_else(|| {
                invariant_violation("assignment 缺少程序点 previous-owner fact")
            });
            let destroy_plan = destroy_plan.as_deref().unwrap_or_else(|| {
                invariant_violation("assignment 缺少 resolved destruction plan")
            });
            let value = emit_expr(c, bcx, frame, value)?;
            // A match RHS may return from every arm. Its placeholder carrier is not a prepared
            // owner and the current block already has a terminator, so no target work may follow.
            if frame.terminated {
                return Ok(());
            }
            if operation == AssignmentOperation::RebindBorrowedAlias {
                let crate::sema::hir::Place::Local { binding_id, .. } = target else {
                    invariant_violation("borrowed alias rebind target 必须是 local Place")
                };
                let cell = super::cells::cell_addr(c, frame, *binding_id).unwrap_or_else(|| {
                    invariant_violation("borrowed rebind target 必须有 alias cell")
                });
                let cell = super::cells::materialize_cell_addr(bcx, frame, &cell);
                let cell_vty = crate::codegen::abi::binding_cell_vty(
                    &c.vty(target.ty()),
                    Some(StorageRelation::Borrowed),
                );
                value.store(bcx, cell, 0, &cell_vty);
                return Ok(());
            }
            // Materialize the target once after the complete RHS. Destruction can free the old
            // value graph, so recomputing a projecting target afterwards would be invalid.
            let (address, vty) = emit_place_addr(c, bcx, frame, target)?;
            let destroy_old = |c: &mut Compiler<M>, bcx: &mut FunctionBuilder| {
                let old = ExprValue::load(bcx, address, 0, &vty);
                emit_destroy_value(c, bcx, old, vty.clone(), destroy_plan)
            };
            match previous_owner {
                PreviousOwner::Live => destroy_old(c, bcx)?,
                PreviousOwner::MaybeMoved => {
                    let crate::sema::hir::Place::Local { binding_id, .. } = target else {
                        invariant_violation("MaybeMoved replacement 必须指向完整 local")
                    };
                    let destroy = bcx.create_block();
                    let commit = bcx.create_block();
                    let present = owner_presence(bcx, frame, *binding_id);
                    bcx.ins().brif(present, destroy, &[], commit, &[]);
                    bcx.seal_block(destroy);
                    bcx.switch_to_block(destroy);
                    destroy_old(c, bcx)?;
                    bcx.ins().jump(commit, &[]);
                    bcx.seal_block(commit);
                    bcx.switch_to_block(commit);
                }
                PreviousOwner::None | PreviousOwner::Unreachable => {}
            }
            value.store(bcx, address, 0, &vty);
            if let crate::sema::hir::Place::Local { binding_id, .. } = target {
                mark_owner_present(bcx, frame, *binding_id);
            }
            Ok(())
        }
        Stmt::Expr {
            expr,
            discard_destroy_plan,
        } => {
            let value = emit_expr(c, bcx, frame, expr)?;
            if frame.terminated {
                return Ok(());
            }
            if let Some(plan) = discard_destroy_plan.as_deref() {
                emit_destroy_value(c, bcx, value, c.vty(expr.ty()), plan)?;
            }
            Ok(())
        }
        Stmt::Return { value, .. } => {
            let expected = frame
                .ret_vty
                .clone()
                .unwrap_or_else(|| invariant_violation("return 位于函数帧内"));
            if frame.return_passing == UserReturnPassing::Unit {
                if expected != VTy::Unit {
                    invariant_violation("unit return passing 与返回 VTy 漂移")
                }
                emit_cleanup_to_depth(c, bcx, frame, 0)?;
                bcx.ins().jump(ret_block, &[]);
                frame.terminated = true;
                return Ok(());
            }
            let Some(value) = value else {
                return Err(native_err(
                    Span::default(),
                    "内部: 非 unit return 缺少返回值，sema 返回值不变式被破坏",
                ));
            };
            let v = emit_return_value(c, bcx, frame, value)?;
            emit_return_jump(c, bcx, frame, v, ret_block)?;
            Ok(())
        }
        Stmt::If {
            branches,
            else_body,
            ..
        } => emit_if(c, bcx, frame, branches, else_body.as_deref(), ret_block),
        Stmt::While { cond, body, .. } => emit_while(c, bcx, frame, cond, body, ret_block),
        Stmt::For {
            binding_id,
            ty,
            element_plan,
            element_destroy_plan,
            iterable,
            source_pass,
            body,
            span,
            ..
        } => {
            let elem_vty = c.vty(ty);
            emit_for(
                c,
                bcx,
                frame,
                (
                    iterable,
                    source_pass.as_ref().unwrap_or_else(|| invariant_violation("for source 缺少 resolved pass")),
                    *binding_id,
                    body,
                    &elem_vty,
                    element_plan,
                    element_destroy_plan.as_deref().unwrap_or_else(|| {
                        invariant_violation("for element 缺少 resolved destruction plan")
                    }),
                    *span,
                    ret_block,
                ),
            )
        }
        Stmt::Break => {
            let Some((break_b, _, cleanup_depth, _)) = frame.loop_targets.last().copied() else {
                return Err(native_err(Span::default(), "break 缺少循环目标"));
            };
            emit_cleanup_to_depth(c, bcx, frame, cleanup_depth)?;
            bcx.ins().jump(break_b, &[]);
            frame.terminated = true;
            Ok(())
        }
        Stmt::Continue => {
            let Some((_, continue_b, _, cleanup_depth)) = frame.loop_targets.last().copied() else {
                return Err(native_err(Span::default(), "continue 缺少循环目标"));
            };
            emit_cleanup_to_depth(c, bcx, frame, cleanup_depth)?;
            bcx.ins().jump(continue_b, &[]);
            frame.terminated = true;
            Ok(())
        }
    }
}

fn emit_scoped_stmts<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    body: &[Stmt],
    ret_block: Block,
) -> AliasResult<()> {
    let cleanup_depth = frame.cleanup_scopes.len();
    push_scope(frame);
    for s in body {
        ensure_current(bcx, frame);
        emit_stmt(c, bcx, frame, s, ret_block)?;
    }
    if !frame.terminated {
        emit_cleanup_to_depth(c, bcx, frame, cleanup_depth)?;
    }
    pop_scope(frame);
    Ok(())
}

fn emit_if<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    branches: &[(Expr, Vec<Stmt>)],
    else_body: Option<&[Stmt]>,
    ret_block: Block,
) -> AliasResult<()> {
    ensure_current(bcx, frame);
    let end_b = bcx.create_block();
    let mut has_fallthrough = false;

    for (idx, (cond, body)) in branches.iter().enumerate() {
        let then_b = bcx.create_block();
        let last = idx + 1 == branches.len();
        let false_b = if last {
            if else_body.is_some() {
                bcx.create_block()
            } else {
                end_b
            }
        } else {
            bcx.create_block()
        };
        let cv = emit_expr(c, bcx, frame, cond)?
            .into_scalar("if condition 必须是 scalar bool expression");
        bcx.ins().brif(cv, then_b, &[], false_b, &[]);
        frame.terminated = true;
        bcx.seal_block(then_b);
        if false_b != end_b {
            bcx.seal_block(false_b);
        }

        bcx.switch_to_block(then_b);
        frame.terminated = false;
        emit_scoped_stmts(c, bcx, frame, body, ret_block)?;
        if !frame.terminated {
            bcx.ins().jump(end_b, &[]);
            frame.terminated = true;
            has_fallthrough = true;
        }

        if last {
            if let Some(else_stmts) = else_body {
                bcx.switch_to_block(false_b);
                frame.terminated = false;
                emit_scoped_stmts(c, bcx, frame, else_stmts, ret_block)?;
                if !frame.terminated {
                    bcx.ins().jump(end_b, &[]);
                    frame.terminated = true;
                    has_fallthrough = true;
                }
            } else {
                has_fallthrough = true;
            }
        } else {
            bcx.switch_to_block(false_b);
            frame.terminated = false;
        }
    }

    bcx.seal_block(end_b);
    if has_fallthrough {
        bcx.switch_to_block(end_b);
        frame.terminated = false;
    } else {
        frame.terminated = true;
    }
    Ok(())
}

fn emit_while<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    cond: &Expr,
    body: &[Stmt],
    ret_block: Block,
) -> AliasResult<()> {
    ensure_current(bcx, frame);
    let header = bcx.create_block();
    let body_b = bcx.create_block();
    let end_b = bcx.create_block();
    bcx.ins().jump(header, &[]);
    frame.terminated = true;

    bcx.switch_to_block(header);
    frame.terminated = false;
    let cv = emit_expr(c, bcx, frame, cond)?
        .into_scalar("while condition 必须是 scalar bool expression");
    bcx.ins().brif(cv, body_b, &[], end_b, &[]);
    frame.terminated = true;
    bcx.seal_block(body_b);

    bcx.switch_to_block(body_b);
    frame.terminated = false;
    let cleanup_depth = frame.cleanup_scopes.len();
    frame
        .loop_targets
        .push((end_b, header, cleanup_depth, cleanup_depth));
    emit_scoped_stmts(c, bcx, frame, body, ret_block)?;
    frame.loop_targets.pop();
    if !frame.terminated {
        bcx.ins().jump(header, &[]);
        frame.terminated = true;
    }

    bcx.seal_block(header);
    bcx.seal_block(end_b);
    bcx.switch_to_block(end_b);
    frame.terminated = false;
    Ok(())
}

fn emit_for<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    input: (
        &Expr,
        &crate::sema::hir::ArgumentPass,
        BindingId,
        &[Stmt],
        &VTy,
        &crate::sema::hir::DeepClonePlan,
        &crate::sema::hir::DestroyPlan,
        Span,
        Block,
    ),
) -> AliasResult<()> {
    let (
        iterable,
        source_pass,
        binding_id,
        body,
        elem_vty,
        element_plan,
        element_destroy_plan,
        span,
        ret_block,
    ) = input;
    ensure_current(bcx, frame);
    let loop_cleanup_depth = frame.cleanup_scopes.len();
    push_scope(frame);
    let source_vty = c.vty(iterable.ty());
    let (source, source_destroy_plan) = match source_pass {
        crate::sema::hir::ArgumentPass::ReadBorrow { source, .. } => {
            (emit_place_value(c, bcx, frame, source)?.0, None)
        }
        crate::sema::hir::ArgumentPass::BorrowTemporary {
            kind: crate::sema::hir::BorrowKind::Read,
            destroy_plan,
        } => (
            emit_expr(c, bcx, frame, iterable)?,
            Some(destroy_plan.as_ref().clone()),
        ),
        _ => invariant_violation("for source 必须是 resolved ReadBorrow"),
    };
    let source = source.into_scalar("for iterable 尚未支持 multi-lane source");
    if let Some(plan) = source_destroy_plan {
        let cell = emit_temporary_cell(
            c,
            bcx,
            ExprValue::scalar(source),
            &source_vty,
        )?;
        register_temporary_cleanup(bcx, frame, cell, source_vty.clone(), Some(plan));
    }
    let iter = match &source_vty {
        VTy::Array(element) => {
            let iter = make_iterator(c, bcx, source)?;
            let iterator_vty = VTy::Iterator(element.clone());
            let cell = emit_temporary_cell(
                c,
                bcx,
                ExprValue::scalar(iter),
                &iterator_vty,
            )?;
            register_temporary_cleanup(bcx, frame, cell, iterator_vty, None);
            iter
        }
        VTy::Iterator(_) => source,
        _ => invariant_violation("for 主语为 array/iterator (sema 已校验)"),
    };

    let header = bcx.create_block();
    let valid_b = bcx.create_block();
    let invalid_b = bcx.create_block();
    let body_b = bcx.create_block();
    let end_b = bcx.create_block();
    bcx.ins().jump(header, &[]);
    frame.terminated = true;

    bcx.switch_to_block(header);
    frame.terminated = false;
    let array = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), iter, ITERATOR_ARRAY_OFFSET);
    let expected = bcx.ins().load(
        types::I64,
        MemFlagsData::new(),
        iter,
        ITERATOR_VERSION_OFFSET,
    );
    let actual = array_version(bcx, array);
    let invalid = bcx.ins().icmp(IntCC::NotEqual, actual, expected);
    bcx.ins().brif(invalid, invalid_b, &[], valid_b, &[]);
    frame.terminated = true;
    bcx.seal_block(invalid_b);
    bcx.seal_block(valid_b);

    bcx.switch_to_block(invalid_b);
    emit_iterator_abort(c, bcx, span)?;
    frame.terminated = true;

    bcx.switch_to_block(valid_b);
    frame.terminated = false;
    let cursor = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), iter, ITERATOR_INDEX_OFFSET);
    let raw = array_raw(bcx, array);
    let len = array_len(bcx, raw);
    let more = bcx.ins().icmp(IntCC::UnsignedLessThan, cursor, len);
    bcx.ins().brif(more, body_b, &[], end_b, &[]);
    frame.terminated = true;
    bcx.seal_block(body_b);

    bcx.switch_to_block(body_b);
    frame.terminated = false;
    let raw = array_raw(bcx, array);
    let addr = array_element_addr(bcx, raw, cursor);
    let elem = super::value::ExprValue::load(bcx, addr, 0, elem_vty)
        .into_scalar("for element deep clone 尚未支持 multi-lane source");
    let elem = emit_deep_clone_value(c, bcx, elem, elem_vty, element_plan)?;
    let next = bcx.ins().iadd_imm_s(cursor, 1);
    // cursor 在进入用户 body 前推进；因此 continue 跳回 header 时不会重复当前元素。
    // 若把该 store 移到 body 之后，任何 continue 都会形成同一元素的无限循环。
    bcx.ins()
        .store(MemFlagsData::new(), next, iter, ITERATOR_INDEX_OFFSET);

    let cleanup_depth = frame.cleanup_scopes.len();
    push_scope(frame);
    let cell = emit_local_cell(
        c,
        bcx,
        frame,
        super::value::ExprValue::scalar(elem),
        elem_vty.clone(),
        binding_id,
        Some(StorageRelation::Owning),
    )?;
    register_local_cleanup(
        bcx,
        frame,
        binding_id,
        cell,
        elem_vty.clone(),
        StorageRelation::Owning,
        element_destroy_plan.clone(),
    );
    frame
        .loop_targets
        .push((end_b, header, cleanup_depth, cleanup_depth));
    for s in body {
        ensure_current(bcx, frame);
        emit_stmt(c, bcx, frame, s, ret_block)?;
    }
    frame.loop_targets.pop();
    if !frame.terminated {
        emit_cleanup_to_depth(c, bcx, frame, cleanup_depth)?;
    }
    pop_scope(frame);
    if !frame.terminated {
        bcx.ins().jump(header, &[]);
        frame.terminated = true;
    }

    bcx.seal_block(header);
    bcx.seal_block(end_b);
    bcx.switch_to_block(end_b);
    frame.terminated = false;
    emit_cleanup_to_depth(c, bcx, frame, loop_cleanup_depth)?;
    pop_scope(frame);
    Ok(())
}
