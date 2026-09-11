use super::arrays::{array_len, array_raw, bump_array_version, make_iterator};
use super::cells::{
    allocate_value_cell, binding_storage_addr, emit_temporary_cell, first_result,
};
use super::clone::emit_deep_clone;
use super::destruction::emit_destroy_value;
use super::expr::{emit_container_value, emit_expr};
use super::ops::{emit_abort_branch, emit_binary_values};
use super::places::{emit_place_addr, emit_place_value};
use super::shallow::emit_shallow_clone;
use super::strings::display_word;
use super::value::ExprValue;
use crate::codegen::abi::{
    cl_type, norm_load, norm_store, user_function_abi, UserFunctionAbi,
    UserParameterPassing, UserReturnPassing, VTy,
};
use crate::codegen::layout::{
    result_layout, result_tag, CLOSURE_CODE_OFFSET, CLOSURE_ENV_OFFSET, RESULT_TAG_OFFSET,
};
use crate::codegen::{bound_vty, invariant_violation, Compiler, Frame};
use crate::sema::hir::{
    ArgumentPass, BinOp, BorrowKind, BuiltinCall, CallArg, CallTarget, CtorKind, DestroyPlan, Expr,
    MethodTarget,
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
) -> AliasResult<ExprValue> {
    let (target, result_vty, span) = resolved;
    match target {
        CallTarget::Builtin(BuiltinCall::Increase) => {
            emit_incdec(c, bcx, frame, BinOp::Add, args, span).map(ExprValue::scalar)
        }
        CallTarget::Builtin(BuiltinCall::Decrease) => {
            emit_incdec(c, bcx, frame, BinOp::Sub, args, span).map(ExprValue::scalar)
        }
        CallTarget::Builtin(BuiltinCall::Print) => {
            emit_print(c, bcx, frame, false, args).map(ExprValue::scalar)
        }
        CallTarget::Builtin(BuiltinCall::Println) => {
            emit_print(c, bcx, frame, true, args).map(ExprValue::scalar)
        }
        CallTarget::Builtin(BuiltinCall::DeepClone(plan)) => {
            let [arg] = args else {
                invariant_violation("clone 元数 (sema 已校验)")
            };
            emit_deep_clone(c, bcx, frame, &arg.value, plan).map(ExprValue::scalar)
        }
        CallTarget::Builtin(BuiltinCall::ShallowClone(plan)) => {
            let [arg] = args else {
                invariant_violation("shallow 元数 (sema 已校验)")
            };
            emit_shallow_clone(c, bcx, frame, &arg.value, plan).map(ExprValue::scalar)
        }
        CallTarget::StructConstructor {
            name,
            arg_field_indices,
        } => emit_construct(c, bcx, frame, name, args, arg_field_indices).map(ExprValue::scalar),
        CallTarget::ResultConstructor(kind) => {
            emit_result_ctor(c, bcx, frame, *kind, args, result_vty).map(ExprValue::scalar)
        }
        CallTarget::FunctionValue => {
            let callee_vty = c.vty(callee.ty());
            let VTy::Func {
                params: param_vtys,
                param_effects,
                ret: ret_vty,
            } = callee_vty
            else {
                invariant_violation("函数值调用必须携带完整函数签名")
            };
            let clo = emit_expr(c, bcx, frame, callee)?
                .into_scalar("closure call target 收到 multi-lane expression value");
            call_closure(
                c,
                bcx,
                frame,
                clo,
                (&param_vtys, &param_effects, &ret_vty),
                args,
            )
        }
    }
}

