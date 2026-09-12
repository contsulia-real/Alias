//! Raw allocation provenance runtime.
//!
//! Source-level malloc/free reaches these shims only after sema proves allocation-root ownership.
//! They currently own canonical storage identity and the empty-metadata lifecycle; non-empty raw
//! initialization metadata remains fail-closed until typed reverse-order destruction is wired.

use crate::codegen::layout::{
    RAW_INIT_METADATA_BYTES, RAW_INIT_REGIONS_OFFSET, RAW_INIT_REGION_COUNT_OFFSET,
    STORAGE_DESCRIPTOR_BASE_OFFSET, STORAGE_DESCRIPTOR_BYTES, STORAGE_DESCRIPTOR_EXTENT_OFFSET,
    STORAGE_DESCRIPTOR_KIND_OFFSET, STORAGE_DESCRIPTOR_RAW_METADATA_OFFSET, STORAGE_KIND_RAW,
};
use crate::codegen::Compiler;
use crate::AliasResult;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, TrapCode};
use cranelift_module::Module;

pub(super) fn emit_raw_runtime<M: Module>(c: &mut Compiler<'_, M>) -> AliasResult<()> {
    macro_rules! call_rt_m {
        ($bcx:expr, $name:expr, $args:expr) => {{
            let __args = $args;
            c.call_rt(&mut $bcx, $name, &__args)?
        }};
    }

    shim!(c, "rt.raw.alloc", |bcx, a| {
        let storage = call_rt_m!(bcx, "rt.heap.try_alloc", vec![a[0]]);
        let storage_failed = bcx.ins().icmp_imm_s(IntCC::Equal, storage, 0);
        let no_storage_b = bcx.create_block();
        let metadata_b = bcx.create_block();
        bcx.ins()
            .brif(storage_failed, no_storage_b, &[], metadata_b, &[]);
        bcx.seal_block(no_storage_b);
        bcx.seal_block(metadata_b);

        bcx.switch_to_block(no_storage_b);
        let null = bcx.ins().iconst(c.machine_ptr_ty, 0);
        bcx.ins().return_(&[null]);

        bcx.switch_to_block(metadata_b);
        let metadata_size = bcx.ins().iconst(types::I64, RAW_INIT_METADATA_BYTES);
        let metadata = call_rt_m!(bcx, "rt.heap.try_alloc", vec![metadata_size]);
        let metadata_failed = bcx.ins().icmp_imm_s(IntCC::Equal, metadata, 0);
        let release_storage_b = bcx.create_block();
        let descriptor_b = bcx.create_block();
        bcx.ins()
            .brif(metadata_failed, release_storage_b, &[], descriptor_b, &[]);
        bcx.seal_block(release_storage_b);
        bcx.seal_block(descriptor_b);

        bcx.switch_to_block(release_storage_b);
        c.call_rt_void(&mut bcx, "rt.heap.free", &[storage])?;
        let null = bcx.ins().iconst(c.machine_ptr_ty, 0);
        bcx.ins().return_(&[null]);

        bcx.switch_to_block(descriptor_b);
        let descriptor_size = bcx.ins().iconst(types::I64, STORAGE_DESCRIPTOR_BYTES);
        let descriptor = call_rt_m!(bcx, "rt.heap.try_alloc", vec![descriptor_size]);
        let descriptor_failed = bcx.ins().icmp_imm_s(IntCC::Equal, descriptor, 0);
        let release_metadata_b = bcx.create_block();
        let ready_b = bcx.create_block();
        bcx.ins()
            .brif(descriptor_failed, release_metadata_b, &[], ready_b, &[]);
        bcx.seal_block(release_metadata_b);
        bcx.seal_block(ready_b);

        bcx.switch_to_block(release_metadata_b);
        c.call_rt_void(&mut bcx, "rt.heap.free", &[metadata])?;
        c.call_rt_void(&mut bcx, "rt.heap.free", &[storage])?;
        let null = bcx.ins().iconst(c.machine_ptr_ty, 0);
        bcx.ins().return_(&[null]);

        bcx.switch_to_block(ready_b);
        bcx.ins().store(
            MemFlagsData::new(),
            storage,
            descriptor,
            STORAGE_DESCRIPTOR_BASE_OFFSET,
        );
        bcx.ins().store(
            MemFlagsData::new(),
            a[0],
            descriptor,
            STORAGE_DESCRIPTOR_EXTENT_OFFSET,
        );
        let raw_kind = bcx.ins().iconst(types::I64, STORAGE_KIND_RAW);
        bcx.ins().store(
            MemFlagsData::new(),
            raw_kind,
            descriptor,
            STORAGE_DESCRIPTOR_KIND_OFFSET,
        );
        bcx.ins().store(
            MemFlagsData::new(),
            metadata,
            descriptor,
            STORAGE_DESCRIPTOR_RAW_METADATA_OFFSET,
        );
        bcx.ins().return_(&[descriptor]);
        true
    });

    shim!(c, "rt.raw.free", |bcx, a| {
        let absent = bcx.ins().icmp_imm_s(IntCC::Equal, a[0], 0);
        let done_b = bcx.create_block();
        let inspect_b = bcx.create_block();
        bcx.ins().brif(absent, done_b, &[], inspect_b, &[]);
        bcx.seal_block(inspect_b);

        bcx.switch_to_block(inspect_b);
        let kind = bcx.ins().load(
            types::I64,
            MemFlagsData::new(),
            a[0],
            STORAGE_DESCRIPTOR_KIND_OFFSET,
        );
        let wrong_kind = bcx
            .ins()
            .icmp_imm_s(IntCC::NotEqual, kind, STORAGE_KIND_RAW);
        let invalid_b = bcx.create_block();
        let metadata_check_b = bcx.create_block();
        bcx.ins()
            .brif(wrong_kind, invalid_b, &[], metadata_check_b, &[]);
        bcx.seal_block(invalid_b);
        bcx.seal_block(metadata_check_b);

        bcx.switch_to_block(invalid_b);
        bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);

        bcx.switch_to_block(metadata_check_b);
        let metadata = bcx.ins().load(
            c.machine_ptr_ty,
            MemFlagsData::new(),
            a[0],
            STORAGE_DESCRIPTOR_RAW_METADATA_OFFSET,
        );
        let missing_metadata = bcx.ins().icmp_imm_s(IntCC::Equal, metadata, 0);
        let metadata_invalid_b = bcx.create_block();
        let regions_check_b = bcx.create_block();
        bcx.ins().brif(
            missing_metadata,
            metadata_invalid_b,
            &[],
            regions_check_b,
            &[],
        );
        bcx.seal_block(metadata_invalid_b);
        bcx.seal_block(regions_check_b);

        bcx.switch_to_block(metadata_invalid_b);
        bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);

        bcx.switch_to_block(regions_check_b);
        let region_count = bcx.ins().load(
            types::I64,
            MemFlagsData::new(),
            metadata,
            RAW_INIT_REGION_COUNT_OFFSET,
        );
        let has_regions = bcx.ins().icmp_imm_s(IntCC::NotEqual, region_count, 0);
        let unsupported_b = bcx.create_block();
        let release_b = bcx.create_block();
        bcx.ins()
            .brif(has_regions, unsupported_b, &[], release_b, &[]);
        bcx.seal_block(unsupported_b);
        bcx.seal_block(release_b);

        bcx.switch_to_block(unsupported_b);
        // Initialized-region destruction is deliberately fail-closed until its runtime type and
        // destruction descriptors land; silently freeing such storage would leak owned children.
        bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);

        bcx.switch_to_block(release_b);
        let regions = bcx.ins().load(
            c.machine_ptr_ty,
            MemFlagsData::new(),
            metadata,
            RAW_INIT_REGIONS_OFFSET,
        );
        let storage = bcx.ins().load(
            c.machine_ptr_ty,
            MemFlagsData::new(),
            a[0],
            STORAGE_DESCRIPTOR_BASE_OFFSET,
        );
        c.call_rt_void(&mut bcx, "rt.heap.free", &[regions])?;
        c.call_rt_void(&mut bcx, "rt.heap.free", &[metadata])?;
        c.call_rt_void(&mut bcx, "rt.heap.free", &[storage])?;
        c.call_rt_void(&mut bcx, "rt.heap.free", &[a[0]])?;
        bcx.ins().jump(done_b, &[]);

        bcx.seal_block(done_b);
        bcx.switch_to_block(done_b);
        bcx.ins().return_(&[]);
        true
    });

    Ok(())
}
