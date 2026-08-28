// ---------------------------------------------------------------------------
// emit — 表达式/语句发射域: 单元格访问、控制流、迭代、调用与内建。
// ---------------------------------------------------------------------------
use super::*;
use super::{Frame, VTy};
use crate::codegen::{invariant_violation, native_err, Compiler, Slot};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{Block, BlockArg, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::{FunctionBuilder, Variable};
use cranelift_module::{Linkage, Module};

// ---------------------------------------------------------------------------
// 单元格与全局槽位访问
// ---------------------------------------------------------------------------

pub(crate) enum CellAddr {
    Reg(Variable),
    EnvLoad(usize),
    GlobalOff(usize),
}

pub(crate) fn cell_addr<M: Module>(c: &Compiler<M>, frame: &Frame, id: BindingId) -> Option<CellAddr> {
    for scope in frame.scopes.iter().rev() {
        if let Some(s) = scope.get(&id) {
            return Some(match s {
                Slot::Local(v) => CellAddr::Reg(*v),
                Slot::Global(off) => CellAddr::GlobalOff(*off),
            });
        }
    }
    if let Some(idx) = frame.caps.get(&id) {
        return Some(CellAddr::EnvLoad(*idx));
    }
    if frame.init_ctx {
        return None;
    }
    c.globals_final
        .get(&id)
        .map(|(off, _)| CellAddr::GlobalOff(*off))
}

pub(crate) fn read_cell(
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    addr: &CellAddr,
    vty: &VTy,
) -> Value {
    let t = cl_type(vty);
    let raw = match addr {
        CellAddr::Reg(v) => {
            let cp = bcx.use_var(*v);
            bcx.ins().load(t, MemFlagsData::new(), cp, 0)
        }
        CellAddr::EnvLoad(i) => {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            let cell = bcx.ins().load(
                types::I64,
                MemFlagsData::new(),
                base,
                ((*i as i64) * 8) as i32,
            );
            bcx.ins().load(t, MemFlagsData::new(), cell, 0)
        }
        CellAddr::GlobalOff(off) => {
            let base = bcx.use_var(frame.globals);
            bcx.ins().load(t, MemFlagsData::new(), base, *off as i32)
        }
    };
    norm_load(bcx, raw, vty)
}

pub(crate) fn write_cell(
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    addr: &CellAddr,
    v: Value,
    vty: &VTy,
) {
    let sv = norm_store(bcx, v, vty);
    match addr {
        CellAddr::Reg(v) => {
            let cp = bcx.use_var(*v);
            bcx.ins().store(MemFlagsData::new(), sv, cp, 0);
        }
        CellAddr::EnvLoad(i) => {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            let cell = bcx.ins().load(
                types::I64,
                MemFlagsData::new(),
                base,
                ((*i as i64) * 8) as i32,
            );
            bcx.ins().store(MemFlagsData::new(), sv, cell, 0);
        }
        CellAddr::GlobalOff(off) => {
            let base = bcx.use_var(frame.globals);
            bcx.ins().store(MemFlagsData::new(), sv, base, *off as i32);
        }
    }
}

pub(crate) fn emit_local_cell<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    word: Value,
    vty: VTy,
    id: BindingId,
) -> AliasResult<Variable> {
    let (sz, _) = size_align(&vty);
    let szw = bcx.ins().iconst(types::I64, sz as i64);
    let cell = c.call_rt(bcx, "alias.cell.new", &[szw])?;
    let sv = norm_store(bcx, word, &vty);
    bcx.ins().store(MemFlagsData::new(), sv, cell, 0);
    let var = bcx.declare_var(types::I64);
    bcx.def_var(var, cell);
    frame
        .scopes
        .last_mut()
        .unwrap_or_else(|| invariant_violation("作用域栈非空"))
        .insert(id, Slot::Local(var));
    frame
        .locals_vty
        .last_mut()
        .unwrap_or_else(|| invariant_violation("作用域栈非空"))
        .insert(id, vty);
    Ok(var)
}

pub(crate) fn first_result(bcx: &FunctionBuilder, inst: cranelift_codegen::ir::Inst) -> Value {
    match bcx.inst_results(inst) {
        [v] => *v,
        _ => invariant_violation("单返回值签名"),
    }
}

pub(crate) fn ensure_current(bcx: &mut FunctionBuilder, frame: &mut Frame) {
    if frame.terminated {
        let dead = bcx.create_block();
        bcx.switch_to_block(dead);
        bcx.seal_block(dead);
        frame.terminated = false;
    }
}

fn coerce_ret(bcx: &mut FunctionBuilder, frame: &Frame, v: Value) -> Value {
    match &frame.ret_vty {
        Some(vty) => norm_store(bcx, v, vty),
        None => v,
    }
}

fn push_scope(frame: &mut Frame) {
    frame.scopes.push(HashMap::new());
    frame.locals_vty.push(HashMap::new());
}

fn pop_scope(frame: &mut Frame) {
    frame.scopes.pop();
    frame.locals_vty.pop();
}

// ---------------------------------------------------------------------------
// Array / iterator 表示
// ---------------------------------------------------------------------------

fn array_raw(bcx: &mut FunctionBuilder, array: Value) -> Value {
    bcx.ins().load(types::I64, MemFlagsData::new(), array, 0)
}

fn array_version(bcx: &mut FunctionBuilder, array: Value) -> Value {
    bcx.ins().load(types::I64, MemFlagsData::new(), array, 8)
}

fn bump_array_version(bcx: &mut FunctionBuilder, array: Value) {
    let old = array_version(bcx, array);
    let next = bcx.ins().iadd_imm_s(old, 1);
    bcx.ins().store(MemFlagsData::new(), next, array, 8);
}

fn wrap_array<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    raw: Value,
) -> AliasResult<Value> {
    let n2 = bcx.ins().iconst(types::I32, 2);
    let wrapper = c.call_rt(bcx, "alias.env.new", &[n2])?;
    let zero = bcx.ins().iconst(types::I64, 0);
    bcx.ins().store(MemFlagsData::new(), raw, wrapper, 0);
    bcx.ins().store(MemFlagsData::new(), zero, wrapper, 8);
    Ok(wrapper)
}

fn make_iterator<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    array: Value,
) -> AliasResult<Value> {
    let n3 = bcx.ins().iconst(types::I32, 3);
    let iter = c.call_rt(bcx, "alias.env.new", &[n3])?;
    let zero = bcx.ins().iconst(types::I64, 0);
    let version = array_version(bcx, array);
    bcx.ins().store(MemFlagsData::new(), array, iter, 0);
    bcx.ins().store(MemFlagsData::new(), zero, iter, 8);
    bcx.ins().store(MemFlagsData::new(), version, iter, 16);
    Ok(iter)
}

fn emit_iterator_abort<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    span: Span,
) -> AliasResult<()> {
    let text = format!(
        "错误 @ {}:{} — 遍历期间集合结构已修改\n",
        span.line, span.col
    );
    let s = str_literal_handle(c, bcx, &text)?;
    let ptr = bcx.ins().load(types::I64, MemFlagsData::new(), s, 0);
    let len64 = bcx.ins().load(types::I64, MemFlagsData::new(), s, 8);
    let len = bcx.ins().ireduce(types::I32, len64);

    let get = c.import_external("GetStdHandle", &[types::I32], Some(c.ptr_ty))?;
    let get_ref = c.module.declare_func_in_func(get, &mut bcx.func);
    let stderr_id = bcx.ins().iconst(types::I32, -12);
    let call = bcx.ins().call(get_ref, &[stderr_id]);
    let stderr = first_result(bcx, call);

    let written = bcx.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
        cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
        8,
        3,
    ));
    let written_addr = bcx.ins().stack_addr(c.ptr_ty, written, 0);
    let null = bcx.ins().iconst(c.ptr_ty, 0);
    let write = c.import_external(
        "WriteFile",
        &[c.ptr_ty, c.ptr_ty, types::I32, c.ptr_ty, c.ptr_ty],
        Some(types::I32),
    )?;
    let write_ref = c.module.declare_func_in_func(write, &mut bcx.func);
    bcx.ins()
        .call(write_ref, &[stderr, ptr, len, written_addr, null]);

    let exit = c.import_external("ExitProcess", &[types::I32], None)?;
    let exit_ref = c.module.declare_func_in_func(exit, &mut bcx.func);
    let one = bcx.ins().iconst(types::I32, 1);
    bcx.ins().call(exit_ref, &[one]);
    bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);
    Ok(())
}

