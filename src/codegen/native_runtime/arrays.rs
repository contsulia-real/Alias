use super::*;

pub(super) fn emit_array_runtime<M: Module>(
    c: &mut Compiler<'_, M>,
    rtl_move_memory: FuncId,
) -> AliasResult<()> {
    macro_rules! call_rt_m {
        ($bcx:expr, $nm:expr, $args:expr) => {{
            let __args = $args;
            c.call_rt(&mut $bcx, $nm, &__args)?
        }};
    }

    shim!(c, "alias.arr.new", |bcx, a| {
        let hdr = call_rt_m!(bcx, "rt.heap.alloc", vec![bcx.ins().iconst(types::I64, 24)]);
        let cap64 = bcx.ins().sextend(types::I64, a[0]);
        let esz64 = bcx.ins().sextend(types::I64, a[1]);
        let has = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, cap64, 0);
        let then_b = bcx.create_block();
        let else_b = bcx.create_block();
        let end_b = bcx.create_block();
        bcx.ins().brif(has, then_b, &[], else_b, &[]);
        bcx.seal_block(then_b);
        bcx.seal_block(else_b);
        bcx.switch_to_block(then_b);
        {
            let bytes = bcx.ins().imul(cap64, esz64);
            let buf = call_rt_m!(bcx, "rt.heap.alloc", vec![bytes]);
            bcx.ins().store(MemFlagsData::new(), buf, hdr, 0);
            bcx.ins().jump(end_b, &[]);
        }
        bcx.switch_to_block(else_b);
        {
            let zero = bcx.ins().iconst(types::I64, 0);
            bcx.ins().store(MemFlagsData::new(), zero, hdr, 0);
            bcx.ins().jump(end_b, &[]);
        }
        bcx.switch_to_block(end_b);
        bcx.seal_block(end_b);
        let zero = bcx.ins().iconst(types::I64, 0);
        bcx.ins()
            .store(MemFlagsData::new(), zero, hdr, value_word_offset(1));
        bcx.ins()
            .store(MemFlagsData::new(), cap64, hdr, value_word_offset(2));
        bcx.ins().return_(&[hdr]);
        true
    });

    shim!(c, "alias.arr.len", |bcx, a| {
        let l = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], value_word_offset(1));
        let t = bcx.ins().ireduce(types::I32, l);
        bcx.ins().return_(&[t]);
        true
    });

    shim!(c, "alias.arr.push", |bcx, a| {
        let hdr = a[0];
        let dp0 = bcx.ins().load(types::I64, MemFlagsData::new(), hdr, 0);
        let len = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), hdr, value_word_offset(1));
        let cap = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), hdr, value_word_offset(2));
        let full = bcx.ins().icmp(IntCC::Equal, len, cap);
        let grow_b = bcx.create_block();
        let ok_b = bcx.create_block();
        let join_b = bcx.create_block();
        let jdp = bcx.append_block_param(join_b, types::I64);
        bcx.ins().brif(full, grow_b, &[], ok_b, &[]);
        bcx.seal_block(grow_b);
        bcx.seal_block(ok_b);

        bcx.switch_to_block(grow_b);
        {
            let one = bcx.ins().iconst(types::I64, 1);
            let is_empty = bcx.ins().icmp_imm_s(IntCC::Equal, cap, 0);
            let doubled = bcx.ins().imul_imm_s(cap, 2);
            let new_cap = bcx.ins().select(is_empty, one, doubled);
            let bytes = bcx.ins().imul_imm_s(new_cap, VALUE_WORD_BYTES);
            let grown = call_rt_m!(bcx, "rt.heap.alloc", vec![bytes]);
            let mv = c.module.declare_func_in_func(rtl_move_memory, bcx.func);
            let copy_bytes = bcx.ins().imul_imm_s(len, VALUE_WORD_BYTES);
            let copy_b = bcx.create_block();
            let after_copy_b = bcx.create_block();
            let has_old = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, len, 0);
            bcx.ins().brif(has_old, copy_b, &[], after_copy_b, &[]);
            bcx.seal_block(copy_b);
            bcx.switch_to_block(copy_b);
            bcx.ins().call(mv, &[grown, dp0, copy_bytes]);
            bcx.ins().jump(after_copy_b, &[]);
            bcx.switch_to_block(after_copy_b);
            bcx.seal_block(after_copy_b);
            bcx.ins().store(MemFlagsData::new(), grown, hdr, 0);
            bcx.ins()
                .store(MemFlagsData::new(), new_cap, hdr, value_word_offset(2));
            bcx.ins().jump(join_b, &[BlockArg::Value(grown)]);
        }
        bcx.switch_to_block(ok_b);
        bcx.ins().jump(join_b, &[BlockArg::Value(dp0)]);

        bcx.switch_to_block(join_b);
        bcx.seal_block(join_b);
        let slot = bcx.ins().imul_imm_s(len, VALUE_WORD_BYTES);
        let addr = bcx.ins().iadd(jdp, slot);
        bcx.ins().store(MemFlagsData::new(), a[1], addr, 0);
        let one = bcx.ins().iconst(types::I64, 1);
        let len1 = bcx.ins().iadd(len, one);
        bcx.ins()
            .store(MemFlagsData::new(), len1, hdr, value_word_offset(1));
        false
    });

    shim!(c, "alias.arr.pop", |bcx, a| {
        let hdr = a[0];
        let len = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), hdr, value_word_offset(1));
        let new_len = bcx.ins().iadd_imm_s(len, -1);
        bcx.ins()
            .store(MemFlagsData::new(), new_len, hdr, value_word_offset(1));
        let dp = bcx.ins().load(types::I64, MemFlagsData::new(), hdr, 0);
        let slot = bcx.ins().imul_imm_s(new_len, VALUE_WORD_BYTES);
        let addr = bcx.ins().iadd(dp, slot);
        let v = bcx.ins().load(types::I64, MemFlagsData::new(), addr, 0);
        bcx.ins().return_(&[v]);
        true
    });
    Ok(())
}
