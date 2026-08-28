use super::*;

pub(super) fn emit_alloc_runtime<M: Module>(
    c: &mut Compiler<'_, M>,
    ext: &NativeExterns,
    heap_alloc: FuncId,
    get_process_heap: FuncId,
) -> AliasResult<()> {
    macro_rules! call_rt_m {
        ($bcx:expr, $nm:expr, $args:expr) => {{
            let __args = $args;
            c.call_rt(&mut $bcx, $nm, &__args)?
        }};
    }
    macro_rules! call_ext_m {
        ($bcx:expr, $fid:expr, $args:expr) => {{
            let __r = c.module.declare_func_in_func($fid, &mut $bcx.func);
            let __args = $args;
            let __inst = $bcx.ins().call(__r, &__args);
            first_result(&$bcx, __inst)
        }};
    }

    shim!(c, "rt.heap.alloc", |bcx, a| {
        let h = call_ext_m!(bcx, get_process_heap, vec![]);
        let flags = bcx.ins().iconst(types::I32, 8);
        let p = call_ext_m!(bcx, heap_alloc, vec![h, flags, a[0]]);
        let failed = bcx.ins().icmp_imm_s(IntCC::Equal, p, 0);
        let fail_b = bcx.create_block();
        let ok_b = bcx.create_block();
        bcx.ins().brif(failed, fail_b, &[], ok_b, &[]);
        bcx.seal_block(fail_b);
        bcx.seal_block(ok_b);
        bcx.switch_to_block(fail_b);
        let one = bcx.ins().iconst(types::I32, 1);
        let ep = c
            .module
            .declare_func_in_func(ext.exit_process, &mut bcx.func);
        bcx.ins().call(ep, &[one]);
        bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);
        bcx.switch_to_block(ok_b);
        bcx.ins().return_(&[p]);
        true
    });

    shim!(c, "alias.cell.new", |bcx, a| {
        let p = call_rt_m!(bcx, "rt.heap.alloc", vec![a[0]]);
        bcx.ins().return_(&[p]);
        true
    });
    shim!(c, "alias.env.new", |bcx, a| {
        let n64 = bcx.ins().sextend(types::I64, a[0]);
        let bytes = bcx.ins().imul_imm_s(n64, 8);
        let p = call_rt_m!(bcx, "rt.heap.alloc", vec![bytes]);
        bcx.ins().return_(&[p]);
        true
    });
    shim!(c, "alias.globals.new", |bcx, a| {
        let p = call_rt_m!(bcx, "rt.heap.alloc", vec![a[0]]);
        bcx.ins().return_(&[p]);
        true
    });
    shim!(c, "alias.closure.new", |bcx, a| {
        let sz = bcx.ins().iconst(types::I64, 16);
        let p = call_rt_m!(bcx, "rt.heap.alloc", vec![sz]);
        bcx.ins().store(MemFlagsData::new(), a[0], p, 0);
        bcx.ins().store(MemFlagsData::new(), a[1], p, 8);
        bcx.ins().return_(&[p]);
        true
    });
    Ok(())
}