// ---------------------------------------------------------------------------
// 体与控制流发射
// ---------------------------------------------------------------------------

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
                )?;
            } else {
                let vty = c.vty(&b.ty);
                let v = emit_expr_expected(c, bcx, frame, &b.value, &vty)?;
                emit_local_cell(c, bcx, frame, v, vty, b.binding_id)?;
            }
            Ok(())
        }
        Stmt::FieldAssign {
            recv,
            field_index,
            value,
            ..
        } => {
            let fvty = field_vty(c, recv, *field_index)?;
            let v = emit_expr_expected(c, bcx, frame, value, &fvty)?;
            let p = emit_expr(c, bcx, frame, recv)?;
            let off = field_offset(c, recv, *field_index)?;
            let sv = norm_store(bcx, v, &fvty);
            bcx.ins().store(MemFlagsData::new(), sv, p, off);
            Ok(())
        }
        Stmt::Assign {
            target,
            target_id,
            value,
            ..
        } => {
            let tvty = bound_vty(c, frame, *target_id);
            let v = emit_expr_expected(c, bcx, frame, value, &tvty)?;
            match cell_addr(c, frame, *target_id) {
                Some(addr) => {
                    write_cell(bcx, frame, &addr, v, &tvty);
                    Ok(())
                }
                None => Err(native_err(
                    Span::default(),
                    format!("内部: 赋值目标 '{target}' 的 BindingId 无存储"),
                )),
            }
        }
        Stmt::ExprStmt { expr, .. } => {
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
            let v = match value {
                Some(e) => emit_expr_expected(c, bcx, frame, e, &expected)?,
                None => match cl_type(&expected) {
                    types::F32 => bcx.ins().f32const(0.0),
                    types::F64 => bcx.ins().f64const(0.0),
                    ty => bcx.ins().iconst(ty, 0),
                },
            };
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
                iterable,
                *binding_id,
                body,
                &elem_vty,
                *span,
                ret_block,
            )
        }
        Stmt::Break { .. } => {
            let Some((break_b, _)) = frame.loop_targets.last().copied() else {
                return Err(native_err(Span::default(), "break 缺少循环目标"));
            };
            bcx.ins().jump(break_b, &[]);
            frame.terminated = true;
            Ok(())
        }
        Stmt::Continue { .. } => {
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
    iterable: &Expr,
    binding_id: BindingId,
    body: &[Stmt],
    elem_vty: &VTy,
    span: Span,
    ret_block: Block,
) -> AliasResult<()> {
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
    let array = bcx.ins().load(types::I64, MemFlagsData::new(), iter, 0);
    let expected = bcx.ins().load(types::I64, MemFlagsData::new(), iter, 16);
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
    let cursor = bcx.ins().load(types::I64, MemFlagsData::new(), iter, 8);
    let raw = array_raw(bcx, array);
    let len = bcx.ins().load(types::I64, MemFlagsData::new(), raw, 8);
    let more = bcx.ins().icmp(IntCC::UnsignedLessThan, cursor, len);
    bcx.ins().brif(more, body_b, &[], end_b, &[]);
    frame.terminated = true;
    bcx.seal_block(body_b);

    bcx.switch_to_block(body_b);
    frame.terminated = false;
    let raw = array_raw(bcx, array);
    let data = bcx.ins().load(types::I64, MemFlagsData::new(), raw, 0);
    let off = bcx.ins().imul_imm_s(cursor, 8);
    let addr = bcx.ins().iadd(data, off);
    let raw_elem = bcx
        .ins()
        .load(cl_type(elem_vty), MemFlagsData::new(), addr, 0);
    let elem = norm_load(bcx, raw_elem, elem_vty);
    let next = bcx.ins().iadd_imm_s(cursor, 1);
    bcx.ins().store(MemFlagsData::new(), next, iter, 8);

    push_scope(frame);
    emit_local_cell(c, bcx, frame, elem, elem_vty.clone(), binding_id)?;
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

// ---------------------------------------------------------------------------
// 表达式发射
// ---------------------------------------------------------------------------

pub(crate) fn emit_expr<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    e: &Expr,
) -> AliasResult<Value> {
    match e {
        Expr::Int(n, ..) => Ok(bcx.ins().iconst(types::I64, *n as i64)),
        Expr::Float(v, ..) => Ok(bcx.ins().f64const(*v)),
        Expr::Bool(b, ..) => Ok(bcx.ins().iconst(types::I64, *b as i64)),
        Expr::Str(parts, ..) => emit_str(c, bcx, frame, parts),
        Expr::Ident(name, id, span, _) => {
            let id = id.unwrap_or_else(|| {
                panic!("内部代码生成不变式被破坏: 可求值标识符 '{name}' 缺少 BindingId")
            });
            match cell_addr(c, frame, id) {
                Some(addr) => {
                    let vty = bound_vty(c, frame, id);
                    Ok(read_cell(bcx, frame, &addr, &vty))
                }
                None => Err(native_err(*span, format!("内部: BindingId {:?} 无存储", id))),
            }
        }
        Expr::This(span, _) => {
            let fid = frame
                .this_fid
                .ok_or_else(|| native_err(*span, "this 只能出现在 func 体内"))?;
            let fref = c.module.declare_func_in_func(fid, &mut bcx.func);
            let code = bcx.ins().func_addr(c.ptr_ty, fref);
            let env = frame
                .env
                .map(|var| bcx.use_var(var))
                .unwrap_or_else(|| bcx.ins().iconst(types::I64, 0));
            c.call_rt(bcx, "alias.closure.new", &[code, env])
        }
        Expr::Cast { expr, span, .. } => {
            let dst = c.vty(e.ty());
            let src = c.vty(expr.ty());
            let value = emit_expr(c, bcx, frame, expr)?;
            emit_convert(c, bcx, frame, *span, value, &src, &dst)
        }
        Expr::Neg { expr, span, .. } => {
            let v = emit_expr(c, bcx, frame, expr)?;
            let t = c.vty(expr.ty());
            match t {
                VTy::I(w) => {
                    let wt = cl_type(&VTy::I(w));
                    let red = narrow(bcx, v, w.bits());
                    let min = bcx.ins().iconst(
                        wt,
                        match w.bits() {
                            8 => i8::MIN as i64,
                            16 => i16::MIN as i64,
                            32 => i32::MIN as i64,
                            _ => i64::MIN,
                        },
                    );
                    let overflow = bcx.ins().icmp(IntCC::Equal, red, min);
                    emit_abort_branch(c, bcx, frame, overflow, "alias.abort_overflow", *span)?;
                    let n = bcx.ins().ineg(red);
                    Ok(widen_signed(bcx, n, wt))
                }
                VTy::F(_) => Ok(bcx.ins().fneg(v)),
                _ => invariant_violation("取负操作数为有符号整数或浮点 (sema 已校验)"),
            }
        }
        Expr::Not { expr, .. } => {
            let v = emit_expr(c, bcx, frame, expr)?;
            Ok(emit_bool_not(bcx, v))
        }
        Expr::BitNot { expr, .. } => {
            let vty = c.vty(expr.ty());
            let v = emit_expr(c, bcx, frame, expr)?;
            emit_bit_not_typed(bcx, v, &vty)
        }
        Expr::Binary {
            op: BinOp::And,
            lhs,
            rhs,
            ..
        } => emit_short_circuit(c, bcx, frame, false, lhs, rhs),
        Expr::Binary {
            op: BinOp::Or,
            lhs,
            rhs,
            ..
        } => emit_short_circuit(c, bcx, frame, true, lhs, rhs),
        Expr::Binary {
            op, lhs, rhs, span, ..
        } => {
            let l = emit_expr(c, bcx, frame, lhs)?;
            let r = emit_expr(c, bcx, frame, rhs)?;
            emit_binary(c, bcx, frame, *op, lhs, l, r, *span)
        }
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            let expected = c.vty(e.ty());
            emit_ternary_typed(c, bcx, frame, cond, then_expr, else_expr, &expected)
        }
        Expr::Call {
            callee, args, span, ..
        } => emit_call(
            c,
            bcx,
            frame,
            callee,
            args,
            e.call_target()
                .unwrap_or_else(|| invariant_violation("调用表达式必须携带已解析目标")),
            *span,
        ),
        Expr::MethodCall {
            recv, args, span, ..
        } => {
            let Some(CallTarget::Method(target)) = e.call_target() else {
                invariant_violation("方法调用必须携带已解析目标")
            };
            emit_method_call(c, bcx, frame, recv, args, target, *span)
        }
        Expr::Field {
            recv,
            field_index,
            ..
        } => {
            let p = emit_expr(c, bcx, frame, recv)?;
            let fvty = field_vty(c, recv, *field_index)?;
            let off = field_offset(c, recv, *field_index)?;
            let raw = bcx.ins().load(cl_type(&fvty), MemFlagsData::new(), p, off);
            Ok(norm_load(bcx, raw, &fvty))
        }
        Expr::Index {
            recv, idx, span, ..
        } => {
            let array = emit_expr(c, bcx, frame, recv)?;
            let idxw = emit_expr(c, bcx, frame, idx)?;
            let elem_vty = match c.vty(recv.ty()) {
                VTy::Array(inner) => (*inner).clone(),
                _ => invariant_violation("下标主语为 array (sema 已校验)"),
            };
            let raw_array = array_raw(bcx, array);
            let idx32 = bcx.ins().ireduce(types::I32, idxw);
            let len64 = bcx
                .ins()
                .load(types::I64, MemFlagsData::new(), raw_array, 8);
            let len32 = bcx.ins().ireduce(types::I32, len64);
            emit_index_guard(c, bcx, frame, idx32, len32, *span)?;
            let dp = bcx
                .ins()
                .load(types::I64, MemFlagsData::new(), raw_array, 0);
            let idx64 = bcx.ins().sextend(types::I64, idx32);
            let off = bcx.ins().imul_imm_s(idx64, 8);
            let addr = bcx.ins().iadd(dp, off);
            let raw = bcx
                .ins()
                .load(cl_type(&elem_vty), MemFlagsData::new(), addr, 0);
            Ok(norm_load(bcx, raw, &elem_vty))
        }
        Expr::ArrayLit { elems, .. } => {
            let VTy::Array(elem_vty) = c.vty(e.ty()) else {
                invariant_violation("数组字面量携带 array 类型")
            };
            emit_array_lit_typed(c, bcx, frame, elems, &elem_vty)
        }
        Expr::FuncLit {
            params,
            body,
            captures,
            ..
        } => emit_funclit_value(c, bcx, frame, params, body, captures, e.ty()),
        Expr::Match { subject, arms, .. } => {
            let result_vty = c.vty(e.ty());
            emit_match_typed(c, bcx, frame, subject, arms, &result_vty)
        }
        Expr::Propagate { expr, .. } => {
            let subj = emit_expr(c, bcx, frame, expr)?;
            let tag = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 0);
            let is_err = bcx.ins().icmp_imm_s(IntCC::Equal, tag, 1);
            let err_b = bcx.create_block();
            let ok_b = bcx.create_block();
            bcx.ins().brif(is_err, err_b, &[], ok_b, &[]);
            bcx.seal_block(err_b);
            bcx.seal_block(ok_b);

            bcx.switch_to_block(err_b);
            let rb = frame
                .ret_block
                .unwrap_or_else(|| invariant_violation("? 仅在函数体内可达 (sema 已校验)"));
            bcx.ins().jump(rb, &[BlockArg::Value(subj)]);
            frame.terminated = true;

            bcx.switch_to_block(ok_b);
            frame.terminated = false;
            let pvty = match c.vty(expr.ty()) {
                VTy::Result(t, _) => *t,
                _ => invariant_violation("? 操作数携带 result 类型"),
            };
            let raw = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 8);
            Ok(restore_word(bcx, raw, &pvty))
        }
    }
}

