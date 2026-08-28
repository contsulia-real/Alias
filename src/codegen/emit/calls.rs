use super::*;

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

pub(crate) fn emit_result_ctor_typed<M: Module>(
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
