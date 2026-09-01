use super::arrays::{array_len, array_raw, bump_array_version, make_iterator};
use super::cells::{binding_storage_addr, emit_temporary_cell, first_result};
use super::clone::emit_deep_clone;
use super::expr::emit_expr;
use super::ops::{emit_abort_branch, emit_binary_values};
use super::places::emit_place_addr;
use super::shallow::emit_shallow_clone;
use super::strings::display_word;
use crate::codegen::abi::{cl_type, norm_load, norm_store, user_signature, VTy};
use crate::codegen::layout::{
    result_layout, result_tag, CLOSURE_CODE_OFFSET, CLOSURE_ENV_OFFSET, RESULT_TAG_OFFSET,
};
use crate::codegen::{bound_vty, invariant_violation, Compiler, Frame};
use crate::sema::hir::{
    ArgumentPass, BinOp, BuiltinCall, CallArg, CallTarget, CtorKind, Expr, MethodTarget,
};
use crate::sema::types::{FloatW, IntW, UIntW};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;

pub(crate) fn emit_call<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    callee: &Expr,
    args: &[CallArg],
    resolved: (&CallTarget, &VTy, Span),
) -> AliasResult<Value> {
    let (target, result_vty, span) = resolved;
    match target {
        CallTarget::Builtin(BuiltinCall::Increase) => {
            emit_incdec(c, bcx, frame, BinOp::Add, args, span)
        }
        CallTarget::Builtin(BuiltinCall::Decrease) => {
            emit_incdec(c, bcx, frame, BinOp::Sub, args, span)
        }
        CallTarget::Builtin(BuiltinCall::Print) => emit_print(c, bcx, frame, false, args),
        CallTarget::Builtin(BuiltinCall::Println) => emit_print(c, bcx, frame, true, args),
        CallTarget::Builtin(BuiltinCall::DeepClone(plan)) => {
            let [arg] = args else {
                invariant_violation("clone 元数 (sema 已校验)")
            };
            emit_deep_clone(c, bcx, frame, &arg.value, plan)
        }
        CallTarget::Builtin(BuiltinCall::ShallowClone(plan)) => {
            let [arg] = args else {
                invariant_violation("shallow 元数 (sema 已校验)")
            };
            emit_shallow_clone(c, bcx, frame, &arg.value, plan)
        }
        CallTarget::StructConstructor {
            name,
            arg_field_indices,
        } => emit_construct(c, bcx, frame, name, args, arg_field_indices),
        CallTarget::ResultConstructor(kind) => {
            emit_result_ctor(c, bcx, frame, *kind, args, result_vty)
        }
        CallTarget::FunctionValue => {
            let callee_vty = c.vty(callee.ty());
            let VTy::Func(param_vtys, ret_vty) = callee_vty else {
                invariant_violation("函数值调用必须携带完整函数签名")
            };
            let clo = emit_expr(c, bcx, frame, callee)?
                .into_scalar("closure call target 收到 multi-lane expression value");
            call_closure(c, bcx, frame, clo, &param_vtys, &ret_vty, args)
        }
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
        let pass = a
            .pass
            .as_ref()
            .unwrap_or_else(|| invariant_violation("user call argument 缺少 resolved pass"));
        words.push(emit_user_argument(c, bcx, frame, &a.value, pass, pt)?);
    }
    let code = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), clo, CLOSURE_CODE_OFFSET);
    let env = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), clo, CLOSURE_ENV_OFFSET);
    // user_signature 固定要求 [globals, closure env, 显式参数...]。这里逆序插入两个
    // 隐藏值以保持该前缀；若与被调方各自维护顺序，所有显式参数都会整体错位。
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

fn emit_user_argument<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    value: &Expr,
    pass: &ArgumentPass,
    vty: &VTy,
) -> AliasResult<Value> {
    match pass {
        ArgumentPass::Inline | ArgumentPass::Owned => {
            let value = emit_expr(c, bcx, frame, value)?;
            let value = value.into_scalar("direct user argument 尚未支持 multi-lane value");
            Ok(norm_store(bcx, value, vty))
        }
        ArgumentPass::ReadBorrow { source, .. } | ArgumentPass::WriteBorrow { source, .. } => {
            let (address, source_vty) = emit_place_addr(c, bcx, frame, source)?;
            if source_vty != *vty {
                invariant_violation("borrow argument source ABI 与 parameter ABI 漂移")
            }
            Ok(address)
        }
        ArgumentPass::BorrowTemporary { kind } => {
            let _ = kind;
            let value = emit_expr(c, bcx, frame, value)?;
            emit_temporary_cell(c, bcx, value, vty)
        }
    }
}