fn call_closure<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    clo: Value,
    signature: (&[VTy], &[crate::sema::types::ParamEffect], &VTy),
    args: &[CallArg],
) -> AliasResult<ExprValue> {
    let (param_vtys, param_effects, ret_vty) = signature;
    let abi = user_function_abi(c.cc, param_vtys, param_effects, ret_vty);
    let (mut words, sret) = begin_user_call(c, bcx, frame, &abi, ret_vty)?;
    let mut temporaries = Vec::new();
    if args.len() != param_vtys.len() {
        invariant_violation("user call argument 数量与 function ABI 漂移")
    }
    for (index, (a, pt)) in args.iter().zip(param_vtys).enumerate() {
        let pass = a
            .pass
            .as_ref()
            .unwrap_or_else(|| invariant_violation("user call argument 缺少 resolved pass"));
        let (machine_index, passing) = abi.parameter(index);
        if machine_index != words.len() {
            invariant_violation("user call explicit parameter machine index 漂移")
        }
        let (word, temporary) = emit_user_argument(
            c,
            bcx,
            frame,
            &a.value,
            pass,
            pt,
            passing,
        )?;
        words.push(word);
        if let Some(temporary) = temporary {
            temporaries.push(temporary);
        }
    }
    let code = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), clo, CLOSURE_CODE_OFFSET);
    let env = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), clo, CLOSURE_ENV_OFFSET);
    if abi.env_index() + 1 > words.len() {
        invariant_violation("user call hidden prefix 不完整")
    }
    words[abi.env_index()] = env;
    let sig_ref = bcx.func.import_signature(abi.signature().clone());
    let inst = bcx.ins().call_indirect(sig_ref, code, &words);
    let result = finish_user_call(c, bcx, inst, &abi, ret_vty, sret)?;
    cleanup_call_temporaries(c, bcx, temporaries)?;
    Ok(result)
}

fn begin_user_call<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    abi: &UserFunctionAbi,
    ret_vty: &VTy,
) -> AliasResult<(Vec<Value>, Option<Value>)> {
    let mut words = Vec::with_capacity(abi.signature().params.len());
    let sret = if abi.result() == UserReturnPassing::ExplicitSRet {
        let area = allocate_value_cell(c, bcx, ret_vty)?;
        if abi.sret_index() != Some(words.len()) {
            invariant_violation("ExplicitSRet hidden parameter index 漂移")
        }
        words.push(area);
        Some(area)
    } else {
        None
    };
    if abi.globals_index() != words.len() {
        invariant_violation("globals hidden parameter index 漂移")
    }
    words.push(bcx.use_var(frame.globals));
    if abi.env_index() != words.len() {
        invariant_violation("closure env hidden parameter index 漂移")
    }
    // Direct calls replace this null with their actual environment after loading the closure;
    // named methods intentionally keep null because they cannot capture lexical storage.
    words.push(bcx.ins().iconst(types::I64, 0));
    Ok((words, sret))
}

fn finish_user_call<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    inst: cranelift_codegen::ir::Inst,
    abi: &UserFunctionAbi,
    ret_vty: &VTy,
    sret: Option<Value>,
) -> AliasResult<ExprValue> {
    match abi.result() {
        UserReturnPassing::Unit => Ok(ExprValue::scalar(bcx.ins().iconst(types::I64, 0))),
        UserReturnPassing::Direct(_) => {
            let raw = first_result(bcx, inst);
            Ok(ExprValue::scalar(norm_load(bcx, raw, ret_vty)))
        }
        UserReturnPassing::ExplicitSRet => {
            let area = sret.unwrap_or_else(|| {
                invariant_violation("ExplicitSRet call 缺少 caller-owned return area")
            });
            let value = ExprValue::load(bcx, area, 0, ret_vty);
            c.call_rt_void(bcx, "rt.heap.free", &[area])?;
            Ok(value)
        }
    }
}

struct CallTemporary<'a> {
    cell: Value,
    vty: VTy,
    destroy_plan: Option<&'a DestroyPlan>,
}

fn cleanup_call_temporaries<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    temporaries: Vec<CallTemporary<'_>>,
) -> AliasResult<()> {
    for temporary in temporaries.into_iter().rev() {
        if let Some(plan) = temporary.destroy_plan {
            let value = ExprValue::load(bcx, temporary.cell, 0, &temporary.vty);
            emit_destroy_value(c, bcx, value, temporary.vty, plan)?;
        }
        c.call_rt_void(bcx, "rt.heap.free", &[temporary.cell])?;
    }
    Ok(())
}