fn emit_bool_not(bcx: &mut FunctionBuilder, v: Value) -> Value {
    let b = bcx.ins().icmp_imm_s(IntCC::Equal, v, 0);
    bcx.ins().uextend(types::I64, b)
}

fn emit_bit_not_typed(bcx: &mut FunctionBuilder, v: Value, vty: &VTy) -> AliasResult<Value> {
    match vty {
        VTy::I(w) => {
            let wt = cl_type(vty);
            let red = narrow(bcx, v, w.bits());
            let n = bcx.ins().bnot(red);
            Ok(widen_signed(bcx, n, wt))
        }
        VTy::U(w) => {
            let wt = cl_type(vty);
            let red = narrow(bcx, v, w.bits());
            let n = bcx.ins().bnot(red);
            Ok(widen_unsigned(bcx, n, wt))
        }
        _ => invariant_violation("位非操作数为整数 (sema 已校验)"),
    }
}

fn emit_short_circuit<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    is_or: bool,
    lhs: &Expr,
    rhs: &Expr,
) -> AliasResult<Value> {
    let l = emit_expr(c, bcx, frame, lhs)?;
    let rhs_b = bcx.create_block();
    let join_b = bcx.create_block();
    let out = bcx.append_block_param(join_b, types::I64);
    let short = bcx.ins().iconst(types::I64, if is_or { 1 } else { 0 });
    if is_or {
        bcx.ins()
            .brif(l, join_b, &[BlockArg::Value(short)], rhs_b, &[]);
    } else {
        bcx.ins()
            .brif(l, rhs_b, &[], join_b, &[BlockArg::Value(short)]);
    }
    bcx.seal_block(rhs_b);
    bcx.switch_to_block(rhs_b);
    let r = emit_expr(c, bcx, frame, rhs)?;
    bcx.ins().jump(join_b, &[BlockArg::Value(r)]);
    bcx.seal_block(join_b);
    bcx.switch_to_block(join_b);
    frame.terminated = false;
    Ok(out)
}

fn emit_ternary_typed<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    cond: &Expr,
    then_expr: &Expr,
    else_expr: &Expr,
    expected: &VTy,
) -> AliasResult<Value> {
    let cv = emit_expr(c, bcx, frame, cond)?;
    let then_b = bcx.create_block();
    let else_b = bcx.create_block();
    let join_b = bcx.create_block();
    let out = bcx.append_block_param(join_b, cl_type(expected));
    bcx.ins().brif(cv, then_b, &[], else_b, &[]);
    bcx.seal_block(then_b);
    bcx.seal_block(else_b);

    bcx.switch_to_block(then_b);
    frame.terminated = false;
    let a = emit_expr_expected(c, bcx, frame, then_expr, expected)?;
    if !frame.terminated {
        let a = norm_store(bcx, a, expected);
        bcx.ins().jump(join_b, &[BlockArg::Value(a)]);
    }

    bcx.switch_to_block(else_b);
    frame.terminated = false;
    let b = emit_expr_expected(c, bcx, frame, else_expr, expected)?;
    if !frame.terminated {
        let b = norm_store(bcx, b, expected);
        bcx.ins().jump(join_b, &[BlockArg::Value(b)]);
    }

    bcx.seal_block(join_b);
    bcx.switch_to_block(join_b);
    frame.terminated = false;
    Ok(norm_load(bcx, out, expected))
}

fn emit_match_typed<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    subject: &Expr,
    arms: &[MatchArm],
    result_vty: &VTy,
) -> AliasResult<Value> {
    let subject_vty = c.vty(subject.ty());
    let subj = emit_expr(c, bcx, frame, subject)?;
    let join_b = bcx.create_block();
    let jv = if *result_vty == VTy::Unit {
        None
    } else {
        Some(bcx.append_block_param(join_b, types::I64))
    };
    let mut any_join = false;

    for (idx, arm) in arms.iter().enumerate() {
        let arm_b = bcx.create_block();
        let last = idx + 1 == arms.len();
        let next_b = if last { None } else { Some(bcx.create_block()) };

        if let Some(next_b) = next_b {
            let matched = emit_pattern_test(c, bcx, &arm.pattern, subj)?;
            bcx.ins().brif(matched, arm_b, &[], next_b, &[]);
            frame.terminated = true;
            bcx.seal_block(arm_b);
            bcx.seal_block(next_b);
        } else {
            bcx.ins().jump(arm_b, &[]);
            frame.terminated = true;
            bcx.seal_block(arm_b);
        }

        bcx.switch_to_block(arm_b);
        frame.terminated = false;
        any_join |= emit_match_arm(c, bcx, frame, arm, &subject_vty, result_vty, subj, join_b)?;

        if let Some(next_b) = next_b {
            bcx.switch_to_block(next_b);
            frame.terminated = false;
        }
    }

    if any_join {
        bcx.seal_block(join_b);
        bcx.switch_to_block(join_b);
        frame.terminated = false;
        Ok(match jv {
            Some(value) => restore_word(bcx, value, result_vty),
            None => bcx.ins().iconst(types::I64, 0),
        })
    } else {
        ensure_current(bcx, frame);
        Ok(bcx.ins().iconst(types::I64, 0))
    }
}

fn emit_pattern_test<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    pattern: &Pattern,
    subj: Value,
) -> AliasResult<Value> {
    Ok(match pattern {
        Pattern::Wildcard { .. } | Pattern::Binding { .. } => bcx.ins().iconst(types::I64, 1),
        Pattern::Int { value, .. } => {
            let rhs = bcx.ins().iconst(types::I64, *value as i64);
            bcx.ins().icmp(IntCC::Equal, subj, rhs)
        }
        Pattern::Bool { value, .. } => bcx.ins().icmp_imm_s(IntCC::Equal, subj, *value as i64),
        Pattern::Str { value, .. } => {
            let rhs = str_literal_handle(c, bcx, value)?;
            let ord = call_str_cmp(c, bcx, subj, rhs)?;
            bcx.ins().icmp_imm_s(IntCC::Equal, ord, 0)
        }
        Pattern::Constructor { ctor, .. } => {
            let tag = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 0);
            let want = match ctor {
                CtorKind::Ok => 0,
                CtorKind::Err => 1,
            };
            bcx.ins().icmp_imm_s(IntCC::Equal, tag, want)
        }
    })
}

