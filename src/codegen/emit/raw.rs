//! Lowering for already-resolved raw allocation operations.
//!
//! Sema has already proved allocation-root ownership consumption before these structured HIR
//! nodes arrive. This module constructs and consumes the canonical descriptor/four-lane value;
//! it never inspects source syntax or guesses whether a pointer is an owner.

use super::expr::emit_expr;
use super::value::ExprValue;
use crate::codegen::abi::{value_layout, PtrLane, VTy};
use crate::codegen::layout::{STORAGE_DESCRIPTOR_BASE_OFFSET, STORAGE_DESCRIPTOR_EXTENT_OFFSET};
use crate::codegen::{invariant_violation, Compiler, Frame};
use crate::sema::hir::Expr;
use crate::sema::types::{IntW, Ty};
use crate::AliasResult;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, BlockArg, InstBuilder, MemFlagsData};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;

pub(crate) fn emit_raw_allocate<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    element_ty: &Ty,
    count: &Expr,
    result_vty: &VTy,
) -> AliasResult<ExprValue> {
    let VTy::Ptr {
        pointee,
        nullable: true,
    } = result_vty
    else {
        invariant_violation("raw allocation result 必须是 nullable ptr<T>")
    };
    if c.vty(element_ty) != **pointee {
        invariant_violation("raw allocation element type 与 pointer pointee ABI 漂移")
    }
    if c.vty(count.ty()) != VTy::I(IntW::W64) {
        invariant_violation("raw allocation HIR count 必须规范化为 i64")
    }
    let count =
        emit_expr(c, bcx, frame, count)?.into_scalar("raw allocation count 必须是单 lane i64");
    let stride = value_layout(pointee).stride;
    let stride = i64::try_from(stride)
        .unwrap_or_else(|_| invariant_violation("raw allocation element stride 超出 i64"));

    let null_b = bcx.create_block();
    let multiply_b = bcx.create_block();
    let allocate_b = bcx.create_block();
    let success_b = bcx.create_block();
    let result_b = bcx.create_block();
    for _ in PtrLane::ALL {
        bcx.append_block_param(result_b, types::I64);
    }

    let non_positive = bcx.ins().icmp_imm_s(IntCC::SignedLessThanOrEqual, count, 0);
    bcx.ins().brif(non_positive, null_b, &[], multiply_b, &[]);

    bcx.seal_block(multiply_b);
    bcx.switch_to_block(multiply_b);
    let stride_value = bcx.ins().iconst(types::I64, stride);
    let (bytes, overflow) = bcx.ins().umul_overflow(count, stride_value);
    bcx.ins().brif(overflow, null_b, &[], allocate_b, &[]);

    bcx.seal_block(allocate_b);
    bcx.switch_to_block(allocate_b);
    let descriptor = c.call_rt(bcx, "rt.raw.alloc", &[bytes])?;
    let failed = bcx.ins().icmp_imm_s(IntCC::Equal, descriptor, 0);
    bcx.ins().brif(failed, null_b, &[], success_b, &[]);

    bcx.seal_block(success_b);
    bcx.switch_to_block(success_b);
    let base = bcx.ins().load(
        c.machine_ptr_ty,
        MemFlagsData::new(),
        descriptor,
        STORAGE_DESCRIPTOR_BASE_OFFSET,
    );
    let extent = bcx.ins().load(
        types::I64,
        MemFlagsData::new(),
        descriptor,
        STORAGE_DESCRIPTOR_EXTENT_OFFSET,
    );
    let end = bcx.ins().iadd(base, extent);
    bcx.ins().jump(
        result_b,
        &[
            BlockArg::Value(descriptor),
            BlockArg::Value(base),
            BlockArg::Value(base),
            BlockArg::Value(end),
        ],
    );

    bcx.seal_block(null_b);
    bcx.switch_to_block(null_b);
    let zero = bcx.ins().iconst(types::I64, 0);
    bcx.ins().jump(result_b, &[BlockArg::Value(zero); 4]);

    bcx.seal_block(result_b);
    bcx.switch_to_block(result_b);
    Ok(ExprValue::pointer(
        bcx.block_params(result_b)
            .try_into()
            .unwrap_or_else(|_| invariant_violation("raw allocation result 必须有四个 lane")),
    ))
}

pub(crate) fn emit_raw_free<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    pointer: &Expr,
) -> AliasResult<ExprValue> {
    let vty = c.vty(pointer.ty());
    let value = emit_expr(c, bcx, frame, pointer)?;
    let descriptor = value.pointer_lane(bcx, &vty, PtrLane::Provenance);
    c.call_rt_void(bcx, "rt.raw.free", &[descriptor])?;
    Ok(ExprValue::scalar(bcx.ins().iconst(types::I64, 0)))
}
