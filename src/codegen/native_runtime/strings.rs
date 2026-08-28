use super::declare_runtime_shim;
use crate::codegen::emit::cells::first_result;
use crate::codegen::layout::{STRING_BYTES, STRING_DATA_OFFSET, STRING_LEN_OFFSET};
use crate::codegen::Compiler;
use crate::AliasResult;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{
    types, BlockArg, Function, InstBuilder, MemFlagsData, UserFuncName, Value,
};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Module};

const TRIM_SET: &[u8] = b" \t\r\n";

fn emit_is_trim_byte(bcx: &mut FunctionBuilder, b: Value) -> Value {
    let mut acc = bcx.ins().icmp_imm_s(IntCC::Equal, b, TRIM_SET[0] as i64);
    for &t in &TRIM_SET[1..] {
        let e = bcx.ins().icmp_imm_s(IntCC::Equal, b, t as i64);
        acc = bcx.ins().bor(acc, e);
    }
    acc
}

fn emit_case_shim<M: Module>(
    c: &mut Compiler<'_, M>,
    name: &str,
    lo: i64,
    hi: i64,
    delta: i64,
) -> AliasResult<()> {
    let (fid, sig, _) = declare_runtime_shim(c, name)?;
    let mut ctx = Context::new();
    ctx.func = Function::with_name_signature(UserFuncName::user(0x77, fid.as_u32()), sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fbc);
    let entry = bcx.create_block();
    bcx.append_block_params_for_function_params(entry);
    bcx.switch_to_block(entry);
    bcx.seal_block(entry);
    let a: Vec<Value> = bcx.block_params(entry).to_vec();

    let pa = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), a[0], STRING_DATA_OFFSET);
    let la = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), a[0], STRING_LEN_OFFSET);
    let lo_c = bcx.ins().iconst(types::I8, lo);
    let hi_c = bcx.ins().iconst(types::I8, hi);
    let delta_c = bcx.ins().iconst(types::I8, delta);

    let has = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, la, 0);
    let then_b = bcx.create_block();
    let else_b = bcx.create_block();
    let end_b = bcx.create_block();
    let jv = bcx.append_block_param(end_b, types::I64);
    bcx.ins().brif(has, then_b, &[], else_b, &[]);
    bcx.seal_block(then_b);
    bcx.seal_block(else_b);

    bcx.switch_to_block(then_b);
    {
        let alloc_f = c.import_runtime("rt.heap.alloc")?;
        let out = {
            let r = c.module.declare_func_in_func(alloc_f, bcx.func);
            let inst = bcx.ins().call(r, &[la]);
            first_result(&bcx, inst)
        };
        let i = bcx.declare_var(types::I64);
        let i0 = bcx.ins().iconst(types::I64, 0);
        bcx.def_var(i, i0);
        let loop_b = bcx.create_block();
        let done_b = bcx.create_block();
        bcx.ins().jump(loop_b, &[]);
        bcx.switch_to_block(loop_b);
        {
            let iv = bcx.use_var(i);
            let more = bcx.ins().icmp(IntCC::SignedLessThan, iv, la);
            let body_b = bcx.create_block();
            bcx.ins().brif(more, body_b, &[], done_b, &[]);
            bcx.seal_block(body_b);
            bcx.switch_to_block(body_b);
            let addr = bcx.ins().iadd(pa, iv);
            let b = bcx.ins().load(types::I8, MemFlagsData::new(), addr, 0);
            let ge = bcx.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, b, lo_c);
            let le = bcx.ins().icmp(IntCC::UnsignedLessThanOrEqual, b, hi_c);
            let in_range = bcx.ins().band(ge, le);
            let mapped = bcx.ins().iadd(b, delta_c);
            let nb = bcx.ins().select(in_range, mapped, b);
            let slot = bcx.ins().iadd(out, iv);
            bcx.ins().store(MemFlagsData::new(), nb, slot, 0);
            let one = bcx.ins().iconst(types::I64, 1);
            let next = bcx.ins().iadd(iv, one);
            bcx.def_var(i, next);
            bcx.ins().jump(loop_b, &[]);
            bcx.seal_block(loop_b);
        }
        bcx.seal_block(done_b);
        bcx.switch_to_block(done_b);
        let blk = {
            let r = c.module.declare_func_in_func(alloc_f, bcx.func);
            let sz = bcx.ins().iconst(types::I64, STRING_BYTES);
            let inst = bcx.ins().call(r, &[sz]);
            first_result(&bcx, inst)
        };
        bcx.ins()
            .store(MemFlagsData::new(), out, blk, STRING_DATA_OFFSET);
        bcx.ins()
            .store(MemFlagsData::new(), la, blk, STRING_LEN_OFFSET);
        bcx.ins().jump(end_b, &[BlockArg::Value(blk)]);
    }
    bcx.switch_to_block(else_b);
    {
        let zero = bcx.ins().iconst(types::I64, 0);
        let blk = {
            let f = c.import_runtime("rt.heap.alloc")?;
            let r = c.module.declare_func_in_func(f, bcx.func);
            let sz = bcx.ins().iconst(types::I64, STRING_BYTES);
            let inst = bcx.ins().call(r, &[sz]);
            first_result(&bcx, inst)
        };
        bcx.ins()
            .store(MemFlagsData::new(), zero, blk, STRING_DATA_OFFSET);
        bcx.ins()
            .store(MemFlagsData::new(), zero, blk, STRING_LEN_OFFSET);
        bcx.ins().jump(end_b, &[BlockArg::Value(blk)]);
    }
    bcx.switch_to_block(end_b);
    bcx.seal_block(end_b);
    bcx.ins().return_(&[jv]);
    bcx.finalize(c.module.target_config());
    c.define_verified_function(fid, &mut ctx, "字符串大小写 runtime")
}