pub(crate) fn emit_expr_expected<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    e: &Expr,
    expected: &VTy,
) -> AliasResult<Value> {
    match (e, expected) {
        (Expr::Int(value, ..), VTy::I(_) | VTy::U(_)) => {
            Ok(bcx.ins().iconst(types::I64, *value as i64))
        }
        (Expr::Neg { expr, .. }, VTy::I(_)) if matches!(expr.as_ref(), Expr::Int(..)) => {
            let Expr::Int(magnitude, ..) = expr.as_ref() else {
                unreachable!()
            };
            Ok(bcx
                .ins()
                .iconst(types::I64, 0u64.wrapping_sub(*magnitude) as i64))
        }
        (
            Expr::Call {
                args, span, info, ..
            },
            _,
        ) if is_contextual_conversion(info) => {
            let [arg] = args.as_slice() else {
                invariant_violation("from/try_from 元数 (sema 已校验)")
            };
            let source = c.vty(arg.value.ty());
            if conversion_exists_vty(&source, expected) {
                let value = emit_expr(c, bcx, frame, &arg.value)?;
                emit_convert(c, bcx, frame, *span, value, &source, expected)
            } else {
                emit_expr(c, bcx, frame, &arg.value)
            }
        }
        (
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
                ..
            },
            _,
        ) => emit_ternary_typed(c, bcx, frame, cond, then_expr, else_expr, expected),
        (Expr::Match { subject, arms, .. }, _) => {
            emit_match_typed(c, bcx, frame, subject, arms, expected)
        }
        (
            Expr::Binary {
                op, lhs, rhs, span, ..
            },
            _,
        ) if binary_vty_flows_expected(*op, expected) => {
            let l = emit_expr_expected(c, bcx, frame, lhs, expected)?;
            let r = emit_expr_expected(c, bcx, frame, rhs, expected)?;
            emit_binary_values(c, bcx, frame, *op, expected, l, r, *span)
        }
        (Expr::BitNot { expr, .. }, VTy::I(_) | VTy::U(_)) => {
            let v = emit_expr_expected(c, bcx, frame, expr, expected)?;
            emit_bit_not_typed(bcx, v, expected)
        }
        (Expr::ArrayLit { elems, .. }, VTy::Array(elem)) => {
            emit_array_lit_typed(c, bcx, frame, elems, elem)
        }
        (Expr::Call { args, info, .. }, VTy::Result(ok, err))
            if matches!(info.call_target, Some(CallTarget::ResultConstructor(_))) =>
        {
            let Some(CallTarget::ResultConstructor(kind)) = info.call_target.as_ref() else {
                unreachable!()
            };
            let payload = match kind {
                CtorKind::Ok => (**ok).clone(),
                CtorKind::Err => (**err).clone(),
            };
            emit_result_ctor_typed(c, bcx, frame, *kind, args, &payload)
        }
        (Expr::Float(..), VTy::F(FloatW::F32)) => {
            let v = emit_expr(c, bcx, frame, e)?;
            Ok(norm_store(bcx, v, expected))
        }
        _ => emit_expr(c, bcx, frame, e),
    }
}

fn is_contextual_conversion(info: &ExprInfo) -> bool {
    matches!(
        info.call_target,
        Some(CallTarget::Builtin(
            BuiltinCall::From | BuiltinCall::TryFrom
        ))
    )
}

fn conversion_exists_vty(source: &VTy, target: &VTy) -> bool {
    (source.is_numeric() && target.is_numeric())
        || (matches!(target, VTy::Str) && !matches!(source, VTy::Unknown | VTy::Unit))
}

fn binary_vty_flows_expected(op: BinOp, expected: &VTy) -> bool {
    use BinOp::*;
    match op {
        Add | Sub | Mul | Div => expected.is_numeric(),
        Rem | Shl | Shr | BitAnd | BitXor | BitOr => matches!(expected, VTy::I(_) | VTy::U(_)),
        Lt | Le | Gt | Ge | EqEq | NotEq | And | Or => false,
    }
}

fn emit_array_lit_typed<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    elems: &[Expr],
    elem_vty: &VTy,
) -> AliasResult<Value> {
    let n = elems.len() as i64;
    let cap = bcx.ins().iconst(types::I32, n);
    let eszw = bcx.ins().iconst(types::I32, 8);
    let raw = c.call_rt(bcx, "alias.arr.new", &[cap, eszw])?;
    for (i, el) in elems.iter().enumerate() {
        let v = emit_expr_expected(c, bcx, frame, el, elem_vty)?;
        let dp = bcx.ins().load(types::I64, MemFlagsData::new(), raw, 0);
        let addr = bcx.ins().iadd_imm_s(dp, (i as i64) * 8);
        let sv = storage_word(bcx, v, elem_vty);
        store_elem(bcx, sv, addr, elem_vty);
    }
    let lenw = bcx.ins().iconst(types::I64, n);
    bcx.ins().store(MemFlagsData::new(), lenw, raw, 8);
    wrap_array(c, bcx, raw)
}

fn emit_binary<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    op: BinOp,
    lhs: &Expr,
    l: Value,
    r: Value,
    span: Span,
) -> AliasResult<Value> {
    let lt = c.vty(lhs.ty());
    emit_binary_values(c, bcx, frame, op, &lt, l, r, span)
}

fn emit_binary_values<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    op: BinOp,
    lt: &VTy,
    l: Value,
    r: Value,
    span: Span,
) -> AliasResult<Value> {
    use BinOp::*;
    match op {
        Add | Sub | Mul | Div | Rem => match lt {
            VTy::F(_) => match op {
                Add => Ok(bcx.ins().fadd(l, r)),
                Sub => Ok(bcx.ins().fsub(l, r)),
                Mul => Ok(bcx.ins().fmul(l, r)),
                Div => Ok(bcx.ins().fdiv(l, r)),
                Rem => invariant_violation("浮点余数已被 sema 拒绝"),
                _ => unreachable!(),
            },
            VTy::I(w) => {
                let wt = cl_type(&VTy::I(*w));
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Add | Sub | Mul => {
                        emit_checked_int_binary(c, bcx, frame, op, li, ri, true, span)?
                    }
                    Div => emit_divrem_guard(c, bcx, frame, li, ri, true, w.bits(), span, false)?,
                    Rem => emit_divrem_guard(c, bcx, frame, li, ri, true, w.bits(), span, true)?,
                    _ => unreachable!(),
                };
                Ok(widen_signed(bcx, v, wt))
            }
            VTy::U(w) => {
                let wt = cl_type(&VTy::U(*w));
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Add | Sub | Mul => {
                        emit_checked_int_binary(c, bcx, frame, op, li, ri, false, span)?
                    }
                    Div => emit_divrem_guard(c, bcx, frame, li, ri, false, w.bits(), span, false)?,
                    Rem => emit_divrem_guard(c, bcx, frame, li, ri, false, w.bits(), span, true)?,
                    _ => unreachable!(),
                };
                Ok(widen_unsigned(bcx, v, wt))
            }
            _ => invariant_violation("算术操作数为数值族 (sema 已校验)"),
        },
        BitAnd | BitXor | BitOr => match lt {
            VTy::I(w) => {
                let wt = cl_type(lt);
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    BitAnd => bcx.ins().band(li, ri),
                    BitXor => bcx.ins().bxor(li, ri),
                    BitOr => bcx.ins().bor(li, ri),
                    _ => unreachable!(),
                };
                Ok(widen_signed(bcx, v, wt))
            }
            VTy::U(w) => {
                let wt = cl_type(lt);
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    BitAnd => bcx.ins().band(li, ri),
                    BitXor => bcx.ins().bxor(li, ri),
                    BitOr => bcx.ins().bor(li, ri),
                    _ => unreachable!(),
                };
                Ok(widen_unsigned(bcx, v, wt))
            }
            _ => invariant_violation("位运算操作数为整数 (sema 已校验)"),
        },
        Shl | Shr => match lt {
            VTy::I(w) => {
                let wt = cl_type(lt);
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Shl => emit_checked_shl(c, bcx, frame, li, ri, true, w.bits(), span)?,
                    Shr => bcx.ins().sshr(li, ri),
                    _ => unreachable!(),
                };
                Ok(widen_signed(bcx, v, wt))
            }
            VTy::U(w) => {
                let wt = cl_type(lt);
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Shl => emit_checked_shl(c, bcx, frame, li, ri, false, w.bits(), span)?,
                    Shr => bcx.ins().ushr(li, ri),
                    _ => unreachable!(),
                };
                Ok(widen_unsigned(bcx, v, wt))
            }
            _ => invariant_violation("移位操作数为整数 (sema 已校验)"),
        },
        Lt | Le | Gt | Ge | EqEq | NotEq => {
            use cranelift_codegen::ir::condcodes::FloatCC;
            let b = match lt {
                VTy::Str => {
                    let ord = call_str_cmp(c, bcx, l, r)?;
                    bcx.ins().icmp_imm_s(int_cc(op, true), ord, 0)
                }
                VTy::F(_) => {
                    let cc = match op {
                        Lt => FloatCC::LessThan,
                        Le => FloatCC::LessThanOrEqual,
                        Gt => FloatCC::GreaterThan,
                        Ge => FloatCC::GreaterThanOrEqual,
                        EqEq => FloatCC::Equal,
                        _ => FloatCC::NotEqual,
                    };
                    bcx.ins().fcmp(cc, l, r)
                }
                VTy::I(w) => {
                    let li = narrow(bcx, l, w.bits());
                    let ri = narrow(bcx, r, w.bits());
                    bcx.ins().icmp(int_cc(op, true), li, ri)
                }
                VTy::U(w) => {
                    let li = narrow(bcx, l, w.bits());
                    let ri = narrow(bcx, r, w.bits());
                    bcx.ins().icmp(int_cc(op, false), li, ri)
                }
                _ => bcx.ins().icmp(int_cc(op, true), l, r),
            };
            Ok(bcx.ins().uextend(types::I64, b))
        }
        And | Or => invariant_violation("短路逻辑运算由 emit_short_circuit 发射"),
    }
}

