use super::expr::emit_expr_expected;
use super::ops::new_span_id;
use super::*;

pub(crate) fn array_raw(bcx: &mut FunctionBuilder, array: Value) -> Value {
    bcx.ins().load(types::I64, MemFlagsData::new(), array, 0)
}

pub(crate) fn array_version(bcx: &mut FunctionBuilder, array: Value) -> Value {
    bcx.ins()
        .load(types::I64, MemFlagsData::new(), array, value_word_offset(1))
}

pub(crate) fn bump_array_version(bcx: &mut FunctionBuilder, array: Value) {
    let old = array_version(bcx, array);
    let next = bcx.ins().iadd_imm_s(old, 1);
    bcx.ins()
        .store(MemFlagsData::new(), next, array, value_word_offset(1));
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
    bcx.ins()
        .store(MemFlagsData::new(), zero, wrapper, value_word_offset(1));
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
    bcx.ins()
        .store(MemFlagsData::new(), zero, iter, value_word_offset(1));
    bcx.ins()
        .store(MemFlagsData::new(), version, iter, value_word_offset(2));
    Ok(iter)
}

pub(crate) fn emit_iterator_abort<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    span: Span,
) -> AliasResult<()> {
    let span_id = new_span_id(c, span);
    let aid = bcx.ins().iconst(types::I32, span_id as i64);
    c.call_rt_void(bcx, "alias.abort_iter", &[aid])?;
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
    let eszw = bcx.ins().iconst(types::I32, VALUE_WORD_BYTES);
    let raw = c.call_rt(bcx, "alias.arr.new", &[cap, eszw])?;
    for (i, el) in elems.iter().enumerate() {
        let v = emit_expr_expected(c, bcx, frame, el, elem_vty)?;
        let dp = bcx.ins().load(types::I64, MemFlagsData::new(), raw, 0);
        let addr = bcx.ins().iadd_imm_s(dp, (i as i64) * VALUE_WORD_BYTES);
        let sv = storage_word(bcx, v, elem_vty);
        store_elem(bcx, sv, addr, elem_vty);
    }
    let lenw = bcx.ins().iconst(types::I64, n);
    bcx.ins()
        .store(MemFlagsData::new(), lenw, raw, value_word_offset(1));
    wrap_array(c, bcx, raw)
}