pub(super) fn emit_string_runtime<M: Module>(
    c: &mut Compiler<'_, M>,
    rtl_move_memory: FuncId,
) -> AliasResult<()> {
    macro_rules! call_rt_m {
        ($bcx:expr, $nm:expr, $args:expr) => {{
            let __args = $args;
            c.call_rt(&mut $bcx, $nm, &__args)?
        }};
    }

    shim!(c, "alias.str.new", |bcx, a| {
        let blk = call_rt_m!(
            bcx,
            "rt.heap.alloc",
            vec![bcx.ins().iconst(types::I64, STRING_BYTES)]
        );
        let len64 = bcx.ins().sextend(types::I64, a[1]);
        let has = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, len64, 0);
        let then_b = bcx.create_block();
        let else_b = bcx.create_block();
        let end_b = bcx.create_block();
        bcx.ins().brif(has, then_b, &[], else_b, &[]);
        bcx.seal_block(then_b);
        bcx.seal_block(else_b);
        bcx.switch_to_block(then_b);
        {
            let buf = call_rt_m!(bcx, "rt.heap.alloc", vec![len64]);
            let mv = c.module.declare_func_in_func(rtl_move_memory, bcx.func);
            bcx.ins().call(mv, &[buf, a[0], len64]);
            bcx.ins()
                .store(MemFlagsData::new(), buf, blk, STRING_DATA_OFFSET);
            bcx.ins().jump(end_b, &[]);
        }
        bcx.switch_to_block(else_b);
        {
            let zero = bcx.ins().iconst(types::I64, 0);
            bcx.ins()
                .store(MemFlagsData::new(), zero, blk, STRING_DATA_OFFSET);
            bcx.ins().jump(end_b, &[]);
        }
        bcx.switch_to_block(end_b);
        bcx.seal_block(end_b);
        bcx.ins()
            .store(MemFlagsData::new(), len64, blk, STRING_LEN_OFFSET);
        bcx.ins().return_(&[blk]);
        true
    });

    shim!(c, "alias.str.concat", |bcx, a| {
        let pa = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], STRING_DATA_OFFSET);
        let la = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], STRING_LEN_OFFSET);
        let pb = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[1], STRING_DATA_OFFSET);
        let lb = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[1], STRING_LEN_OFFSET);
        let total = bcx.ins().iadd(la, lb);
        let has_total = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, total, 0);
        let alloc_b = bcx.create_block();
        let empty_b = bcx.create_block();
        let data_b = bcx.create_block();
        let out_word = bcx.append_block_param(data_b, types::I64);
        bcx.ins().brif(has_total, alloc_b, &[], empty_b, &[]);
        bcx.seal_block(alloc_b);
        bcx.seal_block(empty_b);
        bcx.switch_to_block(alloc_b);
        let out = call_rt_m!(bcx, "rt.heap.alloc", vec![total]);
        bcx.ins().jump(data_b, &[BlockArg::Value(out)]);
        bcx.switch_to_block(empty_b);
        let null = bcx.ins().iconst(types::I64, 0);
        bcx.ins().jump(data_b, &[BlockArg::Value(null)]);
        bcx.switch_to_block(data_b);
        bcx.seal_block(data_b);

        let copy_a_b = bcx.create_block();
        let after_a_b = bcx.create_block();
        let has_a = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, la, 0);
        bcx.ins().brif(has_a, copy_a_b, &[], after_a_b, &[]);
        bcx.seal_block(copy_a_b);
        bcx.switch_to_block(copy_a_b);
        let mv = c.module.declare_func_in_func(rtl_move_memory, bcx.func);
        bcx.ins().call(mv, &[out_word, pa, la]);
        bcx.ins().jump(after_a_b, &[]);
        bcx.switch_to_block(after_a_b);
        bcx.seal_block(after_a_b);

        let copy_b_b = bcx.create_block();
        let after_b_b = bcx.create_block();
        let has_b = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, lb, 0);
        bcx.ins().brif(has_b, copy_b_b, &[], after_b_b, &[]);
        bcx.seal_block(copy_b_b);
        bcx.switch_to_block(copy_b_b);
        let out2 = bcx.ins().iadd(out_word, la);
        bcx.ins().call(mv, &[out2, pb, lb]);
        bcx.ins().jump(after_b_b, &[]);
        bcx.switch_to_block(after_b_b);
        bcx.seal_block(after_b_b);
        let blk = call_rt_m!(
            bcx,
            "rt.heap.alloc",
            vec![bcx.ins().iconst(types::I64, STRING_BYTES)]
        );
        bcx.ins()
            .store(MemFlagsData::new(), out_word, blk, STRING_DATA_OFFSET);
        bcx.ins()
            .store(MemFlagsData::new(), total, blk, STRING_LEN_OFFSET);
        bcx.ins().return_(&[blk]);
        true
    });

    shim!(c, "alias.str.cmp", |bcx, a| {
        let pa = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], STRING_DATA_OFFSET);
        let la = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], STRING_LEN_OFFSET);
        let pb = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[1], STRING_DATA_OFFSET);
        let lb = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[1], STRING_LEN_OFFSET);
        let min_len = bcx.ins().smin(la, lb);
        let i = bcx.declare_var(types::I64);
        let i0 = bcx.ins().iconst(types::I64, 0);
        bcx.def_var(i, i0);
        let m1w = bcx.ins().iconst(types::I64, -1);
        let p1w = bcx.ins().iconst(types::I64, 1);
        let zw = bcx.ins().iconst(types::I64, 0);
        let loop_b = bcx.create_block();
        bcx.ins().jump(loop_b, &[]);
        bcx.switch_to_block(loop_b);
        {
            let iv = bcx.use_var(i);
            let in_range = bcx.ins().icmp(IntCC::UnsignedLessThan, iv, min_len);
            let cmp_body = bcx.create_block();
            let by_len = bcx.create_block();
            bcx.ins().brif(in_range, cmp_body, &[], by_len, &[]);
            bcx.seal_block(cmp_body);
            bcx.switch_to_block(cmp_body);
            {
                let iv2 = bcx.use_var(i);
                let mflags = MemFlagsData::new();
                let a_addr = bcx.ins().iadd(pa, iv2);
                let b_addr = bcx.ins().iadd(pb, iv2);
                let b_a = bcx.ins().load(types::I8, mflags, a_addr, 0);
                let b_b = bcx.ins().load(types::I8, mflags, b_addr, 0);
                let same = bcx.ins().icmp(IntCC::Equal, b_a, b_b);
                let ne_body = bcx.create_block();
                let one = bcx.ins().iconst(types::I64, 1);
                let inc = bcx.ins().iadd(iv2, one);
                bcx.def_var(i, inc);
                bcx.ins().brif(same, loop_b, &[], ne_body, &[]);
                bcx.seal_block(loop_b);
                bcx.seal_block(ne_body);
                bcx.switch_to_block(ne_body);
                let less = bcx.ins().icmp(IntCC::SignedLessThan, b_a, b_b);
                let word = bcx.ins().select(less, m1w, p1w);
                let out = bcx.ins().ireduce(types::I32, word);
                bcx.ins().return_(&[out]);
            }
            bcx.switch_to_block(by_len);
            bcx.seal_block(by_len);
            let lt = bcx.ins().icmp(IntCC::SignedLessThan, la, lb);
            let eq = bcx.ins().icmp(IntCC::Equal, la, lb);
            let by_len_word = {
                let inner = bcx.ins().select(eq, zw, p1w);
                bcx.ins().select(lt, m1w, inner)
            };
            let out = bcx.ins().ireduce(types::I32, by_len_word);
            bcx.ins().return_(&[out]);
        }
        true
    });

    shim!(c, "alias.str.len", |bcx, a| {
        let l = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], STRING_LEN_OFFSET);
        let t = bcx.ins().ireduce(types::I32, l);
        bcx.ins().return_(&[t]);
        true
    });
    emit_case_shim(c, "alias.str.upper", b'a' as i64, b'z' as i64, -32)?;
    emit_case_shim(c, "alias.str.lower", b'A' as i64, b'Z' as i64, 32)?;

    shim!(c, "alias.str.trim", |bcx, a| {
        let pa = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], STRING_DATA_OFFSET);
        let la = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), a[0], STRING_LEN_OFFSET);
        let st = bcx.declare_var(types::I64);
        let st0 = bcx.ins().iconst(types::I64, 0);
        bcx.def_var(st, st0);
        let en = bcx.declare_var(types::I64);
        bcx.def_var(en, la);
        let one = bcx.ins().iconst(types::I64, 1);
        let loop_l = bcx.create_block();
        let done_l = bcx.create_block();
        bcx.ins().jump(loop_l, &[]);
        bcx.switch_to_block(loop_l);
        {
            let sv = bcx.use_var(st);
            let more = bcx.ins().icmp(IntCC::SignedLessThan, sv, la);
            let body_l = bcx.create_block();
            bcx.ins().brif(more, body_l, &[], done_l, &[]);
            bcx.seal_block(body_l);
            bcx.switch_to_block(body_l);
            let addr = bcx.ins().iadd(pa, sv);
            let b = bcx.ins().load(types::I8, MemFlagsData::new(), addr, 0);
            let hit = emit_is_trim_byte(&mut bcx, b);
            let adv_l = bcx.create_block();
            bcx.ins().brif(hit, adv_l, &[], done_l, &[]);
            bcx.seal_block(adv_l);
            bcx.switch_to_block(adv_l);
            let st_next = bcx.ins().iadd(sv, one);
            bcx.def_var(st, st_next);
            bcx.ins().jump(loop_l, &[]);
            bcx.seal_block(loop_l);
        }
        bcx.seal_block(done_l);
        bcx.switch_to_block(done_l);

        let loop_t = bcx.create_block();
        let done_t = bcx.create_block();
        bcx.ins().jump(loop_t, &[]);
        bcx.switch_to_block(loop_t);
        {
            let ev = bcx.use_var(en);
            let stv = bcx.use_var(st);
            let more = bcx.ins().icmp(IntCC::SignedGreaterThan, ev, stv);
            let body_t = bcx.create_block();
            bcx.ins().brif(more, body_t, &[], done_t, &[]);
            bcx.seal_block(body_t);
            bcx.switch_to_block(body_t);
            let idx = bcx.ins().isub(ev, one);
            let addr = bcx.ins().iadd(pa, idx);
            let b = bcx.ins().load(types::I8, MemFlagsData::new(), addr, 0);
            let hit = emit_is_trim_byte(&mut bcx, b);
            let adv_t = bcx.create_block();
            bcx.ins().brif(hit, adv_t, &[], done_t, &[]);
            bcx.seal_block(adv_t);
            bcx.switch_to_block(adv_t);
            let en_next = bcx.ins().isub(ev, one);
            bcx.def_var(en, en_next);
            bcx.ins().jump(loop_t, &[]);
            bcx.seal_block(loop_t);
        }
        bcx.seal_block(done_t);
        bcx.switch_to_block(done_t);

        let en_w = bcx.use_var(en);
        let stv2 = bcx.use_var(st);
        let n = bcx.ins().isub(en_w, stv2);
        let has = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, n, 0);
        let then_b = bcx.create_block();
        let else_b = bcx.create_block();
        let end_b = bcx.create_block();
        let jv = bcx.append_block_param(end_b, types::I64);
        bcx.ins().brif(has, then_b, &[], else_b, &[]);
        bcx.seal_block(then_b);
        bcx.seal_block(else_b);
        bcx.switch_to_block(then_b);
        {
            let out = call_rt_m!(bcx, "rt.heap.alloc", vec![n]);
            let mv = c.module.declare_func_in_func(rtl_move_memory, bcx.func);
            let stv3 = bcx.use_var(st);
            let src = bcx.ins().iadd(pa, stv3);
            bcx.ins().call(mv, &[out, src, n]);
            let blk = call_rt_m!(
                bcx,
                "rt.heap.alloc",
                vec![bcx.ins().iconst(types::I64, STRING_BYTES)]
            );
            bcx.ins()
                .store(MemFlagsData::new(), out, blk, STRING_DATA_OFFSET);
            bcx.ins()
                .store(MemFlagsData::new(), n, blk, STRING_LEN_OFFSET);
            bcx.ins().jump(end_b, &[BlockArg::Value(blk)]);
        }
        bcx.switch_to_block(else_b);
        {
            let zero = bcx.ins().iconst(types::I64, 0);
            let blk = call_rt_m!(
                bcx,
                "rt.heap.alloc",
                vec![bcx.ins().iconst(types::I64, STRING_BYTES)]
            );
            bcx.ins()
                .store(MemFlagsData::new(), zero, blk, STRING_DATA_OFFSET);
            bcx.ins()
                .store(MemFlagsData::new(), zero, blk, STRING_LEN_OFFSET);
            bcx.ins().jump(end_b, &[BlockArg::Value(blk)]);
        }
        bcx.switch_to_block(end_b);
        bcx.seal_block(end_b);
        bcx.ins().return_(&[jv]);
        true
    });

    Ok(())
}