fn emit_checked_int_binary<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    op: BinOp,
    l: Value,
    r: Value,
    signed: bool,
    span: Span,
) -> AliasResult<Value> {
    let (result, overflow) = match (signed, op) {
        (true, BinOp::Add) => bcx.ins().sadd_overflow(l, r),
        (true, BinOp::Sub) => bcx.ins().ssub_overflow(l, r),
        (true, BinOp::Mul) => bcx.ins().smul_overflow(l, r),
        (false, BinOp::Add) => bcx.ins().uadd_overflow(l, r),
        (false, BinOp::Sub) => bcx.ins().usub_overflow(l, r),
        (false, BinOp::Mul) => bcx.ins().umul_overflow(l, r),
        _ => invariant_violation("checked 整数算术仅用于加减乘"),
    };
    emit_abort_branch(c, bcx, frame, overflow, "alias.abort_overflow", span)?;
    Ok(result)
}

fn emit_checked_shl<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    value: Value,
    shift: Value,
    signed: bool,
    bits: u32,
    span: Span,
) -> AliasResult<Value> {
    let count_bad = bcx
        .ins()
        .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, shift, bits as i64);
    let result = bcx.ins().ishl(value, shift);
    let restored = if signed {
        bcx.ins().sshr(result, shift)
    } else {
        bcx.ins().ushr(result, shift)
    };
    let lost = bcx.ins().icmp(IntCC::NotEqual, restored, value);
    let overflow = bcx.ins().bor(count_bad, lost);
    emit_abort_branch(c, bcx, frame, overflow, "alias.abort_overflow", span)?;
    Ok(result)
}

fn int_cc(op: BinOp, signed: bool) -> IntCC {
    match (op, signed) {
        (BinOp::Lt, true) => IntCC::SignedLessThan,
        (BinOp::Le, true) => IntCC::SignedLessThanOrEqual,
        (BinOp::Gt, true) => IntCC::SignedGreaterThan,
        (BinOp::Ge, true) => IntCC::SignedGreaterThanOrEqual,
        (BinOp::Lt, false) => IntCC::UnsignedLessThan,
        (BinOp::Le, false) => IntCC::UnsignedLessThanOrEqual,
        (BinOp::Gt, false) => IntCC::UnsignedGreaterThan,
        (BinOp::Ge, false) => IntCC::UnsignedGreaterThanOrEqual,
        (BinOp::EqEq, _) => IntCC::Equal,
        (BinOp::NotEq, _) => IntCC::NotEqual,
        _ => invariant_violation("比较谓词仅用于比较运算符"),
    }
}

fn narrow(bcx: &mut FunctionBuilder, v: Value, bits: u32) -> Value {
    let ty = ir_type_bits(bits);
    if ty == types::I64 {
        v
    } else {
        bcx.ins().ireduce(ty, v)
    }
}

fn widen_signed(bcx: &mut FunctionBuilder, v: Value, to: cranelift_codegen::ir::Type) -> Value {
    if to == types::I64 {
        v
    } else {
        bcx.ins().sextend(types::I64, v)
    }
}

fn widen_unsigned(bcx: &mut FunctionBuilder, v: Value, to: cranelift_codegen::ir::Type) -> Value {
    if to == types::I64 {
        v
    } else {
        bcx.ins().uextend(types::I64, v)
    }
}

pub(crate) fn emit_match_arm<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    arm: &MatchArm,
    subject_vty: &VTy,
    result_vty: &VTy,
    subj: Value,
    join_b: Block,
) -> AliasResult<bool> {
    push_scope(frame);

    match (&arm.pattern, subject_vty, arm.binding_id) {
        (Pattern::Binding { .. }, _, Some(binding_id)) => {
            emit_local_cell(c, bcx, frame, subj, subject_vty.clone(), binding_id)?;
        }
        (
            Pattern::Constructor {
                ctor,
                binding: Some(_),
                ..
            },
            VTy::Result(ok, err),
            Some(binding_id),
        ) => {
            let bind_vty = match ctor {
                CtorKind::Ok => (**ok).clone(),
                CtorKind::Err => (**err).clone(),
            };
            let raw = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 8);
            let payload = restore_word(bcx, raw, &bind_vty);
            emit_local_cell(c, bcx, frame, payload, bind_vty, binding_id)?;
        }
        (Pattern::Binding { .. }, _, None)
        | (Pattern::Constructor { binding: Some(_), .. }, _, None) => {
            invariant_violation("Pattern 绑定必须携带 BindingId")
        }
        _ => {}
    }

    let joined = match &arm.body {
        ArmBody::Value(e) => {
            let v = emit_expr_expected(c, bcx, frame, e, result_vty)?;
            if *result_vty == VTy::Unit {
                bcx.ins().jump(join_b, &[]);
            } else {
                let word = storage_word(bcx, v, result_vty);
                bcx.ins().jump(join_b, &[BlockArg::Value(word)]);
            }
            frame.terminated = true;
            true
        }
        ArmBody::Ret(e) => {
            let expected = frame
                .ret_vty
                .clone()
                .unwrap_or_else(|| invariant_violation("return match 臂位于函数帧内"));
            let v = emit_expr_expected(c, bcx, frame, e, &expected)?;
            let v = coerce_ret(bcx, frame, v);
            let rb = frame
                .ret_block
                .unwrap_or_else(|| invariant_violation("never 臂仅在函数体内可达 (sema 已校验)"));
            bcx.ins().jump(rb, &[BlockArg::Value(v)]);
            frame.terminated = true;
            false
        }
        ArmBody::Block(stmts) => {
            let rb = frame.ret_block.unwrap_or_else(|| {
                invariant_violation("臂内 return 仅在函数体内可达 (sema 已校验)")
            });
            let n = stmts.len();
            let mut tail: Option<Value> = None;
            for (i, s) in stmts.iter().enumerate() {
                ensure_current(bcx, frame);
                if i + 1 == n {
                    if let Stmt::ExprStmt { expr, .. } = s {
                        tail = Some(emit_expr_expected(c, bcx, frame, expr, result_vty)?);
                        continue;
                    }
                }
                emit_stmt(c, bcx, frame, s, rb)?;
            }
            if frame.terminated {
                false
            } else {
                if *result_vty == VTy::Unit {
                    bcx.ins().jump(join_b, &[]);
                } else {
                    let v = tail.unwrap_or_else(|| bcx.ins().iconst(types::I64, 0));
                    let word = storage_word(bcx, v, result_vty);
                    bcx.ins().jump(join_b, &[BlockArg::Value(word)]);
                }
                frame.terminated = true;
                true
            }
        }
    };
    pop_scope(frame);
    Ok(joined)
}

