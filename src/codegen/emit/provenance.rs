//! Canonical provenance descriptors for address-taken local and global storage.
//!
//! Sema freezes the exact Refer source and address-taken root set. This module only materializes
//! their physical descriptor and four pointer lanes; it does not infer ownership or loan facts.

use super::cells::{allocate_value_cell, binding_storage_addr};
use super::destruction::register_plain_allocation_cleanup;
use super::places::emit_place_addr;
use super::value::ExprValue;
use crate::codegen::abi::{value_layout, PtrLane, VTy};
use crate::codegen::layout::{
    ARRAY_CAP_OFFSET, ARRAY_DATA_OFFSET, ARRAY_STRIDE_OFFSET, ARRAY_WRAPPER_RAW_OFFSET,
    STORAGE_DESCRIPTOR_BASE_OFFSET, STORAGE_DESCRIPTOR_BYTES, STORAGE_DESCRIPTOR_EXTENT_OFFSET,
    STORAGE_DESCRIPTOR_KIND_OFFSET, STORAGE_DESCRIPTOR_RAW_METADATA_OFFSET, STORAGE_KIND_GLOBAL,
    STORAGE_KIND_LOCAL,
};
use crate::codegen::{bound_vty, invariant_violation, Compiler, Frame};
use crate::sema::hir::{
    BinOp, BindingId, DescriptorRegion, DescriptorRoot, Place, RuntimeCheckRequirement,
};
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
    for region in [DescriptorRegion::Cell, DescriptorRegion::Object] {
        let root = DescriptorRoot { binding, region };
        if !c.address_taken_roots.contains(&root) {
            continue;
        }
        let bytes = bcx.ins().iconst(types::I64, STORAGE_DESCRIPTOR_BYTES);
        let descriptor = c.call_rt(bcx, "alias.cell.new", &[bytes])?;
        let (base, extent) = descriptor_storage(c, bcx, storage, vty, region);
        initialize_descriptor(bcx, descriptor, base, extent, STORAGE_KIND_LOCAL);
        let descriptor_var = bcx.declare_var(types::I64);
        bcx.def_var(descriptor_var, descriptor);
        if frame
            .storage_descriptors
            .last_mut()
            .unwrap_or_else(|| invariant_violation("descriptor scope 栈非空"))
            .insert(root, descriptor_var)
            .is_some()
        {
            invariant_violation("同一 local storage root 被重复登记 descriptor")
        }
        register_plain_allocation_cleanup(bcx, frame, descriptor);
    }
    Ok(())
}

fn descriptor_storage<M: Module>(
    c: &Compiler<M>,
    bcx: &mut FunctionBuilder,
    storage: Value,
    vty: &VTy,
    region: DescriptorRegion,
) -> (Value, Value) {
    match region {
        DescriptorRegion::Cell => {
            let layout = value_layout(vty);
            if layout.size != layout.stride {
                invariant_violation("address-taken cell 的 value size/stride 尚未统一")
            }
            (storage, bcx.ins().iconst(types::I64, layout.stride as i64))
        }
        DescriptorRegion::Object => {
            let object = bcx.ins().load(types::I64, MemFlagsData::new(), storage, 0);
            match vty {
                VTy::Struct(name) => {
                    let layout = c.struct_layouts.get(name).unwrap_or_else(|| {
                        invariant_violation("address-taken struct 缺少物理布局")
                    });
                    (object, bcx.ins().iconst(types::I64, layout.size as i64))
                }
                VTy::Array(_) => {
                    let raw = bcx.ins().load(
                        types::I64,
                        MemFlagsData::new(),
                        object,
                        ARRAY_WRAPPER_RAW_OFFSET,
                    );
                    let data = bcx.ins().load(types::I64, MemFlagsData::new(), raw, ARRAY_DATA_OFFSET);
                    let cap = bcx.ins().load(types::I64, MemFlagsData::new(), raw, ARRAY_CAP_OFFSET);
                    let stride = bcx.ins().load(types::I64, MemFlagsData::new(), raw, ARRAY_STRIDE_OFFSET);
                    (data, bcx.ins().imul(cap, stride))
                }
                _ => invariant_violation("object descriptor 只对应 struct/array root"),
            }
        }
    }
}

