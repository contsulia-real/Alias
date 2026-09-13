//! Canonical provenance descriptors for address-taken local and global storage.
//!
//! Sema freezes the exact Refer source and address-taken root set. This module only materializes
//! their physical descriptor and four pointer lanes; it does not infer ownership or loan facts.

use super::cells::{allocate_value_cell, binding_storage_addr};
use super::destruction::register_plain_allocation_cleanup;
use super::value::ExprValue;
use crate::codegen::abi::{value_layout, PtrLane, VTy};
use crate::codegen::layout::{
    STORAGE_DESCRIPTOR_BASE_OFFSET, STORAGE_DESCRIPTOR_BYTES, STORAGE_DESCRIPTOR_EXTENT_OFFSET,
    STORAGE_DESCRIPTOR_KIND_OFFSET, STORAGE_DESCRIPTOR_RAW_METADATA_OFFSET, STORAGE_KIND_GLOBAL,
    STORAGE_KIND_LOCAL,
};
use crate::codegen::{bound_vty, invariant_violation, Compiler, Frame};
use crate::sema::hir::{BinOp, BindingId, Place, RuntimeCheckRequirement};
use crate::sema::types::Ty;
use crate::AliasResult;
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;
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
    initialize_descriptor(bcx, descriptor, storage, layout.stride, STORAGE_KIND_LOCAL);
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

fn initialize_descriptor(
    bcx: &mut FunctionBuilder,
    descriptor: Value,
    storage: Value,
    extent: usize,
    storage_kind: i64,
) {
    let extent = bcx.ins().iconst(types::I64, extent as i64);
    let kind = bcx.ins().iconst(types::I64, storage_kind);
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
}

pub(in crate::codegen) fn initialize_address_taken_global<M: Module>(
    c: &Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    binding: BindingId,
) -> AliasResult<()> {
    let Some(offset) = c.global_descriptor_offsets.get(&binding).copied() else {
        if c.address_taken_roots.contains(&binding) {
            invariant_violation("address-taken global 缺少 static descriptor offset")
        }
        return Ok(());
    };
    let base = bcx.use_var(frame.globals);
    let descriptor = bcx.ins().iadd_imm_s(base, offset as i64);
    let storage = binding_storage_addr(c, bcx, frame, binding)
        .unwrap_or_else(|| invariant_violation("address-taken global BindingId 无 storage"));
    let layout = value_layout(&bound_vty(c, frame, binding));
    if layout.size != layout.stride {
        invariant_violation("address-taken global 的 value size/stride 尚未统一")
    }
    initialize_descriptor(bcx, descriptor, storage, layout.stride, STORAGE_KIND_GLOBAL);
    Ok(())
}

fn descriptor_for<M: Module>(
    c: &Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    binding: BindingId,
) -> Value {
    if let Some(descriptor) = frame
        .storage_descriptors
        .iter()
        .rev()
        .find_map(|scope| scope.get(&binding).copied())
    {
        return bcx.use_var(descriptor);
    }
    if let Some(offset) = c.global_descriptor_offsets.get(&binding).copied() {
        let base = bcx.use_var(frame.globals);
        return bcx.ins().iadd_imm_s(base, offset as i64);
    }
    invariant_violation("Refer source 缺少 canonical local/global descriptor")
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
    let descriptor = descriptor_for(c, bcx, frame, *binding_id);
    let view_end = bcx.ins().iadd_imm_s(address, layout.stride as i64);
    let value = ExprValue::pointer([descriptor, address, address, view_end]);
    let cell = allocate_value_cell(c, bcx, result_vty)?;
    value.store(bcx, cell, 0, result_vty);
    register_plain_allocation_cleanup(bcx, frame, cell);
    Ok(ExprValue::scalar(cell))
}

pub(super) fn emit_pointer_offset<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    input: (
        BinOp,
        &ExprValue,
        &VTy,
        Value,
        &Ty,
        Option<RuntimeCheckRequirement>,
        crate::Span,
    ),
) -> AliasResult<ExprValue> {
    let (op, pointer, pointer_vty, offset, offset_ty, check, span) = input;
    if !matches!(op, BinOp::Add | BinOp::Sub) {
        invariant_violation("pointer offset emitter 只接受加减")
    }
    let VTy::Ptr { pointee, nullable: false } = pointer_vty else {
        invariant_violation("pointer arithmetic 需要 non-null pointer")
    };
    let provenance = pointer.pointer_lane(bcx, pointer_vty, PtrLane::Provenance);
    let address = pointer.pointer_lane(bcx, pointer_vty, PtrLane::Address);
    let view_start = pointer.pointer_lane(bcx, pointer_vty, PtrLane::ViewStart);
    let view_end = pointer.pointer_lane(bcx, pointer_vty, PtrLane::ViewEnd);
    let stride = i64::try_from(value_layout(pointee).stride)
        .unwrap_or_else(|_| invariant_violation("pointer arithmetic stride 超出 i64"));
    let stride = bcx.ins().iconst(types::I64, stride);
    let (scaled, scale_overflow, signed) = match offset_ty {
        Ty::Int(_) => {
            let (scaled, overflow) = bcx.ins().smul_overflow(offset, stride);
            (scaled, overflow, true)
        }
        Ty::UInt(_) => {
            let (scaled, overflow) = bcx.ins().umul_overflow(offset, stride);
            (scaled, overflow, false)
        }
        _ => invariant_violation("pointer arithmetic offset 不是整数"),
    };
    let (new_address, address_overflow) = if signed {
        use cranelift_codegen::ir::condcodes::IntCC;
        let negative = bcx.ins().icmp_imm_s(IntCC::SignedLessThan, scaled, 0);
        let magnitude = bcx.ins().ineg(scaled);
        let (positive_value, positive_overflow) = if op == BinOp::Add {
            bcx.ins().uadd_overflow(address, scaled)
        } else {
            bcx.ins().usub_overflow(address, scaled)
        };
        let (negative_value, negative_overflow) = if op == BinOp::Add {
            bcx.ins().usub_overflow(address, magnitude)
        } else {
            bcx.ins().uadd_overflow(address, magnitude)
        };
        (
            bcx.ins().select(negative, negative_value, positive_value),
            bcx.ins().select(negative, negative_overflow, positive_overflow),
        )
    } else if op == BinOp::Add {
        bcx.ins().uadd_overflow(address, scaled)
    } else {
        bcx.ins().usub_overflow(address, scaled)
    };
    match check {
        Some(RuntimeCheckRequirement::Required) => {
            let overflow = bcx.ins().bor(scale_overflow, address_overflow);
            super::ops::emit_abort_branch(
                c, bcx, overflow, "alias.abort_ptr_arithmetic", span,
            )?;
            use cranelift_codegen::ir::condcodes::IntCC;
            let before_start = bcx.ins().icmp(IntCC::UnsignedLessThan, new_address, view_start);
            let after_end = bcx.ins().icmp(IntCC::UnsignedGreaterThan, new_address, view_end);
            let outside = bcx.ins().bor(before_start, after_end);
            super::ops::emit_abort_branch(c, bcx, outside, "alias.abort_ptr_bounds", span)?;
        }
        Some(RuntimeCheckRequirement::Proven) => {}
        None => invariant_violation("pointer arithmetic 缺少 runtime-check fact"),
    }
    let value = ExprValue::pointer([provenance, new_address, view_start, view_end]);
    let cell = allocate_value_cell(c, bcx, pointer_vty)?;
    value.store(bcx, cell, 0, pointer_vty);
    register_plain_allocation_cleanup(bcx, frame, cell);
    Ok(ExprValue::scalar(cell))
}