pub(crate) fn emit_str<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    parts: &[StrPart],
) -> AliasResult<Value> {
    let z8 = bcx.ins().iconst(types::I64, 0);
    let z4 = bcx.ins().iconst(types::I32, 0);
    let empty = c.call_rt(bcx, "alias.str.new", &[z8, z4])?;
    let mut acc = empty;
    for p in parts {
        let piece = match p {
            StrPart::Lit(s) => str_literal_handle(c, bcx, s)?,
            StrPart::Hole(h) => {
                if matches!(h.as_ref(), Expr::Call { info, .. } if is_contextual_conversion(info)) {
                    emit_expr_expected(c, bcx, frame, h, &VTy::Str)?
                } else {
                    let w = emit_expr(c, bcx, frame, h)?;
                    display_word(c, bcx, h, w)?
                }
            }
        };
        acc = c.call_rt(bcx, "alias.str.concat", &[acc, piece])?;
    }
    Ok(acc)
}

pub(crate) fn str_literal_handle<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    s: &str,
) -> AliasResult<Value> {
    let data_id = match c.str_data.get(s) {
        Some(id) => *id,
        None => {
            let dname = format!("str{}", c.str_data.len());
            let id = c
                .module
                .declare_data(&dname, Linkage::Local, false, false)
                .map_err(|e| native_err(Span::default(), format!("内部: 数据段声明失败 {e}")))?;
            let mut desc = cranelift_module::DataDescription::new();
            desc.define(s.as_bytes().to_vec().into());
            c.module
                .define_data(id, &desc)
                .map_err(|e| native_err(Span::default(), format!("内部: 数据段定义失败 {e}")))?;
            c.str_data.insert(s.to_string(), id);
            id
        }
    };
    let gv = c.module.declare_data_in_func(data_id, &mut bcx.func);
    let addr = bcx.ins().symbol_value(c.ptr_ty, gv);
    let len = bcx.ins().iconst(types::I32, s.len() as i64);
    c.call_rt(bcx, "alias.str.new", &[addr, len])
}

pub(crate) fn display_word<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    e: &Expr,
    w: Value,
) -> AliasResult<Value> {
    let vty = c.vty(e.ty());
    display_typed(c, bcx, &vty, w, e.span())
}

fn display_typed<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    vty: &VTy,
    w: Value,
    span: Span,
) -> AliasResult<Value> {
    match vty {
        VTy::I(IntW::W64) => c.call_rt(bcx, "alias.display.i64", &[w]),
        VTy::I(_) => {
            let t = bcx.ins().ireduce(types::I32, w);
            c.call_rt(bcx, "alias.display.int", &[t])
        }
        VTy::U(UIntW::U8) | VTy::U(UIntW::U16) => {
            let t = bcx.ins().ireduce(types::I32, w);
            c.call_rt(bcx, "alias.display.int", &[t])
        }
        VTy::U(_) => c.call_rt(bcx, "alias.display.u64", &[w]),
        VTy::F(FloatW::F32) => c.call_rt(bcx, "alias.display.f32", &[w]),
        VTy::F(FloatW::F64) => c.call_rt(bcx, "alias.display.f64", &[w]),
        VTy::Bool => {
            let t = bcx.ins().ireduce(types::I32, w);
            c.call_rt(bcx, "alias.display.bool", &[t])
        }
        VTy::Str => c.call_rt(bcx, "alias.display.str", &[w]),
        VTy::Unit => Err(native_err(span, "内部: unit 无返回值表达式进入 display")),
        VTy::Func(..) | VTy::FuncPoly => c.call_rt(bcx, "alias.display.func", &[]),
        VTy::Struct(_) => c.call_rt(bcx, "alias.display.struct", &[]),
        VTy::Array(_) => c.call_rt(bcx, "alias.display.array", &[]),
        VTy::Iterator(_) => str_literal_handle(c, bcx, "<iterator>"),
        VTy::Result(..) => {
            let tag = bcx.ins().load(types::I64, MemFlagsData::new(), w, 0);
            let t = bcx.ins().ireduce(types::I32, tag);
            c.call_rt(bcx, "alias.display.result", &[t])
        }
        VTy::Unknown => Err(native_err(span, "原生后端无法发射未确定类型的显示值")),
    }
}

pub(crate) fn call_str_cmp<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    l: Value,
    r: Value,
) -> AliasResult<Value> {
    c.call_rt(bcx, "alias.str.cmp", &[l, r])
}

// ---------------------------------------------------------------------------
// 调用 / 内建 / 闭包创建
// ---------------------------------------------------------------------------

pub(crate) fn emit_call<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    callee: &Expr,
    args: &[CallArg],
    target: &CallTarget,
    span: Span,
) -> AliasResult<Value> {
    match target {
        CallTarget::Builtin(BuiltinCall::Increase) => {
            emit_incdec(c, bcx, frame, "increase", args, span)
        }
        CallTarget::Builtin(BuiltinCall::Decrease) => {
            emit_incdec(c, bcx, frame, "decrease", args, span)
        }
        CallTarget::Builtin(BuiltinCall::Print) => emit_print(c, bcx, frame, "print", args, span),
        CallTarget::Builtin(BuiltinCall::Println) => {
            emit_print(c, bcx, frame, "println", args, span)
        }
        CallTarget::Builtin(BuiltinCall::Typeof) => {
            let [arg] = args else {
                invariant_violation("typeof 元数 (sema 已校验)")
            };
            let tn = c.vty(arg.value.ty()).display_name();
            str_literal_handle(c, bcx, &tn)
        }
        CallTarget::Builtin(BuiltinCall::From | BuiltinCall::TryFrom) => {
            invariant_violation("上下文转换必须由带目标类型的发射入口处理")
        }
        CallTarget::StructConstructor(name) => emit_construct(c, bcx, frame, name, args),
        CallTarget::ResultConstructor(kind) => emit_result_ctor(c, bcx, frame, *kind, args),
        CallTarget::FunctionValue => {
            let callee_vty = c.vty(callee.ty());
            let VTy::Func(param_vtys, ret_vty) = callee_vty else {
                invariant_violation("函数值调用必须携带完整函数签名")
            };
            let clo = emit_expr(c, bcx, frame, callee)?;
            call_closure(c, bcx, frame, clo, &param_vtys, &ret_vty, args)
        }
        CallTarget::Method(_) => invariant_violation("普通调用不能携带方法目标"),
    }
}

fn call_closure<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    clo: Value,
    param_vtys: &[VTy],
    ret_vty: &VTy,
    args: &[CallArg],
) -> AliasResult<Value> {
    let mut words: Vec<Value> = Vec::with_capacity(args.len() + 2);
    for (a, pt) in args.iter().zip(param_vtys) {
        let v = emit_expr_expected(c, bcx, frame, &a.value, pt)?;
        words.push(norm_store(bcx, v, pt));
    }
    let code = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 0);
    let env = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 8);
    words.insert(0, env);
    words.insert(0, bcx.use_var(frame.globals));
    let sig = user_signature(c.cc, param_vtys, ret_vty);
    let sig_ref = bcx.func.import_signature(sig);
    let inst = bcx.ins().call_indirect(sig_ref, code, &words);
    if *ret_vty == VTy::Unit {
        return Ok(bcx.ins().iconst(types::I64, 0));
    }
    let raw = first_result(bcx, inst);
    Ok(norm_load(bcx, raw, ret_vty))
}

fn jump_zero_return(bcx: &mut FunctionBuilder, frame: &Frame) {
    let rb = frame
        .ret_block
        .unwrap_or_else(|| invariant_violation("运行时错误传播需要返回块"));
    let params = bcx.block_params(rb);
    if params.is_empty() {
        bcx.ins().jump(rb, &[]);
        return;
    }
    let [param] = params else {
        invariant_violation("返回块至多一个参数")
    };
    let ty = bcx.func.dfg.value_type(*param);
    let zero = if ty == types::F32 {
        bcx.ins().f32const(0.0)
    } else if ty == types::F64 {
        bcx.ins().f64const(0.0)
    } else {
        bcx.ins().iconst(ty, 0)
    };
    bcx.ins().jump(rb, &[BlockArg::Value(zero)]);
}

fn emit_convert<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    span: Span,
    v: Value,
    src: &VTy,
    dst: &VTy,
) -> AliasResult<Value> {
    match dst {
        VTy::Str => display_typed(c, bcx, src, v, span),
        VTy::F(w) => {
            let t = cl_type(dst);
            let base = match src {
                VTy::I(_) => bcx.ins().fcvt_from_sint(t, v),
                VTy::U(_) => bcx.ins().fcvt_from_uint(t, v),
                VTy::F(_) => match w {
                    FloatW::F32 => match bcx.func.dfg.value_type(v) {
                        types::F64 => bcx.ins().fdemote(types::F32, v),
                        _ => v,
                    },
                    FloatW::F64 => match bcx.func.dfg.value_type(v) {
                        types::F32 => bcx.ins().fpromote(types::F64, v),
                        _ => v,
                    },
                },
                _ => invariant_violation("转换源为数值族 (sema 已校验)"),
            };
            Ok(base)
        }
        VTy::I(w) => {
            let bits = w.bits();
            let wt = ir_type_bits(bits);
            emit_convert_to_int(c, bcx, frame, span, v, src, true, bits, wt)
        }
        VTy::U(w) => {
            let bits = w.bits();
            let wt = ir_type_bits(bits);
            emit_convert_to_int(c, bcx, frame, span, v, src, false, bits, wt)
        }
        _ => invariant_violation("转换目标为数值族或 string"),
    }
}