pub(crate) fn emit_construct<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
    arg_field_indices: &[usize],
) -> AliasResult<Value> {
    let layout = c.struct_layouts[name].clone();
    let bytes = bcx.ins().iconst(types::I64, layout.size as i64);
    let ptr = c.call_rt(bcx, "alias.cell.new", &[bytes])?;
    if args.len() != arg_field_indices.len() {
        invariant_violation("构造器实参与字段索引必须一一对应")
    }
    for (field_index, field) in layout.fields.iter().enumerate() {
        let expr = args
            .iter()
            .zip(arg_field_indices)
            .find(|(_, index)| **index == field_index)
            .map(|(arg, _)| &arg.value)
            .or(field.default.as_ref())
            .unwrap_or_else(|| invariant_violation("构造字段全覆盖 (sema 已校验)"));
        let v = emit_expr(c, bcx, frame, expr)?;
        v.store(bcx, ptr, field.offset, &field.vty);
    }
    Ok(ptr)
}

pub(crate) fn emit_result_ctor<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    kind: CtorKind,
    args: &[CallArg],
    result_vty: &VTy,
) -> AliasResult<Value> {
    let [arg] = args else {
        invariant_violation("result 构造元数 (sema 已校验)")
    };
    let VTy::Result(ok_vty, err_vty) = result_vty else {
        invariant_violation("result constructor 必须携带完整 result VTy")
    };
    let pvty = match kind {
        CtorKind::Ok => ok_vty.as_ref(),
        CtorKind::Err => err_vty.as_ref(),
    };
    if c.vty(arg.value.ty()) != *pvty {
        invariant_violation("result constructor payload VTy 与 resolved variant 漂移")
    }
    let payload = emit_expr(c, bcx, frame, &arg.value)?;
    let layout = result_layout(ok_vty, err_vty);
    let bytes = bcx.ins().iconst(types::I64, layout.size as i64);
    let blk = c.call_rt(bcx, "alias.cell.new", &[bytes])?;
    let tagw = bcx.ins().iconst(types::I64, result_tag(kind));
    bcx.ins()
        .store(MemFlagsData::new(), tagw, blk, RESULT_TAG_OFFSET);
    payload.store(bcx, blk, layout.payload_offset, pvty);
    Ok(blk)
}

