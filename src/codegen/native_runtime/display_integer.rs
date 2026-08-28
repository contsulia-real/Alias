use super::*;

pub(super) fn emit_integer_display_shim<M: Module>(
    c: &mut Compiler<'_, M>,
    name: &str,
    signed: bool,
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
    let value = bcx.block_params(entry)[0];

    let alloc = c.import_runtime("rt.heap.alloc")?;
    let alloc_ref = c.module.declare_func_in_func(alloc, &mut bcx.func);
    let sz32 = bcx.ins().iconst(types::I64, 32);
    let buf_call = bcx.ins().call(alloc_ref, &[sz32]);
    let buf = first_result(&bcx, buf_call);
    let zero = bcx.ins().iconst(types::I64, 0);
    let neg = if signed {
        bcx.ins().icmp(IntCC::SignedLessThan, value, zero)
    } else {
        bcx.ins().iconst(types::I8, 0)
    };
    let neg_mag = bcx.ins().isub(zero, value);
    let mag = bcx.ins().select(neg, neg_mag, value);
    let pos = bcx.declare_var(types::I64);
    let end_pos = bcx.ins().iconst(types::I64, 32);
    bcx.def_var(pos, end_pos);
    let n = bcx.declare_var(types::I64);
    bcx.def_var(n, mag);
    let ten = bcx.ins().iconst(types::I64, 10);
    let ascii0 = bcx.ins().iconst(types::I64, b'0' as i64);
    let loop_b = bcx.create_block();
    let sign_b = bcx.create_block();
    let end_b = bcx.create_block();
    bcx.ins().jump(loop_b, &[]);
    bcx.switch_to_block(loop_b);
    {
        let cur = bcx.use_var(n);
        let p = bcx.use_var(pos);
        let digit = bcx.ins().urem(cur, ten);
        let ch = bcx.ins().iadd(digit, ascii0);
        let next_pos = bcx.ins().iadd_imm_s(p, -1);
        bcx.def_var(pos, next_pos);
        let addr = bcx.ins().iadd(buf, next_pos);
        let byte = bcx.ins().ireduce(types::I8, ch);
        bcx.ins().store(MemFlagsData::new(), byte, addr, 0);
        let rest = bcx.ins().udiv(cur, ten);
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
    if signed {
        let add_sign = bcx.create_block();
        bcx.ins().brif(neg, add_sign, &[], end_b, &[]);
        bcx.switch_to_block(add_sign);
        bcx.seal_block(add_sign);
        let p = bcx.use_var(pos);
        let next_pos = bcx.ins().iadd_imm_s(p, -1);
        bcx.def_var(pos, next_pos);
        let addr = bcx.ins().iadd(buf, next_pos);
        let minus = bcx.ins().iconst(types::I8, b'-' as i64);
        bcx.ins().store(MemFlagsData::new(), minus, addr, 0);
        bcx.ins().jump(end_b, &[]);
    } else {
        bcx.ins().jump(end_b, &[]);
    }
    bcx.switch_to_block(end_b);
    bcx.seal_block(end_b);
    let p = bcx.use_var(pos);
    let start = bcx.ins().iadd(buf, p);
    let c32 = bcx.ins().iconst(types::I64, 32);
    let len = bcx.ins().isub(c32, p);
    let sz16 = bcx.ins().iconst(types::I64, 16);
    let blk_call = bcx.ins().call(alloc_ref, &[sz16]);
    let blk = first_result(&bcx, blk_call);
    bcx.ins().store(MemFlagsData::new(), start, blk, 0);
    bcx.ins().store(MemFlagsData::new(), len, blk, 8);
    bcx.ins().return_(&[blk]);

    bcx.finalize(c.module.target_config());
    c.module
        .define_function(fid, &mut ctx)
        .map_err(|e| native_err(Span::default(), format!("内部: shim 定义失败 {e}")))
}