fn emit_convert_to_int<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    span: Span,
    v: Value,
    src: &VTy,
    signed: bool,
    bits: u32,
    wt: cranelift_codegen::ir::Type,
) -> AliasResult<Value> {
    use cranelift_codegen::ir::condcodes::FloatCC;
    if matches!(src, VTy::F(_)) {
        let f64v = match bcx.func.dfg.value_type(v) {
            types::F32 => bcx.ins().fpromote(types::F64, v),
            _ => v,
        };
        let (lo, hi) = if signed {
            (-(2f64).powi(bits as i32 - 1), (2f64).powi(bits as i32 - 1))
        } else {
            (0.0f64, (2f64).powi(bits as i32))
        };
        let nan = bcx.ins().fcmp(FloatCC::NotEqual, f64v, f64v);
        let lo_c = bcx.ins().f64const(lo);
        let hi_c = bcx.ins().f64const(hi);
        let below = bcx.ins().fcmp(FloatCC::LessThan, f64v, lo_c);
        let above = bcx.ins().fcmp(FloatCC::GreaterThanOrEqual, f64v, hi_c);
        let bad_lo = bcx.ins().bor(nan, below);
        let bad = bcx.ins().bor(bad_lo, above);
        emit_abort_branch(c, bcx, frame, bad, "alias.abort_conv", span)?;
        let sat = if signed {
            bcx.ins().fcvt_to_sint(types::I64, f64v)
        } else {
            bcx.ins().fcvt_to_uint(types::I64, f64v)
        };
        let red = narrow(bcx, sat, bits);
        Ok(if signed {
            widen_signed(bcx, red, wt)
        } else {
            widen_unsigned(bcx, red, wt)
        })
    } else {
        let no = bcx.ins().iconst(types::I8, 0);
        let bad = match src {
            VTy::I(source_w) if signed => {
                if bits >= source_w.bits() {
                    no
                } else {
                    let min = -(1i128 << (bits - 1)) as i64;
                    let max = ((1u128 << (bits - 1)) - 1) as i64;
                    let below = bcx.ins().icmp_imm_s(IntCC::SignedLessThan, v, min);
                    let above = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, v, max);
                    bcx.ins().bor(below, above)
                }
            }
            VTy::I(_) => {
                let negative = bcx.ins().icmp_imm_s(IntCC::SignedLessThan, v, 0);
                if bits == 64 {
                    negative
                } else {
                    let max = ((1u128 << bits) - 1) as i64;
                    let above = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, v, max);
                    bcx.ins().bor(negative, above)
                }
            }
            VTy::U(_) if signed => {
                let max = ((1u128 << (bits - 1)) - 1) as u64 as i64;
                bcx.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, v, max)
            }
            VTy::U(source_w) => {
                if bits >= source_w.bits() {
                    no
                } else {
                    let max = ((1u128 << bits) - 1) as u64 as i64;
                    bcx.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, v, max)
                }
            }
            _ => invariant_violation("整数转换源为整数 (sema 已校验)"),
        };
        emit_abort_branch(c, bcx, frame, bad, "alias.abort_conv", span)?;
        let red = narrow(bcx, v, bits);
        Ok(if signed {
            widen_signed(bcx, red, wt)
        } else {
            widen_unsigned(bcx, red, wt)
        })
    }
}

fn emit_abort_branch<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    trap: Value,
    sym: &str,
    span: Span,
) -> AliasResult<()> {
    let span_id = new_span_id(c, span);
    let abort_b = bcx.create_block();
    let ok_b = bcx.create_block();
    bcx.ins().brif(trap, abort_b, &[], ok_b, &[]);
    bcx.seal_block(abort_b);
    bcx.seal_block(ok_b);

    bcx.switch_to_block(abort_b);
    let aid = bcx.ins().iconst(types::I32, span_id as i64);
    c.call_rt(bcx, sym, &[aid])?;
    jump_zero_return(bcx, frame);

    bcx.switch_to_block(ok_b);
    Ok(())
}

pub(crate) fn emit_construct<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
) -> AliasResult<Value> {
    let layout = c.struct_layouts[name].clone();
    let bytes = bcx.ins().iconst(types::I64, layout.size as i64);
    let ptr = c.call_rt(bcx, "alias.cell.new", &[bytes])?;
    for field in &layout.fields {
        let expr = args
            .iter()
            .find(|a| a.label.as_deref() == Some(field.name.as_str()))
            .map(|a| &a.value)
            .or(field.default.as_ref())
            .unwrap_or_else(|| invariant_violation("构造字段全覆盖 (sema 已校验)"));
        let v = emit_expr_expected(c, bcx, frame, expr, &field.vty)?;
        let sv = norm_store(bcx, v, &field.vty);
        bcx.ins().store(MemFlagsData::new(), sv, ptr, field.offset);
    }
    Ok(ptr)
}

pub(crate) fn emit_result_ctor<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    kind: CtorKind,
    args: &[CallArg],
) -> AliasResult<Value> {
    let [arg] = args else {
        invariant_violation("result 构造元数 (sema 已校验)")
    };
    let pvty = c.vty(arg.value.ty());
    emit_result_ctor_typed(c, bcx, frame, kind, args, &pvty)
}

fn emit_result_ctor_typed<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    kind: CtorKind,
    args: &[CallArg],
    pvty: &VTy,
) -> AliasResult<Value> {
    let [arg] = args else {
        invariant_violation("result 构造元数 (sema 已校验)")
    };
    let payload = emit_expr_expected(c, bcx, frame, &arg.value, pvty)?;
    let pw = storage_word(bcx, payload, pvty);
    let n2 = bcx.ins().iconst(types::I32, 2);
    let blk = c.call_rt(bcx, "alias.env.new", &[n2])?;
    let tag = match kind {
        CtorKind::Ok => 0i64,
        CtorKind::Err => 1i64,
    };
    let tagw = bcx.ins().iconst(types::I64, tag);
    bcx.ins().store(MemFlagsData::new(), tagw, blk, 0);
    bcx.ins().store(MemFlagsData::new(), pw, blk, 8);
    Ok(blk)
}