fn emit_user_argument<'a, M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    value: &Expr,
    pass: &'a ArgumentPass,
    vty: &VTy,
    passing: UserParameterPassing,
) -> AliasResult<(Value, Option<CallTemporary<'a>>)> {
    match (pass, passing) {
        (ArgumentPass::Inline | ArgumentPass::Owned, UserParameterPassing::Direct(_)) => {
            let value = emit_expr(c, bcx, frame, value)?;
            let value = value.into_scalar("direct user argument 必须是单 lane value");
            Ok((norm_store(bcx, value, vty), None))
        }
        (
            ArgumentPass::Inline | ArgumentPass::Owned,
            UserParameterPassing::IndirectByValue,
        ) => {
            let value = emit_expr(c, bcx, frame, value)?;
            let cell = emit_temporary_cell(c, bcx, value, vty)?;
            Ok((
                cell,
                Some(CallTemporary {
                    cell,
                    vty: vty.clone(),
                    destroy_plan: None,
                }),
            ))
        }
        (
            ArgumentPass::ReadBorrow { source, .. }
            | ArgumentPass::WriteBorrow { source, .. },
            UserParameterPassing::BorrowedAddress,
        ) => {
            let (address, source_vty) = emit_place_addr(c, bcx, frame, source)?;
            if source_vty != *vty {
                invariant_violation("borrow argument source ABI 与 parameter ABI 漂移")
            }
            Ok((address, None))
        }
        (
            ArgumentPass::BorrowTemporary { kind, destroy_plan },
            UserParameterPassing::BorrowedAddress,
        ) => {
            let _ = kind;
            let value = emit_expr(c, bcx, frame, value)?;
            let cell = emit_temporary_cell(c, bcx, value, vty)?;
            Ok((
                cell,
                Some(CallTemporary {
                    cell,
                    vty: vty.clone(),
                    destroy_plan: Some(destroy_plan),
                }),
            ))
        }
        _ => invariant_violation("resolved argument pass 与 canonical function ABI 漂移"),
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
        let v = emit_container_value(c, bcx, frame, expr)?;
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
    let payload = emit_container_value(c, bcx, frame, &arg.value)?;
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
) -> AliasResult<ExprValue> {
    let (recv, receiver_pass) = receiver;
    let svt = c.vty(recv.ty());
    if let MethodTarget::User {
        receiver,
        id: method_id,
        ..
    } = target
    {
        let receiver_vty = c.vty(receiver);
        if receiver_vty != svt {
            invariant_violation("已解析方法接收者与表达式静态类型一致")
        }
        let receiver_pass = receiver_pass
            .unwrap_or_else(|| invariant_violation("user method receiver 缺少 resolved pass"));
        return emit_user_method_call(
            c,
            bcx,
            frame,
            recv,
            receiver_pass,
            args,
            *method_id,
        );
    }
    if matches!(target, MethodTarget::ArrayIterator) {
        let value = match receiver_pass {
            Some(ArgumentPass::ReadBorrow { source, .. }) => {
                emit_place_value(c, bcx, frame, source)?.0
            }
            Some(ArgumentPass::BorrowTemporary {
                kind: BorrowKind::Read,
                ..
            }) => emit_expr(c, bcx, frame, recv)?,
            _ => invariant_violation("iterator receiver 缺少 resolved read pass"),
        };
        let array = value.into_scalar("iterator receiver 必须是 array root");
        return make_iterator(c, bcx, array).map(ExprValue::scalar);
    }
    if receiver_pass.is_some() {
        invariant_violation("builtin method receiver 携带 user pass")
    }
    let rv = emit_expr(c, bcx, frame, recv)?
        .into_scalar("builtin method receiver 收到 multi-lane expression value");

    match target {
        MethodTarget::Numeric(op) => {
            let [arg] = args else {
                invariant_violation("算术扩展函数元数 (sema 已校验)")
            };
            let r = emit_expr(c, bcx, frame, &arg.value)?;
            let r = r.into_scalar("numeric method argument 收到 multi-lane expression value");
            emit_binary_values(c, bcx, (*op, &svt, rv, r, span)).map(ExprValue::scalar)
        }
        MethodTarget::BoolNot => {
            if !args.is_empty() {
                invariant_violation("not 扩展函数元数 (sema 已校验)");
            }
            let b = bcx.ins().icmp_imm_s(IntCC::Equal, rv, 0);
            Ok(ExprValue::scalar(bcx.ins().uextend(types::I64, b)))
        }
        MethodTarget::StringLen => {
            let t = c.call_rt(bcx, "alias.str.len", &[rv])?;
            Ok(ExprValue::scalar(bcx.ins().sextend(types::I64, t)))
        }
        MethodTarget::StringUpper => c
            .call_rt(bcx, "alias.str.upper", &[rv])
            .map(ExprValue::scalar),
        MethodTarget::StringLower => c
            .call_rt(bcx, "alias.str.lower", &[rv])
            .map(ExprValue::scalar),
        MethodTarget::StringTrim => c
            .call_rt(bcx, "alias.str.trim", &[rv])
            .map(ExprValue::scalar),
        MethodTarget::ArrayLen => {
            let VTy::Array(_) = &svt else {
                invariant_violation("array.len 目标必须保留数组类型")
            };
            let raw = array_raw(bcx, rv);
            let t = c.call_rt(bcx, "alias.arr.len", &[raw])?;
            Ok(ExprValue::scalar(bcx.ins().sextend(types::I64, t)))
        }
        MethodTarget::ArrayPush => {
            let VTy::Array(elem) = &svt else {
                invariant_violation("array.push 目标必须保留数组类型")
            };
            let [arg] = args else {
                invariant_violation("push 元数 (sema 已校验)")
            };
            let value = emit_container_value(c, bcx, frame, &arg.value)?;
            let raw = array_raw(bcx, rv);
            let slot = c.call_rt(bcx, "alias.arr.push", &[raw])?;
            value.store(bcx, slot, 0, elem);
            bump_array_version(bcx, rv);
            Ok(ExprValue::scalar(bcx.ins().iconst(types::I64, 0)))
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
            let value = ExprValue::load(bcx, slot, 0, elem);
            bump_array_version(bcx, rv);
            Ok(value)
        }
        MethodTarget::ArrayIterator => invariant_violation("iterator 必须消费 resolved read pass"),
        MethodTarget::User { .. } => invariant_violation("user method 必须走 canonical ABI 分支"),
    }
}

fn emit_user_method_call<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    receiver: &Expr,
    receiver_pass: &ArgumentPass,
    args: &[CallArg],
    method_id: crate::sema::hir::MethodId,
) -> AliasResult<ExprValue> {
    let (param_vtys, param_effects, ret_vty) = c
        .method_sigs
        .get(&method_id)
        .cloned()
        .unwrap_or_else(|| invariant_violation("MethodId 必须存在于方法签名表"));
    if param_vtys.len() != args.len() + 1 {
        invariant_violation("user method receiver/argument 数量与 function ABI 漂移")
    }
    let abi = user_function_abi(c.cc, &param_vtys, &param_effects, &ret_vty);
    let (mut words, sret) = begin_user_call(c, bcx, frame, &abi, &ret_vty)?;
    let mut temporaries = Vec::new();

    let (receiver_index, receiver_passing) = abi.parameter(0);
    if receiver_index != words.len() {
        invariant_violation("user method receiver machine index 漂移")
    }
    let (receiver_word, receiver_temporary) = emit_user_argument(
        c,
        bcx,
        frame,
        receiver,
        receiver_pass,
        &param_vtys[0],
        receiver_passing,
    )?;
    words.push(receiver_word);
    if let Some(temporary) = receiver_temporary {
        temporaries.push(temporary);
    }
    for (offset, (arg, param)) in args.iter().zip(param_vtys.iter().skip(1)).enumerate() {
        let index = offset + 1;
        let pass = arg
            .pass
            .as_ref()
            .unwrap_or_else(|| invariant_violation("user method argument 缺少 resolved pass"));
        let (machine_index, passing) = abi.parameter(index);
        if machine_index != words.len() {
            invariant_violation("user method argument machine index 漂移")
        }
        let (word, temporary) = emit_user_argument(
            c,
            bcx,
            frame,
            &arg.value,
            pass,
            param,
            passing,
        )?;
        words.push(word);
        if let Some(temporary) = temporary {
            temporaries.push(temporary);
        }
    }
    let fid = *c
        .methods
        .get(&method_id)
        .unwrap_or_else(|| invariant_violation("MethodId 必须存在函数 ID"));
    let fref = c.module.declare_func_in_func(fid, bcx.func);
    let inst = bcx.ins().call(fref, &words);
    let result = finish_user_call(c, bcx, inst, &abi, &ret_vty, sret)?;
    cleanup_call_temporaries(c, bcx, temporaries)?;
    Ok(result)
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
