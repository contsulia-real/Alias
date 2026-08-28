use super::expr::emit_expr;
use super::ops::new_span_id;
use crate::codegen::abi::{storage_word, store_elem, VTy, VALUE_WORD_BYTES};
use crate::codegen::layout::{
    ARRAY_DATA_OFFSET, ARRAY_LEN_OFFSET, ARRAY_WRAPPER_RAW_OFFSET, ARRAY_WRAPPER_VERSION_OFFSET,
    ARRAY_WRAPPER_WORDS, ITERATOR_ARRAY_OFFSET, ITERATOR_INDEX_OFFSET, ITERATOR_VERSION_OFFSET,
    ITERATOR_WORDS,
};
use crate::codegen::{Compiler, Frame};
use crate::sema::hir::Expr;
use crate::{AliasResult, Span};
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, TrapCode, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;

pub(crate) fn array_raw(bcx: &mut FunctionBuilder, array: Value) -> Value {
    bcx.ins().load(
        types::I64,
        MemFlagsData::new(),
        array,
        ARRAY_WRAPPER_RAW_OFFSET,
    )
}

pub(crate) fn array_version(bcx: &mut FunctionBuilder, array: Value) -> Value {
    bcx.ins().load(
        types::I64,
        MemFlagsData::new(),
        array,
        ARRAY_WRAPPER_VERSION_OFFSET,
    )
}

pub(crate) fn bump_array_version(bcx: &mut FunctionBuilder, array: Value) {
    let old = array_version(bcx, array);
    let next = bcx.ins().iadd_imm_s(old, 1);
    bcx.ins().store(
        MemFlagsData::new(),
        next,
        array,
        ARRAY_WRAPPER_VERSION_OFFSET,
    );
}

pub(crate) fn wrap_array<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    raw: Value,
) -> AliasResult<Value> {
    let words = bcx.ins().iconst(types::I32, ARRAY_WRAPPER_WORDS);
    let wrapper = c.call_rt(bcx, "alias.env.new", &[words])?;
    let zero = bcx.ins().iconst(types::I64, 0);
    bcx.ins()
        .store(MemFlagsData::new(), raw, wrapper, ARRAY_WRAPPER_RAW_OFFSET);
    bcx.ins().store(
        MemFlagsData::new(),
        zero,
        wrapper,
        ARRAY_WRAPPER_VERSION_OFFSET,
    );
    Ok(wrapper)
}

pub(crate) fn make_iterator<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    array: Value,
) -> AliasResult<Value> {
    let words = bcx.ins().iconst(types::I32, ITERATOR_WORDS);
    let iter = c.call_rt(bcx, "alias.env.new", &[words])?;
    let zero = bcx.ins().iconst(types::I64, 0);
    let version = array_version(bcx, array);
    bcx.ins()
        .store(MemFlagsData::new(), array, iter, ITERATOR_ARRAY_OFFSET);
    bcx.ins()
        .store(MemFlagsData::new(), zero, iter, ITERATOR_INDEX_OFFSET);
    bcx.ins()
        .store(MemFlagsData::new(), version, iter, ITERATOR_VERSION_OFFSET);
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

pub(crate) fn emit_array_lit<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    elems: &[Expr],
    elem_vty: &VTy,
) -> AliasResult<Value> {
    let n = elems.len() as i64;
    let cap = bcx.ins().iconst(types::I32, n);
    let raw = c.call_rt(bcx, "alias.arr.new", &[cap])?;
    for (i, el) in elems.iter().enumerate() {
        let v = emit_expr(c, bcx, frame, el)?;
        let dp = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), raw, ARRAY_DATA_OFFSET);
        let addr = bcx.ins().iadd_imm_s(dp, (i as i64) * VALUE_WORD_BYTES);
        let sv = storage_word(bcx, v, elem_vty);
        store_elem(bcx, sv, addr, elem_vty);
    }
    let lenw = bcx.ins().iconst(types::I64, n);
    bcx.ins()
        .store(MemFlagsData::new(), lenw, raw, ARRAY_LEN_OFFSET);
    wrap_array(c, bcx, raw)
}
