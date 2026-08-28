use super::display_float::emit_float_display_shim;
use super::display_integer::emit_integer_display_shim;
use super::NativeExterns;
use crate::codegen::layout::{STRING_BYTES, STRING_DATA_OFFSET, STRING_LEN_OFFSET};
use crate::codegen::Compiler;
use crate::AliasResult;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind};
use cranelift_module::Module;
use std::collections::HashMap;

const DECIMAL_BUFFER_BYTES: i64 = 24;
const DECIMAL_LAST_INDEX: i64 = DECIMAL_BUFFER_BYTES - 1;

pub(super) fn emit_display_runtime<M: Module>(
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

    shim!(c, "rt.write.dec", |bcx, a| {
        let buf = call_rt_m!(
            bcx,
            "rt.heap.alloc",
            vec![bcx.ins().iconst(types::I64, DECIMAL_BUFFER_BYTES)]
        );
        let ten = bcx.ins().iconst(types::I64, 10);
        let digits = bcx.ins().iconst(types::I64, b'0' as i64);
        let pos = bcx.declare_var(types::I64);
        let end_pos = bcx.ins().iconst(types::I64, DECIMAL_LAST_INDEX);
        bcx.def_var(pos, end_pos);
        let n = bcx.declare_var(types::I64);
        bcx.def_var(n, a[1]);
        let loop_b = bcx.create_block();
        let end_b = bcx.create_block();
        bcx.ins().jump(loop_b, &[]);
        bcx.switch_to_block(loop_b);
        {
            let cur = bcx.use_var(n);
            let p = bcx.use_var(pos);
            let d = bcx.ins().srem(cur, ten);
            let ch = bcx.ins().iadd(d, digits);
            let one = bcx.ins().iconst(types::I64, 1);
            let newp = bcx.ins().isub(p, one);
            bcx.def_var(pos, newp);
            let slot = bcx.ins().iadd(buf, newp);
            let b8 = bcx.ins().ireduce(types::I8, ch);
            bcx.ins().store(MemFlagsData::new(), b8, slot, 0);
            let rest = bcx.ins().sdiv(cur, ten);
            bcx.def_var(n, rest);
            let more = bcx.ins().icmp_imm_s(IntCC::NotEqual, rest, 0);
            let again = bcx.create_block();
            bcx.ins().brif(more, again, &[], end_b, &[]);
            bcx.switch_to_block(again);
            bcx.seal_block(again);
            bcx.ins().jump(loop_b, &[]);
            bcx.seal_block(loop_b);
        }
        bcx.switch_to_block(end_b);
        bcx.seal_block(end_b);
        {
            let start = {
                let start_off = bcx.use_var(pos);
                bcx.ins().iadd(buf, start_off)
            };
            let len = {
                let p = bcx.use_var(pos);
                let last_index = bcx.ins().iconst(types::I64, DECIMAL_LAST_INDEX);
                bcx.ins().isub(last_index, p)
            };
            let ss =
                bcx.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
            let wa = bcx.ins().stack_addr(c.ptr_ty, ss, 0);
            let null = bcx.ins().iconst(c.ptr_ty, 0);
            let wf = c.module.declare_func_in_func(ext.write_file, bcx.func);
            let len32 = bcx.ins().ireduce(types::I32, len);
            let wf_args = [a[0], start, len32, wa, null];
            bcx.ins().call(wf, &wf_args);
        }
        false
    });

    shim!(c, "alias.display.int", |bcx, a| {
        let buf = call_rt_m!(
            bcx,
            "rt.heap.alloc",
            vec![bcx.ins().iconst(types::I64, DECIMAL_BUFFER_BYTES)]
        );
        let v64 = bcx.ins().sextend(types::I64, a[0]);
        let zero = bcx.ins().iconst(types::I64, 0);
        let neg = bcx.ins().icmp(IntCC::SignedLessThan, v64, zero);
        let neg_mag = bcx.ins().isub(zero, v64);
        let mag = bcx.ins().select(neg, neg_mag, v64);
        let ten = bcx.ins().iconst(types::I64, 10);
        let pos = bcx.declare_var(types::I64);
        let end_pos = bcx.ins().iconst(types::I64, DECIMAL_LAST_INDEX);
        bcx.def_var(pos, end_pos);
        let n = bcx.declare_var(types::I64);
        bcx.def_var(n, mag);
        let digits = bcx.ins().iconst(types::I64, b'0' as i64);
        let loop_b = bcx.create_block();
        let sign_b = bcx.create_block();
        let end_b = bcx.create_block();
        bcx.ins().jump(loop_b, &[]);
        bcx.switch_to_block(loop_b);
        {
            let cur = bcx.use_var(n);
            let p = bcx.use_var(pos);
            let d = bcx.ins().srem(cur, ten);
            let ch = bcx.ins().iadd(d, digits);
            let one = bcx.ins().iconst(types::I64, 1);
            let newp = bcx.ins().isub(p, one);
            bcx.def_var(pos, newp);
            let slot = bcx.ins().iadd(buf, newp);
            let b8 = bcx.ins().ireduce(types::I8, ch);
            bcx.ins().store(MemFlagsData::new(), b8, slot, 0);
            let rest = bcx.ins().sdiv(cur, ten);
            bcx.def_var(n, rest);
            let more = bcx.ins().icmp_imm_s(IntCC::NotEqual, rest, 0);
            let again = bcx.create_block();
            bcx.ins().brif(more, again, &[], sign_b, &[]);
            bcx.switch_to_block(again);
            bcx.seal_block(again);
            bcx.ins().jump(loop_b, &[]);
            bcx.seal_block(loop_b);
        }
        bcx.switch_to_block(sign_b);
        bcx.seal_block(sign_b);
        {
            let do_sign = bcx.create_block();
            bcx.ins().brif(neg, do_sign, &[], end_b, &[]);
            bcx.switch_to_block(do_sign);
            bcx.seal_block(do_sign);
            let minus = bcx.ins().iconst(types::I64, b'-' as i64);
            let p = bcx.use_var(pos);
            let one = bcx.ins().iconst(types::I64, 1);
            let newp = bcx.ins().isub(p, one);
            bcx.def_var(pos, newp);
            let slot = bcx.ins().iadd(buf, newp);
            let b8 = bcx.ins().ireduce(types::I8, minus);
            bcx.ins().store(MemFlagsData::new(), b8, slot, 0);
            bcx.ins().jump(end_b, &[]);
        }
        bcx.switch_to_block(end_b);
        bcx.seal_block(end_b);
        {
            let start = {
                let start_off = bcx.use_var(pos);
                bcx.ins().iadd(buf, start_off)
            };
            let len = {
                let p = bcx.use_var(pos);
                let last_index = bcx.ins().iconst(types::I64, DECIMAL_LAST_INDEX);
                bcx.ins().isub(last_index, p)
            };
            let blk = call_rt_m!(
                bcx,
                "rt.heap.alloc",
                vec![bcx.ins().iconst(types::I64, STRING_BYTES)]
            );
            bcx.ins()
                .store(MemFlagsData::new(), start, blk, STRING_DATA_OFFSET);
            bcx.ins()
                .store(MemFlagsData::new(), len, blk, STRING_LEN_OFFSET);
            bcx.ins().return_(&[blk]);
        }
        true
    });

    emit_integer_display_shim(c, "alias.display.i64", true)?;
    emit_integer_display_shim(c, "alias.display.u64", false)?;
    emit_float_display_shim(c, "alias.display.f32", types::F32)?;
    emit_float_display_shim(c, "alias.display.f64", types::F64)?;

    shim!(c, "alias.display.bool", |bcx, a| {
        let t_addr = sym!(bcx, "rt_true");
        let f_addr = sym!(bcx, "rt_false");
        let is_t = bcx.ins().icmp_imm_s(IntCC::NotEqual, a[0], 0);
        let addr = bcx.ins().select(is_t, t_addr, f_addr);
        let t_len = bcx.ins().iconst(types::I64, 4);
        let f_len = bcx.ins().iconst(types::I64, 5);
        let len = bcx.ins().select(is_t, t_len, f_len);
        let blk = call_rt_m!(
            bcx,
            "rt.heap.alloc",
            vec![bcx.ins().iconst(types::I64, STRING_BYTES)]
        );
        bcx.ins()
            .store(MemFlagsData::new(), addr, blk, STRING_DATA_OFFSET);
        bcx.ins()
            .store(MemFlagsData::new(), len, blk, STRING_LEN_OFFSET);
        bcx.ins().return_(&[blk]);
        true
    });

    shim!(c, "alias.display.str", |bcx, a| {
        bcx.ins().return_(&[a[0]]);
        true
    });

    for (name, dname, dlen) in [
        ("alias.display.func", "rt_func", 6i64),
        ("alias.display.struct", "rt_struct", 8),
        ("alias.display.array", "rt_array", 7),
    ] {
        shim!(c, name, |bcx, _a| {
            let addr = sym!(bcx, dname);
            let len = bcx.ins().iconst(types::I64, dlen);
            let blk = call_rt_m!(
                bcx,
                "rt.heap.alloc",
                vec![bcx.ins().iconst(types::I64, STRING_BYTES)]
            );
            bcx.ins()
                .store(MemFlagsData::new(), addr, blk, STRING_DATA_OFFSET);
            bcx.ins()
                .store(MemFlagsData::new(), len, blk, STRING_LEN_OFFSET);
            bcx.ins().return_(&[blk]);
            true
        });
    }

    shim!(c, "alias.display.result", |bcx, a| {
        let ok_addr = sym!(bcx, "rt_ok");
        let err_addr = sym!(bcx, "rt_err");
        let is_ok = bcx.ins().icmp_imm_s(IntCC::Equal, a[0], 0);
        let ok_len = bcx.ins().iconst(types::I64, 4);
        let err_len = bcx.ins().iconst(types::I64, 5);
        let addr = bcx.ins().select(is_ok, ok_addr, err_addr);
        let len = bcx.ins().select(is_ok, ok_len, err_len);
        let blk = call_rt_m!(
            bcx,
            "rt.heap.alloc",
            vec![bcx.ins().iconst(types::I64, STRING_BYTES)]
        );
        bcx.ins()
            .store(MemFlagsData::new(), addr, blk, STRING_DATA_OFFSET);
        bcx.ins()
            .store(MemFlagsData::new(), len, blk, STRING_LEN_OFFSET);
        bcx.ins().return_(&[blk]);
        true
    });

    Ok(())
}
