use super::arrays::{
    array_element_addr, array_len, array_raw, array_version, emit_iterator_abort, make_iterator,
};
use super::cells::{coerce_ret, emit_local_cell, ensure_current, pop_scope, push_scope};
use super::expr::emit_expr;
use super::places::emit_place_write;
use crate::codegen::abi::{cl_type, norm_load, VTy};
use crate::codegen::funcgen::emit_funclit_value_typed;
use crate::codegen::layout::{
    ITERATOR_ARRAY_OFFSET, ITERATOR_INDEX_OFFSET, ITERATOR_VERSION_OFFSET,
};
use crate::codegen::{invariant_violation, native_err, Compiler, Frame};
use crate::sema::hir::{BindKind, BindingId, Body, Expr, Stmt, StorageRelation, ValueCategory};
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

pub(crate) fn emit_stmt<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    s: &Stmt,
    ret_block: Block,
) -> AliasResult<()> {
    match s {
        Stmt::Binding(b) => {
            if b.kind == BindKind::Func {
                let Expr::FuncLit {
                    params,
                    body,
                    captures,
                    ..
                } = &b.value
                else {
                    return Err(native_err(b.span, "函数绑定必须由函数字面量初始化"));
                };
                let VTy::Func(param_vtys, ret_vty) = c.vty(&b.ty) else {
                    invariant_violation("局部 func 绑定携带完整函数类型")
                };
                let ret_vty = *ret_vty;
                let v = emit_funclit_value_typed(
                    c,
                    bcx,
                    frame,
                    params,
                    body,
                    captures,
                    ret_vty.clone(),
                )?;
                emit_local_cell(
                    c,
                    bcx,
                    frame,
                    v,
                    VTy::Func(param_vtys, Box::new(ret_vty)),
                    b.binding_id,
                    b.relation,
                )?;
            } else {
                let vty = c.vty(&b.ty);
                let v = emit_expr(c, bcx, frame, &b.value)?;
                emit_local_cell(c, bcx, frame, v, vty, b.binding_id, b.relation)?;
            }
            Ok(())
        }
        Stmt::Assign { target, value } => {
            // Replacement 的 ownership 语义由 resolved HIR 决定；运行时顺序必须保持 RHS
            // （包括 ReadPlace clone）先完整求值，再求值 target 的 Field/Index projection。
            // 反转顺序会让重叠 Place 的 replacement 观察到错误 source 状态。
            let borrowed_rebind = matches!(
                (target, value.value_category()),
                (
                    crate::sema::hir::Place::Local { binding_id, .. },
                    Some(ValueCategory::BorrowedValue)
                ) if crate::codegen::bound_relation(c, frame, *binding_id)
                    == Some(StorageRelation::Borrowed)
            );
            let value = emit_expr(c, bcx, frame, value)?;
            if borrowed_rebind {
                let crate::sema::hir::Place::Local { binding_id, .. } = target else {
                    unreachable!()
                };
                let cell = super::cells::cell_addr(c, frame, *binding_id).unwrap_or_else(|| {
                    invariant_violation("borrowed rebind target 必须有 alias cell")
                });
                let cell = super::cells::materialize_cell_addr(bcx, frame, &cell);
                bcx.ins().store(MemFlagsData::new(), value, cell, 0);
                return Ok(());
            }
            emit_place_write(c, bcx, frame, target, value)
        }
        Stmt::Expr { expr, .. } => {
            emit_expr(c, bcx, frame, expr)?;
            Ok(())
        }
        Stmt::Return { value, .. } => {
            let expected = frame
                .ret_vty
                .clone()
                .unwrap_or_else(|| invariant_violation("return 位于函数帧内"));
            if expected == VTy::Unit {
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
            let v = emit_expr(c, bcx, frame, value)?;
            let v = coerce_ret(bcx, frame, v);
            bcx.ins().jump(ret_block, &[BlockArg::Value(v)]);
            frame.terminated = true;
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
            iterable,
            body,
            span,
            ..
        } => {
            let elem_vty = c.vty(ty);
            emit_for(
                c,
                bcx,
                frame,
                (iterable, *binding_id, body, &elem_vty, *span, ret_block),
            )
        }
        Stmt::Break => {
            let Some((break_b, _)) = frame.loop_targets.last().copied() else {
                return Err(native_err(Span::default(), "break 缺少循环目标"));
            };
            bcx.ins().jump(break_b, &[]);
            frame.terminated = true;
            Ok(())
        }
        Stmt::Continue => {
            let Some((_, continue_b)) = frame.loop_targets.last().copied() else {
                return Err(native_err(Span::default(), "continue 缺少循环目标"));
            };
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
    push_scope(frame);
    for s in body {
        ensure_current(bcx, frame);
        emit_stmt(c, bcx, frame, s, ret_block)?;
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
        let cv = emit_expr(c, bcx, frame, cond)?;
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
    let cv = emit_expr(c, bcx, frame, cond)?;
    bcx.ins().brif(cv, body_b, &[], end_b, &[]);
    frame.terminated = true;
    bcx.seal_block(body_b);

    bcx.switch_to_block(body_b);
    frame.terminated = false;
    frame.loop_targets.push((end_b, header));
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
    input: (&Expr, BindingId, &[Stmt], &VTy, Span, Block),
) -> AliasResult<()> {
    let (iterable, binding_id, body, elem_vty, span, ret_block) = input;
    ensure_current(bcx, frame);
    let source_vty = c.vty(iterable.ty());
    let source = emit_expr(c, bcx, frame, iterable)?;
    let iter = match source_vty {
        VTy::Array(_) => make_iterator(c, bcx, source)?,
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
    let raw_elem = bcx
        .ins()
        .load(cl_type(elem_vty), MemFlagsData::new(), addr, 0);
    let elem = norm_load(bcx, raw_elem, elem_vty);
    let next = bcx.ins().iadd_imm_s(cursor, 1);
    // cursor 在进入用户 body 前推进；因此 continue 跳回 header 时不会重复当前元素。
    // 若把该 store 移到 body 之后，任何 continue 都会形成同一元素的无限循环。
    bcx.ins()
        .store(MemFlagsData::new(), next, iter, ITERATOR_INDEX_OFFSET);

    push_scope(frame);
    emit_local_cell(
        c,
        bcx,
        frame,
        elem,
        elem_vty.clone(),
        binding_id,
        Some(StorageRelation::Owning),
    )?;
    frame.loop_targets.push((end_b, header));
    for s in body {
        ensure_current(bcx, frame);
        emit_stmt(c, bcx, frame, s, ret_block)?;
    }
    frame.loop_targets.pop();
    pop_scope(frame);
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
