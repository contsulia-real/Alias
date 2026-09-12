use super::expr::emit_container_value;
use super::ops::{emit_index_guard, emit_runtime_abort};
use crate::codegen::abi::{value_layout, VTy};
use crate::codegen::layout::{
    ARRAY_DATA_OFFSET, ARRAY_LEN_OFFSET, ARRAY_STRIDE_OFFSET, ARRAY_WRAPPER_RAW_OFFSET,
    ARRAY_WRAPPER_VERSION_OFFSET, ARRAY_WRAPPER_WORDS, ITERATOR_ARRAY_OFFSET,
    ITERATOR_INDEX_OFFSET, ITERATOR_VERSION_OFFSET, ITERATOR_WORDS,
};
use crate::codegen::{Compiler, Frame};
use crate::sema::hir::{Expr, RuntimeCheckRequirement};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, Value};
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

pub(super) fn array_len(bcx: &mut FunctionBuilder, raw: Value) -> Value {
    bcx.ins()
        .load(types::I64, MemFlagsData::new(), raw, ARRAY_LEN_OFFSET)
}

/// raw array header 保存创建时由 canonical ValueLayout 固化的 element stride。所有 emitter
/// 对 backing element 的寻址必须经过这里；调用点自行重算或写死 8-byte 步长会让扩容复制、
/// index 与 iterator 对同一 backing 产生不同解释。
pub(super) fn array_element_addr(bcx: &mut FunctionBuilder, raw: Value, index: Value) -> Value {
    let data = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), raw, ARRAY_DATA_OFFSET);
    let stride = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), raw, ARRAY_STRIDE_OFFSET);
    let offset = bcx.ins().imul(index, stride);
    bcx.ins().iadd(data, offset)
}

/// 用户可控 array index 的 resolved bounds contract + backing address 唯一入口。
///
/// Sema has already frozen whether the guard is required. Reconstructing that decision from the
/// receiver syntax here would let pointer/raw-memory checks drift into a second backend policy.
/// Internal for/clone traversals remain on `array_element_addr` because their loop invariant is a
/// different proof owner.
pub(super) fn resolved_array_element_addr<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    array: Value,
    index_word: Value,
    bounds_check: RuntimeCheckRequirement,
    span: Span,
) -> AliasResult<Value> {
    let raw = array_raw(bcx, array);
    let idx32 = bcx.ins().ireduce(types::I32, index_word);
    if bounds_check == RuntimeCheckRequirement::Required {
        let len64 = array_len(bcx, raw);
        let len32 = bcx.ins().ireduce(types::I32, len64);
        emit_index_guard(c, bcx, idx32, len32, span)?;
    }
    let idx64 = bcx.ins().sextend(types::I64, idx32);
    Ok(array_element_addr(bcx, raw, idx64))
}

pub(crate) fn array_version(bcx: &mut FunctionBuilder, array: Value) -> Value {
    bcx.ins().load(
        types::I64,
        MemFlagsData::new(),
        array,
        ARRAY_WRAPPER_VERSION_OFFSET,
    )
}

/// 版本属于共享 wrapper，而不是可能在扩容时替换的 raw backing store；因此所有别名
/// 和既有 iterator 都观察同一计数。push/pop 成功后必须调用，遗漏会让结构修改绕过
/// for 的 fail-fast 检查，把旧 cursor 继续用于已变化的集合。
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
    emit_runtime_abort(c, bcx, "alias.abort_iter", span)
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
    let stride = bcx
        .ins()
        .iconst(types::I64, value_layout(elem_vty).stride as i64);
    let raw = c.call_rt(bcx, "alias.arr.new", &[cap, stride])?;
    for (i, el) in elems.iter().enumerate() {
        let v = emit_container_value(c, bcx, frame, el)?;
        let index = bcx.ins().iconst(types::I64, i as i64);
        let addr = array_element_addr(bcx, raw, index);
        v.store(bcx, addr, 0, elem_vty);
    }
    let lenw = bcx.ins().iconst(types::I64, n);
    bcx.ins()
        .store(MemFlagsData::new(), lenw, raw, ARRAY_LEN_OFFSET);
    wrap_array(c, bcx, raw)
}
