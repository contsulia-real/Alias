use super::*;

pub(crate) fn array_raw(bcx: &mut FunctionBuilder, array: Value) -> Value {
    bcx.ins().load(types::I64, MemFlagsData::new(), array, 0)
}

pub(crate) fn array_version(bcx: &mut FunctionBuilder, array: Value) -> Value {
    bcx.ins().load(types::I64, MemFlagsData::new(), array, 8)
}

pub(crate) fn bump_array_version(bcx: &mut FunctionBuilder, array: Value) {
    let old = array_version(bcx, array);
    let next = bcx.ins().iadd_imm_s(old, 1);
    bcx.ins().store(MemFlagsData::new(), next, array, 8);
}

pub(crate) fn wrap_array<M: Module>(
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

pub(crate) fn make_iterator<M: Module>(
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

pub(crate) fn emit_iterator_abort<M: Module>(
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

pub(crate) fn emit_array_lit_typed<M: Module>(
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
