// ---------------------------------------------------------------------------
// emit — 表达式/语句发射域: 单元格访问、控制流、迭代、调用与内建。
// ---------------------------------------------------------------------------
use super::*;
use super::{decl_vty, Frame, VTy};
use crate::ast::*;
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

pub(crate) fn cell_addr<M: Module>(c: &Compiler<M>, frame: &Frame, name: &str) -> Option<CellAddr> {
    for scope in frame.scopes.iter().rev() {
        if let Some(s) = scope.get(name) {
            return Some(match s {
                Slot::Local(v) => CellAddr::Reg(*v),
                Slot::Global(off) => CellAddr::GlobalOff(*off),
            });
        }
    }
    if let Some(idx) = frame.caps.get(name) {
        return Some(CellAddr::EnvLoad(*idx));
    }
    if frame.init_ctx {
        return None;
    }
    c.globals_final
        .get(name)
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
    name: &str,
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
        .insert(name.to_string(), Slot::Local(var));
    frame
        .locals_vty
        .last_mut()
        .unwrap_or_else(|| invariant_violation("作用域栈非空"))
        .insert(name.to_string(), vty);
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
//
// array<T> 在 Alias 值层是 2-word wrapper:
//   [0] raw array header (现有 alias.arr.* runtime ABI)
//   [1] structural version
// iterator<T> 是 3-word 对象:
//   [0] array wrapper
//   [1] cursor
//   [2] expected version
// 结构修改只需改 wrapper.version；所有别名共享 wrapper，因此必然失效。
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

/// iterator 失效是语言运行时错误。这里直接使用 Win32 stderr，避免为了一个
/// 语义层错误改动已有 raw-array runtime ABI / 契约表。
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
    bcx.ins().call(write_ref, &[stderr, ptr, len, written_addr, null]);

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
                let Expr::FuncLit { params, body, .. } = &b.value else {
                    return Err(native_err(b.span, "函数绑定必须由函数字面量初始化"));
                };
                let ret_vty = decl_vty(&b.ty, &c.struct_layouts);
                let param_vtys = params
                    .iter()
                    .map(|p| decl_vty(&p.ty, &c.struct_layouts))
                    .collect::<Vec<_>>();
                let v = emit_funclit_value_typed(c, bcx, frame, params, body, ret_vty.clone())?;
                emit_local_cell(
                    c,
                    bcx,
                    frame,
                    v,
                    VTy::Func(param_vtys, Box::new(ret_vty)),
                    &b.name,
                )?;
            } else {
                let vty = decl_vty(&b.ty, &c.struct_layouts);
                let v = emit_expr_expected(c, bcx, frame, &b.value, &vty)?;
                emit_local_cell(c, bcx, frame, v, vty, &b.name)?;
            }
            Ok(())
        }
        Stmt::FieldAssign { recv, field, value, .. } => {
            let fvty = field_vty(c, frame, recv, field)?;
            let v = emit_expr_expected(c, bcx, frame, value, &fvty)?;
            let p = emit_expr(c, bcx, frame, recv)?;
            let off = field_offset(c, frame, recv, field)?;
            let sv = norm_store(bcx, v, &fvty);
            bcx.ins().store(MemFlagsData::new(), sv, p, off);
            Ok(())
        }
        Stmt::Assign { target, value, .. } => {
            let tvty = vty_of_name(c, frame, target);
            let v = emit_expr_expected(c, bcx, frame, value, &tvty)?;
            match cell_addr(c, frame, target) {
                Some(addr) => {
                    write_cell(bcx, frame, &addr, v, &tvty);
                    Ok(())
                }
                None => Err(native_err(
                    Span::default(),
                    format!("赋值目标 '{target}' 未定义"),
                )),
            }
        }
        Stmt::ExprStmt { expr, .. } => {
            emit_expr(c, bcx, frame, expr)?;
            Ok(())
        }
        Stmt::Return { value, .. } => {
            let expected = frame.ret_vty.clone().unwrap_or(VTy::Other);
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
        Stmt::If { branches, else_body, .. } => {
            emit_if(c, bcx, frame, branches, else_body.as_deref(), ret_block)
        }
        Stmt::While { cond, body, .. } => emit_while(c, bcx, frame, cond, body, ret_block),
        Stmt::For { ty, iterable, name, body, span } => {
            let elem_vty = decl_vty(ty, &c.struct_layouts);
            emit_for(c, bcx, frame, iterable, name, body, &elem_vty, *span, ret_block)
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
            if else_body.is_some() { bcx.create_block() } else { end_b }
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
                has_fallthrough = true; // 最后条件为 false 直接到 end
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
    name: &str,
    body: &[Stmt],
    elem_vty: &VTy,
    span: Span,
    ret_block: Block,
) -> AliasResult<()> {
    ensure_current(bcx, frame);
    // iterable 恰好求值一次。
    let source_vty = static_vty(c, frame, iterable);
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
    let raw_elem = bcx.ins().load(cl_type(elem_vty), MemFlagsData::new(), addr, 0);
    let elem = norm_load(bcx, raw_elem, elem_vty);
    let next = bcx.ins().iadd_imm_s(cursor, 1);
    bcx.ins().store(MemFlagsData::new(), next, iter, 8);

    push_scope(frame);
    emit_local_cell(c, bcx, frame, elem, elem_vty.clone(), name)?;
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
        Expr::Int(n, _) => Ok(bcx.ins().iconst(types::I64, *n)),
        Expr::Float(v, _) => Ok(bcx.ins().f64const(*v)),
        Expr::Bool(b, _) => Ok(bcx.ins().iconst(types::I64, *b as i64)),
        Expr::Unit(_) => Ok(bcx.ins().iconst(types::I64, 0)),
        Expr::Str(parts, _) => emit_str(c, bcx, frame, parts),
        Expr::Ident(name, span) => match cell_addr(c, frame, name) {
            Some(addr) => {
                let vty = vty_of_name(c, frame, name);
                Ok(read_cell(bcx, frame, &addr, &vty))
            }
            None => Err(native_err(*span, format!("未定义的绑定 '{name}'"))),
        },
        Expr::Neg { expr, .. } => {
            let v = emit_expr(c, bcx, frame, expr)?;
            let t = static_vty(c, frame, expr);
            match t {
                VTy::I(w) => {
                    let wt = cl_type(&VTy::I(w));
                    let red = narrow(bcx, v, w.bits());
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
        Expr::Binary { op: BinOp::And, lhs, rhs, .. } => {
            emit_short_circuit(c, bcx, frame, false, lhs, rhs)
        }
        Expr::Binary { op: BinOp::Or, lhs, rhs, .. } => {
            emit_short_circuit(c, bcx, frame, true, lhs, rhs)
        }
        Expr::Binary { op, lhs, rhs, span } => {
            let l = emit_expr(c, bcx, frame, lhs)?;
            let r = emit_expr(c, bcx, frame, rhs)?;
            emit_binary(c, bcx, frame, *op, lhs, l, r, *span)
        }
        Expr::Ternary { cond, then_expr, else_expr, .. } => {
            let expected = static_vty(c, frame, e);
            emit_ternary_typed(c, bcx, frame, cond, then_expr, else_expr, &expected)
        }
        Expr::Call { callee, args, span } => emit_call(c, bcx, frame, callee, args, *span),
        Expr::MethodCall { recv, name, args, span } => {
            emit_method_call(c, bcx, frame, recv, name, args, *span)
        }
        Expr::Field { recv, name, .. } => {
            let p = emit_expr(c, bcx, frame, recv)?;
            let fvty = field_vty(c, frame, recv, name)?;
            let off = field_offset(c, frame, recv, name)?;
            let raw = bcx.ins().load(cl_type(&fvty), MemFlagsData::new(), p, off);
            Ok(norm_load(bcx, raw, &fvty))
        }
        Expr::Index { recv, idx, span } => {
            let array = emit_expr(c, bcx, frame, recv)?;
            let idxw = emit_expr(c, bcx, frame, idx)?;
            let elem_vty = match static_vty(c, frame, recv) {
                VTy::Array(inner) => (*inner).clone(),
                _ => invariant_violation("下标主语为 array (sema 已校验)"),
            };
            let raw_array = array_raw(bcx, array);
            let idx32 = bcx.ins().ireduce(types::I32, idxw);
            let len64 = bcx.ins().load(types::I64, MemFlagsData::new(), raw_array, 8);
            let len32 = bcx.ins().ireduce(types::I32, len64);
            emit_index_guard(c, bcx, frame, idx32, len32, *span)?;
            let dp = bcx.ins().load(types::I64, MemFlagsData::new(), raw_array, 0);
            let idx64 = bcx.ins().sextend(types::I64, idx32);
            let off = bcx.ins().imul_imm_s(idx64, 8);
            let addr = bcx.ins().iadd(dp, off);
            let raw = bcx.ins().load(cl_type(&elem_vty), MemFlagsData::new(), addr, 0);
            Ok(norm_load(bcx, raw, &elem_vty))
        }
        Expr::ArrayLit { elems, .. } => {
            let elem_vty = elems
                .first()
                .map(|el| static_vty(c, frame, el))
                .unwrap_or(VTy::Other);
            emit_array_lit_typed(c, bcx, frame, elems, &elem_vty)
        }
        Expr::FuncLit { params, body, .. } => emit_funclit_value(c, bcx, frame, params, body),
        Expr::Match { subject, arms, .. } => {
            let result_vty = static_vty(c, frame, e);
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
            let pvty = match static_vty(c, frame, expr) {
                VTy::Result(t, _) => vty_of_type_name(&c.struct_layouts, &t),
                _ => VTy::Other,
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
        bcx.ins().brif(l, join_b, &[BlockArg::Value(short)], rhs_b, &[]);
    } else {
        bcx.ins().brif(l, rhs_b, &[], join_b, &[BlockArg::Value(short)]);
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
    let subj = emit_expr(c, bcx, frame, subject)?;
    let tag = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 0);
    let is_ok = bcx.ins().icmp_imm_s(IntCC::Equal, tag, 0);
    let ok_b = bcx.create_block();
    let err_b = bcx.create_block();
    let join_b = bcx.create_block();
    let jv = bcx.append_block_param(join_b, types::I64);
    bcx.ins().brif(is_ok, ok_b, &[], err_b, &[]);
    bcx.seal_block(ok_b);
    bcx.seal_block(err_b);

    let ok_arm = arms
        .iter()
        .find(|a| a.ctor == CtorKind::Ok)
        .unwrap_or_else(|| invariant_violation("match ok 臂存在 (sema 已校验)"));
    let err_arm = arms
        .iter()
        .find(|a| a.ctor == CtorKind::Err)
        .unwrap_or_else(|| invariant_violation("match err 臂存在 (sema 已校验)"));
    let bind_vtys = match static_vty(c, frame, subject) {
        VTy::Result(ok, err) => (
            vty_of_type_name(&c.struct_layouts, &ok),
            vty_of_type_name(&c.struct_layouts, &err),
        ),
        _ => (VTy::Other, VTy::Other),
    };

    bcx.switch_to_block(ok_b);
    frame.terminated = false;
    let ok_joined = emit_match_arm(c, bcx, frame, ok_arm, bind_vtys.0, result_vty, subj, join_b)?;
    bcx.switch_to_block(err_b);
    frame.terminated = false;
    let err_joined = emit_match_arm(
        c,
        bcx,
        frame,
        err_arm,
        bind_vtys.1,
        result_vty,
        subj,
        join_b,
    )?;

    if ok_joined || err_joined {
        bcx.seal_block(join_b);
        bcx.switch_to_block(join_b);
        frame.terminated = false;
        Ok(restore_word(bcx, jv, result_vty))
    } else {
        ensure_current(bcx, frame);
        Ok(bcx.ins().iconst(types::I64, 0))
    }
}

pub(crate) fn emit_expr_expected<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    e: &Expr,
    expected: &VTy,
) -> AliasResult<Value> {
    match (e, expected) {
        (Expr::Ternary { cond, then_expr, else_expr, .. }, _) => {
            emit_ternary_typed(c, bcx, frame, cond, then_expr, else_expr, expected)
        }
        (Expr::Match { subject, arms, .. }, _) => {
            emit_match_typed(c, bcx, frame, subject, arms, expected)
        }
        (Expr::ArrayLit { elems, .. }, VTy::Array(elem)) => {
            emit_array_lit_typed(c, bcx, frame, elems, elem)
        }
        (Expr::Call { callee, args, .. }, VTy::Result(ok, err)) => {
            if let Expr::Ident(name, _) = callee.as_ref() {
                if (name == "ok" || name == "err") && cell_addr(c, frame, name).is_none() {
                    let payload = if name == "ok" {
                        vty_of_type_name(&c.struct_layouts, ok)
                    } else {
                        vty_of_type_name(&c.struct_layouts, err)
                    };
                    return emit_result_ctor_typed(c, bcx, frame, name, args, &payload);
                }
            }
            emit_expr(c, bcx, frame, e)
        }
        (Expr::Float(..), VTy::F(FloatW::F32)) => {
            let v = emit_expr(c, bcx, frame, e)?;
            Ok(norm_store(bcx, v, expected))
        }
        _ => emit_expr(c, bcx, frame, e),
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
    let lt = static_vty(c, frame, lhs);
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
        Add | Sub | Mul | Div => match lt {
            VTy::F(_) => match op {
                Add => Ok(bcx.ins().fadd(l, r)),
                Sub => Ok(bcx.ins().fsub(l, r)),
                Mul => Ok(bcx.ins().fmul(l, r)),
                _ => Ok(bcx.ins().fdiv(l, r)),
            },
            VTy::I(w) => {
                let wt = cl_type(&VTy::I(*w));
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Add => bcx.ins().iadd(li, ri),
                    Sub => bcx.ins().isub(li, ri),
                    Mul => bcx.ins().imul(li, ri),
                    _ => emit_div_guard(c, bcx, frame, li, ri, true, w.bits(), span)?,
                };
                Ok(widen_signed(bcx, v, wt))
            }
            VTy::U(w) => {
                let wt = cl_type(&VTy::U(*w));
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Add => bcx.ins().iadd(li, ri),
                    Sub => bcx.ins().isub(li, ri),
                    Mul => bcx.ins().imul(li, ri),
                    _ => emit_div_guard(c, bcx, frame, li, ri, false, w.bits(), span)?,
                };
                Ok(widen_unsigned(bcx, v, wt))
            }
            _ => invariant_violation("算术操作数为数值族 (sema 已校验)"),
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
    if ty == types::I64 { v } else { bcx.ins().ireduce(ty, v) }
}

fn widen_signed(bcx: &mut FunctionBuilder, v: Value, to: cranelift_codegen::ir::Type) -> Value {
    if to == types::I64 { v } else { bcx.ins().sextend(types::I64, v) }
}

fn widen_unsigned(bcx: &mut FunctionBuilder, v: Value, to: cranelift_codegen::ir::Type) -> Value {
    if to == types::I64 { v } else { bcx.ins().uextend(types::I64, v) }
}

pub(crate) fn emit_match_arm<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    arm: &MatchArm,
    bind_vty: VTy,
    result_vty: &VTy,
    subj: Value,
    join_b: Block,
) -> AliasResult<bool> {
    let raw = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 8);
    let payload = restore_word(bcx, raw, &bind_vty);
    push_scope(frame);
    emit_local_cell(c, bcx, frame, payload, bind_vty, &arm.binding)?;
    let joined = match &arm.body {
        ArmBody::Value(e) => {
            let v = emit_expr_expected(c, bcx, frame, e, result_vty)?;
            let word = storage_word(bcx, v, result_vty);
            bcx.ins().jump(join_b, &[BlockArg::Value(word)]);
            true
        }
        ArmBody::Ret(e) => {
            let expected = frame.ret_vty.clone().unwrap_or(VTy::Other);
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
                let v = tail.unwrap_or_else(|| bcx.ins().iconst(types::I64, 0));
                let word = storage_word(bcx, v, result_vty);
                bcx.ins().jump(join_b, &[BlockArg::Value(word)]);
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
    parts: &[StrPartAst],
) -> AliasResult<Value> {
    let z8 = bcx.ins().iconst(types::I64, 0);
    let z4 = bcx.ins().iconst(types::I32, 0);
    let empty = c.call_rt(bcx, "alias.str.new", &[z8, z4])?;
    let mut acc = empty;
    for p in parts {
        let piece = match p {
            StrPartAst::Lit(s) => str_literal_handle(c, bcx, s)?,
            StrPartAst::Hole(h) => {
                let w = emit_expr(c, bcx, frame, h)?;
                display_word(c, bcx, frame, h, w)?
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
    frame: &Frame,
    e: &Expr,
    w: Value,
) -> AliasResult<Value> {
    match static_vty(c, frame, e) {
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
        VTy::Unit => c.call_rt(bcx, "alias.display.unit", &[]),
        VTy::Func(..) => c.call_rt(bcx, "alias.display.func", &[]),
        VTy::Struct(_) => c.call_rt(bcx, "alias.display.struct", &[]),
        VTy::Array(_) => c.call_rt(bcx, "alias.display.array", &[]),
        VTy::Iterator(_) => str_literal_handle(c, bcx, "<iterator>"),
        VTy::Result(..) => {
            let tag = bcx.ins().load(types::I64, MemFlagsData::new(), w, 0);
            let t = bcx.ins().ireduce(types::I32, tag);
            c.call_rt(bcx, "alias.display.result", &[t])
        }
        VTy::Other => Err(native_err(e.span(), "原生后端无法推断该表达式的显示类型")),
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
    span: Span,
) -> AliasResult<Value> {
    if let Expr::Ident(name, _) = callee {
        if name == "increase" || name == "decrease" {
            return emit_incdec(c, bcx, frame, name, args, span);
        }
        if name == "println" || name == "print" {
            return emit_print(c, bcx, frame, name, args, span);
        }
        if let Some(target) = conv_target_vty(name) {
            let [arg] = args else {
                invariant_violation("转换内建元数 (sema 已校验)")
            };
            let v = emit_expr(c, bcx, frame, &arg.value)?;
            let src = static_vty(c, frame, &arg.value);
            return emit_convert(c, bcx, frame, span, v, &src, &target);
        }
        if name == "typeof" {
            let [arg] = args else {
                invariant_violation("typeof 元数 (sema 已校验)")
            };
            emit_expr(c, bcx, frame, &arg.value)?;
            let tn = static_vty(c, frame, &arg.value).display_name();
            return str_literal_handle(c, bcx, &tn);
        }
    }
    if let Expr::FuncLit { params, body, .. } = callee {
        let clo = emit_funclit_value(c, bcx, frame, params, body)?;
        let param_vtys: Vec<VTy> = params
            .iter()
            .map(|p| decl_vty(&p.ty, &c.struct_layouts))
            .collect();
        let ret_vty = infer_ret_vty(c, frame, params, body);
        return call_closure(c, bcx, frame, clo, &param_vtys, &ret_vty, args);
    }
    let clo = match callee {
        Expr::Ident(name, _) => {
            if c.struct_layouts.contains_key(name) && cell_addr(c, frame, name).is_none() {
                return emit_construct(c, bcx, frame, name, args);
            }
            if (name == "ok" || name == "err") && cell_addr(c, frame, name).is_none() {
                return emit_result_ctor(c, bcx, frame, name, args);
            }
            match cell_addr(c, frame, name) {
                Some(addr) => {
                    let callee_vty = vty_of_name(c, frame, name);
                    read_cell(bcx, frame, &addr, &callee_vty)
                }
                None => return Err(native_err(span, format!("未定义的绑定 '{name}'"))),
            }
        }
        _ => return Err(native_err(span, "函数值尚未接入原生后端 (Phase 3)")),
    };
    if let Expr::Ident(name, _) = callee {
        return match vty_of_name(c, frame, name) {
            VTy::Func(param_vtys, ret_vty) => {
                call_closure(c, bcx, frame, clo, &param_vtys, &ret_vty, args)
            }
            _ => Err(native_err(span, format!("'{name}' 不是带签名的函数绑定"))),
        };
    }
    invariant_violation("被调方形态 (上方已分派)")
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
    let raw = first_result(bcx, inst);
    Ok(norm_load(bcx, raw, ret_vty))
}

fn jump_zero_return(bcx: &mut FunctionBuilder, frame: &Frame) {
    let rb = frame
        .ret_block
        .unwrap_or_else(|| invariant_violation("运行时错误传播需要返回块"));
    let [param] = bcx.block_params(rb) else {
        invariant_violation("返回块恰有一个参数")
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

pub(crate) fn conv_target_vty(name: &str) -> Option<VTy> {
    Some(match name {
        "to_i8" => VTy::I(IntW::W8),
        "to_i16" => VTy::I(IntW::W16),
        "to_i32" => VTy::I(IntW::W32),
        "to_i64" => VTy::I(IntW::W64),
        "to_u8" => VTy::U(UIntW::U8),
        "to_u16" => VTy::U(UIntW::U16),
        "to_u32" => VTy::U(UIntW::U32),
        "to_u64" => VTy::U(UIntW::U64),
        "to_f32" => VTy::F(FloatW::F32),
        "to_f64" => VTy::F(FloatW::F64),
        _ => return None,
    })
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
        _ => invariant_violation("转换目标为数值族"),
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
    name: &str,
    args: &[CallArg],
) -> AliasResult<Value> {
    let [arg] = args else {
        return Err(native_err(Span::default(), format!("{name} 构造恰好接受 1 个参数")));
    };
    let pvty = static_vty(c, frame, &arg.value);
    emit_result_ctor_typed(c, bcx, frame, name, args, &pvty)
}

fn emit_result_ctor_typed<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
    pvty: &VTy,
) -> AliasResult<Value> {
    let [arg] = args else {
        return Err(native_err(Span::default(), format!("{name} 构造恰好接受 1 个参数")));
    };
    let payload = emit_expr_expected(c, bcx, frame, &arg.value, pvty)?;
    let pw = storage_word(bcx, payload, pvty);
    let n2 = bcx.ins().iconst(types::I32, 2);
    let blk = c.call_rt(bcx, "alias.env.new", &[n2])?;
    let tag = if name == "ok" { 0i64 } else { 1i64 };
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
    name: &str,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    // 接收者先求值；所有内建/用户方法都保持这一求值顺序。
    let rv = emit_expr(c, bcx, frame, recv)?;
    let svt = static_vty(c, frame, recv);

    // 运算符对应的编译器内建扩展函数：和符号运算走完全同一条机器语义。
    if svt.is_numeric() {
        let op = match name {
            "plus" => Some(BinOp::Add),
            "minus" => Some(BinOp::Sub),
            "times" => Some(BinOp::Mul),
            "div" => Some(BinOp::Div),
            _ => None,
        };
        if let Some(op) = op {
            let [arg] = args else {
                invariant_violation("算术扩展函数元数 (sema 已校验)")
            };
            let r = emit_expr_expected(c, bcx, frame, &arg.value, &svt)?;
            return emit_binary_values(c, bcx, frame, op, &svt, rv, r, span);
        }
    }
    if svt == VTy::Bool && name == "not" {
        if !args.is_empty() {
            invariant_violation("not 扩展函数元数 (sema 已校验)");
        }
        return Ok(emit_bool_not(bcx, rv));
    }

    if let VTy::Array(elem) = &svt {
        match name {
            "len" => {
                let raw = array_raw(bcx, rv);
                let t = c.call_rt(bcx, "alias.arr.len", &[raw])?;
                return Ok(bcx.ins().sextend(types::I64, t));
            }
            "push" => {
                let [a] = args else {
                    invariant_violation("push 元数 (sema 已校验)")
                };
                let v = emit_expr_expected(c, bcx, frame, &a.value, elem)?;
                let sw = storage_word(bcx, v, elem);
                let raw = array_raw(bcx, rv);
                c.call_rt(bcx, "alias.arr.push", &[raw, sw])?;
                bump_array_version(bcx, rv);
                return Ok(bcx.ins().iconst(types::I64, 0));
            }
            "pop" => {
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
                return Ok(restore_word(bcx, raw_value, elem));
            }
            "iterator" => return make_iterator(c, bcx, rv),
            _ => {}
        }
    }

    let tname = match &svt {
        VTy::Other | VTy::Unit => {
            return Err(native_err(span, "原生后端无法推断该表达式的接收者类型"))
        }
        other => other.display_name(),
    };
    if let Some((param_vtys, ret_vty)) = c
        .method_sigs
        .get(&(tname.clone(), name.to_string()))
        .cloned()
    {
        let fid = c.methods[&(tname.clone(), name.to_string())];
        let fref = c.module.declare_func_in_func(fid, &mut bcx.func);
        let mut words: Vec<Value> = Vec::with_capacity(args.len() + 3);
        words.push(bcx.use_var(frame.globals));
        words.push(bcx.ins().iconst(types::I64, 0));
        words.push(norm_store(bcx, rv, &param_vtys[0]));
        for (a, pt) in args.iter().zip(param_vtys.iter().skip(1)) {
            let v = emit_expr_expected(c, bcx, frame, &a.value, pt)?;
            words.push(norm_store(bcx, v, pt));
        }
        let inst = bcx.ins().call(fref, &words);
        let raw = first_result(bcx, inst);
        return Ok(norm_load(bcx, raw, &ret_vty));
    }
    if tname == "string" {
        match name {
            "len" => {
                let t = c.call_rt(bcx, "alias.str.len", &[rv])?;
                return Ok(bcx.ins().sextend(types::I64, t));
            }
            "upper" => return c.call_rt(bcx, "alias.str.upper", &[rv]),
            "lower" => return c.call_rt(bcx, "alias.str.lower", &[rv]),
            "trim" => return c.call_rt(bcx, "alias.str.trim", &[rv]),
            _ => {}
        }
    }
    Err(native_err(span, format!("类型 {tname} 上没有方法 '{name}'")))
}

pub(crate) fn field_offset<M: Module>(
    c: &Compiler<M>,
    frame: &Frame,
    recv: &Expr,
    field: &str,
) -> AliasResult<i32> {
    Ok(field_entry(c, frame, recv, field)?.1)
}

pub(crate) fn field_vty<M: Module>(
    c: &Compiler<M>,
    frame: &Frame,
    recv: &Expr,
    field: &str,
) -> AliasResult<VTy> {
    Ok(field_entry(c, frame, recv, field)?.0)
}

fn field_entry<M: Module>(
    c: &Compiler<M>,
    frame: &Frame,
    recv: &Expr,
    field: &str,
) -> AliasResult<(VTy, i32)> {
    if let VTy::Struct(s) = static_vty(c, frame, recv) {
        if let Some(layout) = c.struct_layouts.get(&s) {
            if let Some(entry) = layout.fields.iter().find(|entry| entry.name == field) {
                return Ok((entry.vty.clone(), entry.offset));
            }
        }
    }
    invariant_violation("字段访问目标为结构体实例 (sema 已校验)");
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
    let Expr::Ident(target, tspan) = &arg.value else {
        return Err(native_err(span, format!("{name} 的参数必须是可变绑定名")));
    };
    let Some(addr) = cell_addr(c, frame, target) else {
        return Err(native_err(*tspan, format!("'{target}' 未定义")));
    };
    let cur = read_cell(bcx, frame, &addr, &VTy::I(IntW::W32));
    let delta = if name == "increase" { 1i64 } else { -1i64 };
    let cur32 = bcx.ins().ireduce(types::I32, cur);
    let next = bcx.ins().iadd_imm_s(cur32, delta);
    let nextw = bcx.ins().sextend(types::I64, next);
    write_cell(bcx, frame, &addr, nextw, &VTy::I(IntW::W32));
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
    match static_vty(c, frame, &arg.value) {
        VTy::I(IntW::W32) | VTy::U(UIntW::U8) | VTy::U(UIntW::U16) => {
            let t = bcx.ins().ireduce(types::I32, v);
            let h = if name == "println" { "alias.println.i32" } else { "alias.print.i32" };
            c.call_rt(bcx, h, &[t])?;
        }
        _ => {
            let s = display_word(c, bcx, frame, &arg.value, v)?;
            let h = if name == "println" { "alias.println.str" } else { "alias.print.str" };
            c.call_rt(bcx, h, &[s])?;
        }
    }
    Ok(bcx.ins().iconst(types::I64, 0))
}

pub(crate) fn new_span_id<M: Module>(c: &mut Compiler<M>, span: Span) -> i32 {
    c.span_table.push((span.line, span.col));
    c.span_table.len() as i32 - 1
}

pub(crate) fn emit_div_guard<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    l: Value,
    r: Value,
    signed: bool,
    bits: u32,
    span: Span,
) -> AliasResult<Value> {
    let span_id = new_span_id(c, span);
    let wt = ir_type_bits(bits);
    let zero = bcx.ins().iconst(wt, 0);
    let by_zero = bcx.ins().icmp(IntCC::Equal, r, zero);
    let trap = if signed {
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
        let m1_min = bcx.ins().band(by_m1, is_min);
        bcx.ins().bor(by_zero, m1_min)
    } else {
        by_zero
    };

    let abort_b = bcx.create_block();
    let ok_b = bcx.create_block();
    bcx.ins().brif(trap, abort_b, &[], ok_b, &[]);
    bcx.seal_block(abort_b);
    bcx.seal_block(ok_b);

    bcx.switch_to_block(abort_b);
    let aid = bcx.ins().iconst(types::I32, span_id as i64);
    c.call_rt(bcx, "alias.abort_div", &[aid])?;
    jump_zero_return(bcx, frame);

    bcx.switch_to_block(ok_b);
    Ok(if signed { bcx.ins().sdiv(l, r) } else { bcx.ins().udiv(l, r) })
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
    let oob_hi = bcx.ins().icmp(IntCC::SignedGreaterThanOrEqual, idx32, len32);
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