fn initialize_descriptor(
    bcx: &mut FunctionBuilder,
    descriptor: Value,
    storage: Value,
    extent: Value,
    storage_kind: i64,
) {
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
    for region in [DescriptorRegion::Cell, DescriptorRegion::Object] {
        let root = DescriptorRoot { binding, region };
        let Some(offset) = c.global_descriptor_offsets.get(&root).copied() else {
            if c.address_taken_roots.contains(&root) {
                invariant_violation("address-taken global 缺少 static descriptor offset")
            }
            continue;
        };
        let base = bcx.use_var(frame.globals);
        let descriptor = bcx.ins().iadd_imm_s(base, offset as i64);
        let storage = binding_storage_addr(c, bcx, frame, binding)
            .unwrap_or_else(|| invariant_violation("address-taken global BindingId 无 storage"));
        let vty = bound_vty(c, frame, binding);
        let (storage, extent) = descriptor_storage(c, bcx, storage, &vty, region);
        initialize_descriptor(bcx, descriptor, storage, extent, STORAGE_KIND_GLOBAL);
    }
    Ok(())
}

fn descriptor_for<M: Module>(
    c: &Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    root: DescriptorRoot,
) -> Value {
    if let Some(descriptor) = frame
        .storage_descriptors
        .iter()
        .rev()
        .find_map(|scope| scope.get(&root).copied())
    {
        return bcx.use_var(descriptor);
    }
    if let Some(offset) = c.global_descriptor_offsets.get(&root).copied() {
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
    let root = source
        .descriptor_root()
        .unwrap_or_else(|| invariant_violation("Refer 缺少 descriptor root"));
    let (address, source_vty) = emit_place_addr(c, bcx, frame, source)?;
    let layout = value_layout(&source_vty);
    let descriptor = descriptor_for(c, bcx, frame, root);
    if root.region == DescriptorRegion::Object {
        let base = match source {
            Place::Field { base, .. } | Place::Index { base, .. } => base.as_ref(),
            Place::Local { .. } => invariant_violation("object descriptor 必须对应 projection"),
        };
        let storage = binding_storage_addr(c, bcx, frame, root.binding)
            .unwrap_or_else(|| invariant_violation("Refer object root 无 storage"));
        let (object, extent) = descriptor_storage(c, bcx, storage, &c.vty(base.ty()), root.region);
        let kind = if c.global_descriptor_offsets.contains_key(&root) {
            STORAGE_KIND_GLOBAL
        } else {
            STORAGE_KIND_LOCAL
        };
        initialize_descriptor(bcx, descriptor, object, extent, kind);
    }
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

pub(super) fn emit_reinterpret<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    input: (
        &ExprValue,
        &VTy,
        &VTy,
        RuntimeCheckRequirement,
        crate::Span,
    ),
) -> AliasResult<ExprValue> {
    let (source, source_vty, result_vty, alignment_check, span) = input;
    let VTy::Ptr { nullable: false, .. } = source_vty else {
        invariant_violation("reinterpret source 必须是 non-null pointer")
    };
    let VTy::Ptr { pointee, nullable: false } = result_vty else {
        invariant_violation("reinterpret result 必须是 non-null pointer")
    };
    let provenance = source.pointer_lane(bcx, source_vty, PtrLane::Provenance);
    let address = source.pointer_lane(bcx, source_vty, PtrLane::Address);
    let source_end = source.pointer_lane(bcx, source_vty, PtrLane::ViewEnd);
    let layout = value_layout(pointee);
    let align = i64::try_from(layout.align)
        .unwrap_or_else(|_| invariant_violation("reinterpret alignment 超出 i64"));
    let stride = i64::try_from(layout.stride)
        .unwrap_or_else(|_| invariant_violation("reinterpret stride 超出 i64"));
    match alignment_check {
        RuntimeCheckRequirement::Required => {
            let align = bcx.ins().iconst(types::I64, align);
            let remainder = bcx.ins().urem(address, align);
            let misaligned = bcx.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                remainder,
                0,
            );
            super::ops::emit_abort_branch(c, bcx, misaligned, "alias.abort_ptr_alignment", span)?;
        }
        RuntimeCheckRequirement::Proven => {}
    }
    let available = bcx.ins().isub(source_end, address);
    let stride = bcx.ins().iconst(types::I64, stride);
    let tail = bcx.ins().urem(available, stride);
    let extent = bcx.ins().isub(available, tail);
    let view_end = bcx.ins().iadd(address, extent);
    let value = ExprValue::pointer([provenance, address, address, view_end]);
    // Borrowed bindings need an addressable carrier for four lanes. This cell is not a new
    // provenance descriptor, storage root, or initialized target object.
    let cell = allocate_value_cell(c, bcx, result_vty)?;
    value.store(bcx, cell, 0, result_vty);
    register_plain_allocation_cleanup(bcx, frame, cell);
    Ok(ExprValue::scalar(cell))
}