pub(crate) fn emit_method_call<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    recv: &Expr,
    args: &[CallArg],
    target: &MethodTarget,
    span: Span,
) -> AliasResult<Value> {
    let rv = emit_expr(c, bcx, frame, recv)?;
    let svt = c.vty(recv.ty());

    match target {
        MethodTarget::Numeric(op) => {
            let [arg] = args else {
                invariant_violation("算术扩展函数元数 (sema 已校验)")
            };
            let r = emit_expr_expected(c, bcx, frame, &arg.value, &svt)?;
            emit_binary_values(c, bcx, frame, *op, &svt, rv, r, span)
        }
        MethodTarget::BoolNot => {
            if !args.is_empty() {
                invariant_violation("not 扩展函数元数 (sema 已校验)");
            }
            Ok(emit_bool_not(bcx, rv))
        }
        MethodTarget::StringLen => {
            let t = c.call_rt(bcx, "alias.str.len", &[rv])?;
            Ok(bcx.ins().sextend(types::I64, t))
        }
        MethodTarget::StringUpper => c.call_rt(bcx, "alias.str.upper", &[rv]),
        MethodTarget::StringLower => c.call_rt(bcx, "alias.str.lower", &[rv]),
        MethodTarget::StringTrim => c.call_rt(bcx, "alias.str.trim", &[rv]),
        MethodTarget::ArrayLen => {
            let VTy::Array(_) = &svt else {
                invariant_violation("array.len 目标必须保留数组类型")
            };
            let raw = array_raw(bcx, rv);
            let t = c.call_rt(bcx, "alias.arr.len", &[raw])?;
            Ok(bcx.ins().sextend(types::I64, t))
        }
        MethodTarget::ArrayPush => {
            let VTy::Array(elem) = &svt else {
                invariant_violation("array.push 目标必须保留数组类型")
            };
            let [arg] = args else {
                invariant_violation("push 元数 (sema 已校验)")
            };
            let value = emit_expr_expected(c, bcx, frame, &arg.value, elem)?;
            let word = storage_word(bcx, value, elem);
            let raw = array_raw(bcx, rv);
            c.call_rt(bcx, "alias.arr.push", &[raw, word])?;
            bump_array_version(bcx, rv);
            Ok(bcx.ins().iconst(types::I64, 0))
        }
        MethodTarget::ArrayPop => {
            let VTy::Array(elem) = &svt else {
                invariant_violation("array.pop 目标必须保留数组类型")
            };
            let raw = array_raw(bcx, rv);
            let len = bcx.ins().load(types::I64, MemFlagsData::new(), raw, 8);
            let empty = bcx.ins().icmp_imm_s(IntCC::Equal, len, 0);
            let span_id = new_span_id(c, span);
            let abort_b = bcx.create_block();
            let ok_b = bcx.create_block();
            bcx.ins().brif(empty, abort_b, &[], ok_b, &[]);
            bcx.seal_block(abort_b);
            bcx.seal_block(ok_b);
            bcx.switch_to_block(abort_b);
            let aid = bcx.ins().iconst(types::I32, span_id as i64);
            c.call_rt(bcx, "alias.abort_pop", &[aid])?;
            jump_zero_return(bcx, frame);
            bcx.switch_to_block(ok_b);
            let raw_value = c.call_rt(bcx, "alias.arr.pop", &[raw])?;
            bump_array_version(bcx, rv);
            Ok(restore_word(bcx, raw_value, elem))
        }
        MethodTarget::ArrayIterator => {
            let VTy::Array(_) = &svt else {
                invariant_violation("array.iterator 目标必须保留数组类型")
            };
            make_iterator(c, bcx, rv)
        }
        MethodTarget::User {
            receiver,
            id: Some(method_id),
            ..
        } => {
            let receiver_vty = c.vty(receiver);
            if receiver_vty != svt {
                invariant_violation("已解析方法接收者与表达式静态类型一致")
            }
            let (param_vtys, ret_vty) = c
                .method_sigs
                .get(method_id)
                .cloned()
                .unwrap_or_else(|| invariant_violation("MethodId 必须存在于方法签名表"));
            let fid = *c
                .methods
                .get(method_id)
                .unwrap_or_else(|| invariant_violation("MethodId 必须存在函数 ID"));
            let fref = c.module.declare_func_in_func(fid, &mut bcx.func);
            let mut words: Vec<Value> = Vec::with_capacity(args.len() + 3);
            words.push(bcx.use_var(frame.globals));
            words.push(bcx.ins().iconst(types::I64, 0));
            words.push(norm_store(bcx, rv, &param_vtys[0]));
            for (arg, param) in args.iter().zip(param_vtys.iter().skip(1)) {
                let value = emit_expr_expected(c, bcx, frame, &arg.value, param)?;
                words.push(norm_store(bcx, value, param));
            }
            let inst = bcx.ins().call(fref, &words);
            if ret_vty == VTy::Unit {
                return Ok(bcx.ins().iconst(types::I64, 0));
            }
            let raw = first_result(bcx, inst);
            Ok(norm_load(bcx, raw, &ret_vty))
        }
        MethodTarget::User { id: None, .. } => {
            invariant_violation("用户方法调用必须在 HIR 中携带 MethodId")
        }
    }
}

pub(crate) fn field_offset<M: Module>(
    c: &Compiler<M>,
    recv: &Expr,
    field_index: usize,
) -> AliasResult<i32> {
    Ok(field_entry(c, recv, field_index)?.1)
}

pub(crate) fn field_vty<M: Module>(
    c: &Compiler<M>,
    recv: &Expr,
    field_index: usize,
) -> AliasResult<VTy> {
    Ok(field_entry(c, recv, field_index)?.0)
}

fn field_entry<M: Module>(
    c: &Compiler<M>,
    recv: &Expr,
    field_index: usize,
) -> AliasResult<(VTy, i32)> {
    if let VTy::Struct(s) = c.vty(recv.ty()) {
        if let Some(layout) = c.struct_layouts.get(&s) {
            if let Some(entry) = layout.fields.get(field_index) {
                return Ok((entry.vty.clone(), entry.offset));
            }
        }
    }
    invariant_violation("字段访问索引必须由 sema/HIR 解析到结构体布局");
}

pub(crate) fn emit_incdec<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    let [arg] = args else {
        return Err(native_err(span, format!("{name} 恰好接受 1 个参数")));
    };
    let Expr::Ident(target, Some(target_id), tspan, _) = &arg.value else {
        return Err(native_err(span, format!("{name} 的参数必须是可变绑定名")));
    };
    let Some(addr) = cell_addr(c, frame, *target_id) else {
        return Err(native_err(
            *tspan,
            format!("内部: '{target}' 的 BindingId 无存储"),
        ));
    };
    let vty = bound_vty(c, frame, *target_id);
    if !vty.is_numeric() {
        invariant_violation("increase/decrease 目标为数值绑定 (sema 已校验)");
    }
    let cur = read_cell(bcx, frame, &addr, &vty);
    let one = match &vty {
        VTy::F(FloatW::F32) => bcx.ins().f32const(1.0),
        VTy::F(FloatW::F64) => bcx.ins().f64const(1.0),
        _ => bcx.ins().iconst(types::I64, 1),
    };
    let op = if name == "increase" {
        BinOp::Add
    } else {
        BinOp::Sub
    };
    let next = emit_binary_values(c, bcx, frame, op, &vty, cur, one, span)?;
    write_cell(bcx, frame, &addr, next, &vty);
    Ok(bcx.ins().iconst(types::I64, 0))
}

pub(crate) fn emit_print<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    let [arg] = args else {
        return Err(native_err(span, format!("{name} 恰好接受 1 个参数")));
    };
    let v = emit_expr(c, bcx, frame, &arg.value)?;
    match c.vty(arg.value.ty()) {
        VTy::I(IntW::W32) | VTy::U(UIntW::U8) | VTy::U(UIntW::U16) => {
            let t = bcx.ins().ireduce(types::I32, v);
            let h = if name == "println" {
                "alias.println.i32"
            } else {
                "alias.print.i32"
            };
            c.call_rt(bcx, h, &[t])?;
        }
        _ => {
            let s = display_word(c, bcx, &arg.value, v)?;
            let h = if name == "println" {
                "alias.println.str"
            } else {
                "alias.print.str"
            };
            c.call_rt(bcx, h, &[s])?;
        }
    }
    Ok(bcx.ins().iconst(types::I64, 0))
}

pub(crate) fn new_span_id<M: Module>(c: &mut Compiler<M>, span: Span) -> i32 {
    c.span_table.push((span.line, span.col));
    c.span_table.len() as i32 - 1
}

pub(crate) fn emit_divrem_guard<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    l: Value,
    r: Value,
    signed: bool,
    bits: u32,
    span: Span,
    remainder: bool,
) -> AliasResult<Value> {
    let wt = ir_type_bits(bits);
    let zero = bcx.ins().iconst(wt, 0);
    let by_zero = bcx.ins().icmp(IntCC::Equal, r, zero);
    emit_abort_branch(c, bcx, frame, by_zero, "alias.abort_div", span)?;
    if signed {
        let m1 = bcx.ins().iconst(wt, -1);
        let mini = bcx.ins().iconst(
            wt,
            match bits {
                8 => i8::MIN as i64,
                16 => i16::MIN as i64,
                32 => i32::MIN as i64,
                _ => i64::MIN,
            },
        );
        let by_m1 = bcx.ins().icmp(IntCC::Equal, r, m1);
        let is_min = bcx.ins().icmp(IntCC::Equal, l, mini);
        let overflow = bcx.ins().band(by_m1, is_min);
        emit_abort_branch(c, bcx, frame, overflow, "alias.abort_overflow", span)?;
    }
    Ok(match (signed, remainder) {
        (true, false) => bcx.ins().sdiv(l, r),
        (false, false) => bcx.ins().udiv(l, r),
        (true, true) => bcx.ins().srem(l, r),
        (false, true) => bcx.ins().urem(l, r),
    })
}

fn emit_index_guard<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    idx32: Value,
    len32: Value,
    span: Span,
) -> AliasResult<()> {
    let span_id = new_span_id(c, span);
    let zero = bcx.ins().iconst(types::I32, 0);
    let neg = bcx.ins().icmp(IntCC::SignedLessThan, idx32, zero);
    let oob_hi = bcx
        .ins()
        .icmp(IntCC::SignedGreaterThanOrEqual, idx32, len32);
    let trap = bcx.ins().bor(neg, oob_hi);

    let abort_b = bcx.create_block();
    let ok_b = bcx.create_block();
    bcx.ins().brif(trap, abort_b, &[], ok_b, &[]);
    bcx.seal_block(abort_b);
    bcx.seal_block(ok_b);

    bcx.switch_to_block(abort_b);
    let aid = bcx.ins().iconst(types::I32, span_id as i64);
    c.call_rt(bcx, "alias.abort_oob", &[aid])?;
    jump_zero_return(bcx, frame);

    bcx.switch_to_block(ok_b);
    Ok(())
}
