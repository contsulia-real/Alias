use super::declare_runtime_shim;
use crate::codegen::emit::cells::first_result;
use crate::codegen::layout::{STRING_BYTES, STRING_DATA_OFFSET, STRING_LEN_OFFSET};
use crate::codegen::Compiler;
use crate::AliasResult;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, Function, InstBuilder, MemFlagsData, UserFuncName};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::Module;

// 反向写入需要容纳 u64 的 20 位十进制数字和可选负号；额外余量让 cursor 始终从
// 缓冲区末端开始。分配、初始 cursor 与最终长度必须共同引用此值，否则会产生越界写
// 或把未初始化前缀纳入 string。
const INTEGER_BUFFER_BYTES: i64 = 32;

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
    let alloc_ref = c.module.declare_func_in_func(alloc, bcx.func);
    let buffer_bytes = bcx.ins().iconst(types::I64, INTEGER_BUFFER_BYTES);
    let buf_call = bcx.ins().call(alloc_ref, &[buffer_bytes]);
    let buf = first_result(&bcx, buf_call);
    let zero = bcx.ins().iconst(types::I64, 0);
    let neg = if signed {
        bcx.ins().icmp(IntCC::SignedLessThan, value, zero)
    } else {
        bcx.ins().iconst(types::I8, 0)
    };
    // `0 - i64::MIN` 的位模式仍是 2^63；后续必须按无符号 magnitude 做除余，才能
    // 正确显示最小有符号整数。改成 sdiv/srem 会让该边界溢出或生成错误数字。
    let neg_mag = bcx.ins().isub(zero, value);
    let mag = bcx.ins().select(neg, neg_mag, value);
    let pos = bcx.declare_var(types::I64);
    let end_pos = bcx.ins().iconst(types::I64, INTEGER_BUFFER_BYTES);
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
    let buffer_bytes = bcx.ins().iconst(types::I64, INTEGER_BUFFER_BYTES);
    let len = bcx.ins().isub(buffer_bytes, p);
    let string_bytes = bcx.ins().iconst(types::I64, STRING_BYTES);
    let blk_call = bcx.ins().call(alloc_ref, &[string_bytes]);
    let blk = first_result(&bcx, blk_call);
    bcx.ins()
        .store(MemFlagsData::new(), start, blk, STRING_DATA_OFFSET);
    bcx.ins()
        .store(MemFlagsData::new(), len, blk, STRING_LEN_OFFSET);
    bcx.ins().return_(&[blk]);

    bcx.finalize(c.module.target_config());
    c.define_verified_function(fid, &mut ctx, name)
}