pub(crate) fn emit_method_call<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    receiver: (&Expr, Option<&ArgumentPass>),
    args: &[CallArg],
    target: &MethodTarget,
    span: Span,
) -> AliasResult<Value> {
    let (recv, receiver_pass) = receiver;
    let svt = c.vty(recv.ty());
    let rv = if matches!(target, MethodTarget::User { .. }) {
        let pass = receiver_pass
            .unwrap_or_else(|| invariant_violation("user method receiver 缺少 resolved pass"));
        emit_user_argument(c, bcx, frame, recv, pass, &svt)?
    } else {
        if receiver_pass.is_some() {
            invariant_violation("builtin method receiver 携带 user pass")
        }
        emit_expr(c, bcx, frame, recv)?
            .into_scalar("builtin method receiver 收到 multi-lane expression value")
    };

    match target {
        MethodTarget::Numeric(op) => {
            let [arg] = args else {
                invariant_violation("算术扩展函数元数 (sema 已校验)")
            };
            let r = emit_expr(c, bcx, frame, &arg.value)?;
            let r = r.into_scalar("numeric method argument 收到 multi-lane expression value");
            emit_binary_values(c, bcx, (*op, &svt, rv, r, span))
        }
        MethodTarget::BoolNot => {
            if !args.is_empty() {
                invariant_violation("not 扩展函数元数 (sema 已校验)");
            }
            let b = bcx.ins().icmp_imm_s(IntCC::Equal, rv, 0);
            Ok(bcx.ins().uextend(types::I64, b))
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
            let value = emit_expr(c, bcx, frame, &arg.value)?;
            let raw = array_raw(bcx, rv);
            let slot = c.call_rt(bcx, "alias.arr.push", &[raw])?;
            value.store(bcx, slot, 0, elem);
            bump_array_version(bcx, rv);
            Ok(bcx.ins().iconst(types::I64, 0))
        }
        MethodTarget::ArrayPop => {
            let VTy::Array(elem) = &svt else {
                invariant_violation("array.pop 目标必须保留数组类型")
            };
            let raw = array_raw(bcx, rv);
            let len = array_len(bcx, raw);
            let empty = bcx.ins().icmp_imm_s(IntCC::Equal, len, 0);
            emit_abort_branch(c, bcx, empty, "alias.abort_pop", span)?;
            let slot = c.call_rt(bcx, "alias.arr.pop", &[raw])?;
            let value = super::value::ExprValue::load(bcx, slot, 0, elem)
                .into_scalar("array.pop result 尚未支持 multi-lane method result");
            bump_array_version(bcx, rv);
            Ok(value)
        }
        MethodTarget::ArrayIterator => {
            let VTy::Array(_) = &svt else {
                invariant_violation("array.iterator 目标必须保留数组类型")
            };
            make_iterator(c, bcx, rv)
        }
        MethodTarget::User {
            receiver,
            id: method_id,
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
            let fref = c.module.declare_func_in_func(fid, bcx.func);
            let mut words: Vec<Value> = Vec::with_capacity(args.len() + 3);
            words.push(bcx.use_var(frame.globals));
            words.push(bcx.ins().iconst(types::I64, 0));
            words.push(rv);
            for (arg, param) in args.iter().zip(param_vtys.iter().skip(1)) {
                let pass = arg.pass.as_ref().unwrap_or_else(|| {
                    invariant_violation("user method argument 缺少 resolved pass")
                });
                words.push(emit_user_argument(c, bcx, frame, &arg.value, pass, param)?);
            }
            let inst = bcx.ins().call(fref, &words);
            if ret_vty == VTy::Unit {
                return Ok(bcx.ins().iconst(types::I64, 0));
            }
            let raw = first_result(bcx, inst);
            Ok(norm_load(bcx, raw, &ret_vty))
        }
    }
}

fn emit_incdec<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    op: BinOp,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    let [arg] = args else {
        invariant_violation("increase/decrease 元数 (sema 已校验)")
    };
    let Expr::Ident(_, Some(target_id), _, _) = &arg.value else {
        invariant_violation("increase/decrease 参数为已解析可变绑定 (sema 已校验)")
    };
    let addr = binding_storage_addr(c, bcx, frame, *target_id)
        .unwrap_or_else(|| invariant_violation("increase/decrease BindingId 必须有存储"));
    let vty = bound_vty(c, frame, *target_id);
    if !vty.is_numeric() {
        invariant_violation("increase/decrease 目标为数值绑定 (sema 已校验)");
    }
    let raw = bcx.ins().load(cl_type(&vty), MemFlagsData::new(), addr, 0);
    let cur = norm_load(bcx, raw, &vty);
    let one = match &vty {
        VTy::F(FloatW::F32) => bcx.ins().f32const(1.0),
        VTy::F(FloatW::F64) => bcx.ins().f64const(1.0),
        _ => bcx.ins().iconst(types::I64, 1),
    };
    let next = emit_binary_values(c, bcx, (op, &vty, cur, one, span))?;
    let stored = norm_store(bcx, next, &vty);
    bcx.ins().store(MemFlagsData::new(), stored, addr, 0);
    Ok(bcx.ins().iconst(types::I64, 0))
}

fn emit_print<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    newline: bool,
    args: &[CallArg],
) -> AliasResult<Value> {
    let [arg] = args else {
        invariant_violation("print/println 元数 (sema 已校验)")
    };
    let v = emit_expr(c, bcx, frame, &arg.value)?;
    let v = v.into_scalar("display 尚未支持 multi-lane expression value");
    match c.vty(arg.value.ty()) {
        VTy::I(IntW::W32) | VTy::U(UIntW::U8) | VTy::U(UIntW::U16) => {
            let t = bcx.ins().ireduce(types::I32, v);
            let symbol = if newline {
                "alias.println.i32"
            } else {
                "alias.print.i32"
            };
            c.call_rt_void(bcx, symbol, &[t])?;
        }
        _ => {
            let s = display_word(c, bcx, &arg.value, v)?;
            let symbol = if newline {
                "alias.println.str"
            } else {
                "alias.print.str"
            };
            c.call_rt_void(bcx, symbol, &[s])?;
        }
    }
    Ok(bcx.ins().iconst(types::I64, 0))
}
