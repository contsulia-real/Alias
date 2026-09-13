//! Canonical provenance descriptors for address-taken local storage.
//!
//! Sema freezes the exact Refer source and address-taken root set. This module only materializes
//! their physical descriptor and four pointer lanes; it does not infer ownership or loan facts.

use super::cells::{allocate_value_cell, binding_storage_addr};
use super::destruction::register_plain_allocation_cleanup;
use super::value::ExprValue;
use crate::codegen::abi::{value_layout, VTy};
use crate::codegen::layout::{
    STORAGE_DESCRIPTOR_BASE_OFFSET, STORAGE_DESCRIPTOR_BYTES, STORAGE_DESCRIPTOR_EXTENT_OFFSET,
    STORAGE_DESCRIPTOR_KIND_OFFSET, STORAGE_DESCRIPTOR_RAW_METADATA_OFFSET, STORAGE_KIND_LOCAL,
};
use crate::codegen::{invariant_violation, Compiler, Frame};
use crate::sema::hir::{BindingId, Place};
use crate::AliasResult;
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::{FunctionBuilder, Variable};
use cranelift_module::Module;

pub(super) fn register_address_taken_local<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    binding: BindingId,
    storage: Value,
    vty: &VTy,
) -> AliasResult<()> {
    if !c.address_taken_roots.contains(&binding) {
        return Ok(());
    }
    let layout = value_layout(vty);
    if layout.size != layout.stride {
        invariant_violation("address-taken local 的 value size/stride 尚未统一")
    }
    let bytes = bcx.ins().iconst(types::I64, STORAGE_DESCRIPTOR_BYTES);
    let descriptor = c.call_rt(bcx, "alias.cell.new", &[bytes])?;
    let extent = bcx.ins().iconst(types::I64, layout.stride as i64);
    let kind = bcx.ins().iconst(types::I64, STORAGE_KIND_LOCAL);
    let none = bcx.ins().iconst(types::I64, 0);
    bcx.ins().store(
        MemFlagsData::new(),
        storage,
        descriptor,
        STORAGE_DESCRIPTOR_BASE_OFFSET,
    );
    bcx.ins().store(
        MemFlagsData::new(),
        extent,
        descriptor,
        STORAGE_DESCRIPTOR_EXTENT_OFFSET,
    );
    bcx.ins().store(
        MemFlagsData::new(),
        kind,
        descriptor,
        STORAGE_DESCRIPTOR_KIND_OFFSET,
    );
    bcx.ins().store(
        MemFlagsData::new(),
        none,
        descriptor,
        STORAGE_DESCRIPTOR_RAW_METADATA_OFFSET,
    );
    let descriptor_var = bcx.declare_var(types::I64);
    bcx.def_var(descriptor_var, descriptor);
    if frame
        .storage_descriptors
        .last_mut()
        .unwrap_or_else(|| invariant_violation("descriptor scope 栈非空"))
        .insert(binding, descriptor_var)
        .is_some()
    {
        invariant_violation("同一 local storage root 被重复登记 descriptor")
    }
    register_plain_allocation_cleanup(bcx, frame, descriptor);
    Ok(())
}

fn descriptor_for(frame: &Frame, binding: BindingId) -> Variable {
    frame
        .storage_descriptors
        .iter()
        .rev()
        .find_map(|scope| scope.get(&binding).copied())
        .unwrap_or_else(|| invariant_violation("Refer source 缺少 canonical local descriptor"))
}

pub(super) fn emit_refer<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    source: &Place,
    result_vty: &VTy,
) -> AliasResult<ExprValue> {
    let Place::Local { binding_id, .. } = source else {
        invariant_violation("Refer subplace 必须被 final HIR gate 拒绝")
    };
    let address = binding_storage_addr(c, bcx, frame, *binding_id)
        .unwrap_or_else(|| invariant_violation("Refer source BindingId 无 storage"));
    let source_vty = c.vty(source.ty());
    let layout = value_layout(&source_vty);
    let descriptor = bcx.use_var(descriptor_for(frame, *binding_id));
    let view_end = bcx.ins().iadd_imm_s(address, layout.stride as i64);
    let value = ExprValue::pointer([descriptor, address, address, view_end]);
    let cell = allocate_value_cell(c, bcx, result_vty)?;
    value.store(bcx, cell, 0, result_vty);
    register_plain_allocation_cleanup(bcx, frame, cell);
    Ok(ExprValue::scalar(cell))
}
