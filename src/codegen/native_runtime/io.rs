use super::NativeExterns;
use crate::codegen::emit::cells::first_result;
use crate::codegen::layout::{STRING_DATA_OFFSET, STRING_LEN_OFFSET};
use crate::codegen::Compiler;
use crate::AliasResult;
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind};
use cranelift_module::Module;
use std::collections::HashMap;

pub(super) fn emit_io_runtime<M: Module>(
    c: &mut Compiler<'_, M>,
    ext: &NativeExterns,
    static_ids: &HashMap<&str, cranelift_module::DataId>,
) -> AliasResult<()> {
    macro_rules! sym {
        ($bcx:expr, $name:expr) => {{
            let __gv = c
                .module
                .declare_data_in_func(static_ids[$name], &mut $bcx.func);
            $bcx.ins().symbol_value(c.ptr_ty, __gv)
        }};
    }
    macro_rules! call_rt_m {
        ($bcx:expr, $nm:expr, $args:expr) => {{
            let __args = $args;
            c.call_rt(&mut $bcx, $nm, &__args)?
        }};
    }
    macro_rules! call_rt_void_m {
        ($bcx:expr, $nm:expr, $args:expr) => {{
            let __args = $args;
            c.call_rt_void(&mut $bcx, $nm, &__args)?
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

    shim!(c, "rt.write.stdout", |bcx, a| {
        // 空 string 以 null data + 0 length 表示，runtime contract 因而只在此参数上允许
        // nullable。该指针仅透传给零长度 WriteFile，不能在 shim 内预读或做地址运算。
        let h = call_ext_m!(
            bcx,
            ext.get_std_handle,
            vec![bcx.ins().iconst(types::I32, -11)]
        );
        let ss = bcx.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
        let wa = bcx.ins().stack_addr(c.ptr_ty, ss, 0);
        let null = bcx.ins().iconst(c.ptr_ty, 0);
        let wf = c.module.declare_func_in_func(ext.write_file, bcx.func);
        let len32 = bcx.ins().ireduce(types::I32, a[1]);
        bcx.ins().call(wf, &[h, a[0], len32, wa, null]);
        false
    });

    shim!(c, "alias.println.str", |bcx, a| {
        let p = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], STRING_DATA_OFFSET);
        let l = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], STRING_LEN_OFFSET);
        call_rt_void_m!(bcx, "rt.write.stdout", vec![p, l]);
        let nl = sym!(bcx, "rt_nl");
        call_rt_void_m!(
            bcx,
            "rt.write.stdout",
            vec![nl, bcx.ins().iconst(types::I64, 1)]
        );
        false
    });
    shim!(c, "alias.print.str", |bcx, a| {
        let p = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], STRING_DATA_OFFSET);
        let l = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], STRING_LEN_OFFSET);
        call_rt_void_m!(bcx, "rt.write.stdout", vec![p, l]);
        false
    });

    for (pname, dname) in [
        ("alias.println.i32", "alias.println.str"),
        ("alias.print.i32", "alias.print.str"),
    ] {
        shim!(c, pname, |bcx, a| {
            let blk = call_rt_m!(bcx, "alias.display.int", vec![a[0]]);
            call_rt_void_m!(bcx, dname, vec![blk]);
            false
        });
    }
    for (pname, dname) in [
        ("alias.println.bool", "alias.println.str"),
        ("alias.print.bool", "alias.print.str"),
    ] {
        shim!(c, pname, |bcx, a| {
            let blk = call_rt_m!(bcx, "alias.display.bool", vec![a[0]]);
            call_rt_void_m!(bcx, dname, vec![blk]);
            false
        });
    }
    Ok(())
}
