use super::declare_runtime_shim;
use crate::codegen::emit::cells::first_result;
use crate::codegen::layout::{STRING_BYTES, STRING_DATA_OFFSET, STRING_LEN_OFFSET};
use crate::codegen::{native_err, Compiler};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{types, BlockArg, Function, InstBuilder, MemFlagsData, UserFuncName, Value};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};

const DISPLAY_BUFFER_BYTES: i64 = 64;
const FRACTION_DIGITS: i64 = 6;
const FRACTION_SCALE: i64 = 1_000_000;
const FRACTION_FIRST_DIVISOR: i64 = 100_000;
const ROUND_CARRY: i64 = 10_000_000;

fn static_string_block<M: Module>(
    c: &mut Compiler<'_, M>,
    bcx: &mut FunctionBuilder,
    alloc: FuncId,
    data_name: &str,
    len: i64,
) -> AliasResult<Value> {
    let id = c
        .module
        .declare_data(data_name, Linkage::Local, false, false)
        .map_err(|e| native_err(Span::default(), format!("内部: 数据声明失败 {e}")))?;
    let gv = c.module.declare_data_in_func(id, bcx.func);
    let addr = bcx.ins().symbol_value(c.ptr_ty, gv);
    let alloc_ref = c.module.declare_func_in_func(alloc, bcx.func);
    let sz = bcx.ins().iconst(types::I64, STRING_BYTES);
    let call = bcx.ins().call(alloc_ref, &[sz]);
    let blk = first_result(bcx, call);
    let n = bcx.ins().iconst(types::I64, len);
    bcx.ins()
        .store(MemFlagsData::new(), addr, blk, STRING_DATA_OFFSET);
    bcx.ins()
        .store(MemFlagsData::new(), n, blk, STRING_LEN_OFFSET);
    Ok(blk)
}

/// 以固定、无 libc 的科学计数格式显示有限浮点数。
///
/// 算法先把绝对值归一化到 `[1, 10)` 并累计十进制指数，再把尾数放大 10^6、
/// 加 0.5 后取整，得到最多六位小数。若舍入得到 10_000_000，则尾数回到
/// 1_000_000 且指数 +1。随后输出首位、六位小数并删除末尾 `0`，最后输出 `e`
/// 和指数。改变缩放/进位常数时必须同步这几个阶段，否则 9.9999995 一类边界会
/// 产生错误尾数或指数。
pub(super) fn emit_float_display_shim<M: Module>(
    c: &mut Compiler<'_, M>,
    name: &str,
    param_ty: cranelift_codegen::ir::Type,
) -> AliasResult<()> {
    let (fid, sig, contract) = declare_runtime_shim(c, name)?;
    if contract.params[0].ty.resolve(c.ptr_ty) != param_ty {
        return Err(native_err(
            Span::default(),
            format!("内部: 浮点 display '{}' 实现类型与契约不一致", name),
        ));
    }
    let mut ctx = Context::new();
    ctx.func = Function::with_name_signature(UserFuncName::user(0x77, fid.as_u32()), sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fbc);
    let entry = bcx.create_block();
    bcx.append_block_params_for_function_params(entry);
    bcx.switch_to_block(entry);
    bcx.seal_block(entry);
    let raw = bcx.block_params(entry)[0];
    let value = if param_ty == types::F32 {
        bcx.ins().fpromote(types::F64, raw)
    } else {
        raw
    };
    let alloc = c.import_runtime("rt.heap.alloc")?;

    let nan_b = bcx.create_block();
    let inf_b = bcx.create_block();
    let zero_b = bcx.create_block();
    let finite_b = bcx.create_block();
    let not_nan_b = bcx.create_block();
    let not_inf_b = bcx.create_block();
    let nan = bcx.ins().fcmp(FloatCC::NotEqual, value, value);
    bcx.ins().brif(nan, nan_b, &[], not_nan_b, &[]);
    bcx.seal_block(nan_b);
    bcx.seal_block(not_nan_b);

    bcx.switch_to_block(nan_b);
    let nan_blk = static_string_block(c, &mut bcx, alloc, "rt_nan", 3)?;
    bcx.ins().return_(&[nan_blk]);

    bcx.switch_to_block(not_nan_b);
    let zero_f = bcx.ins().f64const(0.0);
    let negative = bcx.ins().fcmp(FloatCC::LessThan, value, zero_f);
    let abs = bcx.ins().fabs(value);
    let inf_f = bcx.ins().f64const(f64::INFINITY);
    let inf = bcx.ins().fcmp(FloatCC::Equal, abs, inf_f);
    bcx.ins().brif(inf, inf_b, &[], not_inf_b, &[]);
    bcx.seal_block(inf_b);
    bcx.seal_block(not_inf_b);

    bcx.switch_to_block(inf_b);
    let neg_inf_b = bcx.create_block();
    let pos_inf_b = bcx.create_block();
    bcx.ins().brif(negative, neg_inf_b, &[], pos_inf_b, &[]);
    bcx.seal_block(neg_inf_b);
    bcx.seal_block(pos_inf_b);
    bcx.switch_to_block(neg_inf_b);
    let ninf_blk = static_string_block(c, &mut bcx, alloc, "rt_ninf", 4)?;
    bcx.ins().return_(&[ninf_blk]);
    bcx.switch_to_block(pos_inf_b);
    let inf_blk = static_string_block(c, &mut bcx, alloc, "rt_inf", 3)?;
    bcx.ins().return_(&[inf_blk]);

    bcx.switch_to_block(not_inf_b);
    let is_zero = bcx.ins().fcmp(FloatCC::Equal, abs, zero_f);
    bcx.ins().brif(is_zero, zero_b, &[], finite_b, &[]);
    bcx.seal_block(zero_b);
    bcx.seal_block(finite_b);
    bcx.switch_to_block(zero_b);
    let zero_blk = static_string_block(c, &mut bcx, alloc, "rt_zero", 1)?;
    bcx.ins().return_(&[zero_blk]);

    // 有限非零值归一化到 [1, 10)，exp 记录移动的小数位数。
    bcx.switch_to_block(finite_b);
    let norm = bcx.declare_var(types::F64);
    bcx.def_var(norm, abs);
    let exp = bcx.declare_var(types::I64);
    let exp0 = bcx.ins().iconst(types::I64, 0);
    bcx.def_var(exp, exp0);
    let hi_check = bcx.create_block();
    let hi_body = bcx.create_block();
    let low_check = bcx.create_block();
    let low_body = bcx.create_block();
    let normalized = bcx.create_block();
    bcx.ins().jump(hi_check, &[]);

    bcx.switch_to_block(hi_check);
    let nv = bcx.use_var(norm);
    let ten_f = bcx.ins().f64const(10.0);
    let ge_ten = bcx.ins().fcmp(FloatCC::GreaterThanOrEqual, nv, ten_f);
    bcx.ins().brif(ge_ten, hi_body, &[], low_check, &[]);
    bcx.seal_block(hi_body);
    bcx.switch_to_block(hi_body);
    let nv = bcx.use_var(norm);
    let divided = bcx.ins().fdiv(nv, ten_f);
    bcx.def_var(norm, divided);
    let ev = bcx.use_var(exp);
    let next_e = bcx.ins().iadd_imm_s(ev, 1);
    bcx.def_var(exp, next_e);
    bcx.ins().jump(hi_check, &[]);
    bcx.seal_block(hi_check);

    bcx.switch_to_block(low_check);
    let nv = bcx.use_var(norm);
    let one_f = bcx.ins().f64const(1.0);
    let lt_one = bcx.ins().fcmp(FloatCC::LessThan, nv, one_f);
    bcx.ins().brif(lt_one, low_body, &[], normalized, &[]);
    bcx.seal_block(low_body);
    bcx.seal_block(normalized);
    bcx.switch_to_block(low_body);
    let nv = bcx.use_var(norm);
    let multiplied = bcx.ins().fmul(nv, ten_f);
    bcx.def_var(norm, multiplied);
    let ev = bcx.use_var(exp);
    let prev_e = bcx.ins().iadd_imm_s(ev, -1);
    bcx.def_var(exp, prev_e);
    bcx.ins().jump(low_check, &[]);
    bcx.seal_block(low_check);

    // 六位小数四舍五入；9.9999995... 会发生十进制进位并推进指数。
    bcx.switch_to_block(normalized);
    let nv = bcx.use_var(norm);
    let scale_f = bcx.ins().f64const(FRACTION_SCALE as f64);
    let half_f = bcx.ins().f64const(0.5);
    let scaled_f = bcx.ins().fmul(nv, scale_f);
    let rounded_f = bcx.ins().fadd(scaled_f, half_f);
    let scaled0 = bcx.ins().fcvt_to_uint(types::I64, rounded_f);
    let carry = bcx.ins().icmp_imm_s(IntCC::Equal, scaled0, ROUND_CARRY);
    let scale_i = bcx.ins().iconst(types::I64, FRACTION_SCALE);
    let scaled = bcx.ins().select(carry, scale_i, scaled0);
    let ev = bcx.use_var(exp);
    let ev1 = bcx.ins().iadd_imm_s(ev, 1);
    let final_exp = bcx.ins().select(carry, ev1, ev);

    let alloc_ref = c.module.declare_func_in_func(alloc, bcx.func);
    let cap = bcx.ins().iconst(types::I64, DISPLAY_BUFFER_BYTES);
    let buf_call = bcx.ins().call(alloc_ref, &[cap]);
    let buf = first_result(&bcx, buf_call);
    let pos = bcx.declare_var(types::I64);
    let one_i = bcx.ins().iconst(types::I64, 1);
    let zero_i = bcx.ins().iconst(types::I64, 0);
    let sign_len = bcx.ins().select(negative, one_i, zero_i);
    bcx.def_var(pos, sign_len);
    let minus = bcx.ins().iconst(types::I8, b'-' as i64);
    bcx.ins().store(MemFlagsData::new(), minus, buf, 0);

    let scale = bcx.ins().iconst(types::I64, FRACTION_SCALE);
    let whole = bcx.ins().udiv(scaled, scale);
    let ascii0 = bcx.ins().iconst(types::I64, b'0' as i64);
    let first_ch = bcx.ins().iadd(whole, ascii0);
    let p0 = bcx.use_var(pos);
    let first_addr = bcx.ins().iadd(buf, p0);
    let first_byte = bcx.ins().ireduce(types::I8, first_ch);
    bcx.ins()
        .store(MemFlagsData::new(), first_byte, first_addr, 0);
    let p1 = bcx.ins().iadd_imm_s(p0, 1);
    bcx.def_var(pos, p1);
    let frac = bcx.declare_var(types::I64);
    let frac0 = bcx.ins().urem(scaled, scale);
    bcx.def_var(frac, frac0);
    let has_frac = bcx.ins().icmp_imm_s(IntCC::NotEqual, frac0, 0);
    let frac_b = bcx.create_block();
    let exponent_b = bcx.create_block();
    bcx.ins().brif(has_frac, frac_b, &[], exponent_b, &[]);
    bcx.seal_block(frac_b);
    bcx.switch_to_block(frac_b);
    let p = bcx.use_var(pos);
    let dot_addr = bcx.ins().iadd(buf, p);
    let dot = bcx.ins().iconst(types::I8, b'.' as i64);
    bcx.ins().store(MemFlagsData::new(), dot, dot_addr, 0);
    let after_dot = bcx.ins().iadd_imm_s(p, 1);
    bcx.def_var(pos, after_dot);
    let divisor = bcx.declare_var(types::I64);
    let div0 = bcx.ins().iconst(types::I64, FRACTION_FIRST_DIVISOR);
    bcx.def_var(divisor, div0);
    let digits_left = bcx.declare_var(types::I64);
    let digits = bcx.ins().iconst(types::I64, FRACTION_DIGITS);
    bcx.def_var(digits_left, digits);
    let digit_loop = bcx.create_block();
    let trim_check = bcx.create_block();
    bcx.ins().jump(digit_loop, &[]);
    bcx.switch_to_block(digit_loop);
    let fv = bcx.use_var(frac);
    let dv = bcx.use_var(divisor);
    let digit = bcx.ins().udiv(fv, dv);
    let rest = bcx.ins().urem(fv, dv);
    bcx.def_var(frac, rest);
    let ch = bcx.ins().iadd(digit, ascii0);
    let p = bcx.use_var(pos);
    let addr = bcx.ins().iadd(buf, p);
    let byte = bcx.ins().ireduce(types::I8, ch);
    bcx.ins().store(MemFlagsData::new(), byte, addr, 0);
    let next_pos = bcx.ins().iadd_imm_s(p, 1);
    bcx.def_var(pos, next_pos);
    let next_div = bcx.ins().udiv_imm_u(dv, 10);
    bcx.def_var(divisor, next_div);
    let left = bcx.use_var(digits_left);
    let next_left = bcx.ins().iadd_imm_s(left, -1);
    bcx.def_var(digits_left, next_left);
    let more = bcx.ins().icmp_imm_s(IntCC::NotEqual, next_left, 0);
    let digit_again = bcx.create_block();
    bcx.ins().brif(more, digit_again, &[], trim_check, &[]);
    bcx.seal_block(digit_again);
    bcx.switch_to_block(digit_again);
    bcx.ins().jump(digit_loop, &[]);
    bcx.seal_block(digit_loop);

    // 小数固定写满六位后从尾部回退，保留最短但确定的表示。
    bcx.switch_to_block(trim_check);
    let p = bcx.use_var(pos);
    let last_pos = bcx.ins().iadd_imm_s(p, -1);
    let last_addr = bcx.ins().iadd(buf, last_pos);
    let last = bcx.ins().load(types::I8, MemFlagsData::new(), last_addr, 0);
    let is_zero_digit = bcx.ins().icmp_imm_s(IntCC::Equal, last, b'0' as i64);
    let trim_body = bcx.create_block();
    bcx.ins()
        .brif(is_zero_digit, trim_body, &[], exponent_b, &[]);
    bcx.seal_block(trim_body);
    bcx.switch_to_block(trim_body);
    bcx.def_var(pos, last_pos);
    bcx.ins().jump(trim_check, &[]);
    bcx.seal_block(trim_check);

    bcx.switch_to_block(exponent_b);
    bcx.seal_block(exponent_b);
    let p = bcx.use_var(pos);
    let e_addr = bcx.ins().iadd(buf, p);
    let e_ch = bcx.ins().iconst(types::I8, b'e' as i64);
    bcx.ins().store(MemFlagsData::new(), e_ch, e_addr, 0);
    let p = bcx.ins().iadd_imm_s(p, 1);
    bcx.def_var(pos, p);
    let exp_neg = bcx.ins().icmp_imm_s(IntCC::SignedLessThan, final_exp, 0);
    let exp_sign_b = bcx.create_block();
    let exp_digits_b = bcx.create_block();
    bcx.ins().brif(exp_neg, exp_sign_b, &[], exp_digits_b, &[]);
    bcx.seal_block(exp_sign_b);
    bcx.switch_to_block(exp_sign_b);
    let p = bcx.use_var(pos);
    let addr = bcx.ins().iadd(buf, p);
    bcx.ins().store(MemFlagsData::new(), minus, addr, 0);
    let next_pos = bcx.ins().iadd_imm_s(p, 1);
    bcx.def_var(pos, next_pos);
    bcx.ins().jump(exp_digits_b, &[]);

    bcx.switch_to_block(exp_digits_b);
    bcx.seal_block(exp_digits_b);
    let neg_exp = bcx.ins().ineg(final_exp);
    let exp_mag = bcx.ins().select(exp_neg, neg_exp, final_exp);
    // 这里恰好只需要百/十/个位：有限非零 f64 归一化后的十进制指数范围为
    // -324..=308（f32 提升后更窄）。若输入域不再受 IEEE-754 f64 约束，必须先
    // 扩展指数编码，不能让更高位被 hundreds 当成单个十进制字符写出。
    let hundreds = bcx.ins().udiv_imm_u(exp_mag, 100);
    let has_hundreds = bcx.ins().icmp_imm_s(IntCC::NotEqual, hundreds, 0);
    let hundred_b = bcx.create_block();
    let tens_check_b = bcx.create_block();
    bcx.ins()
        .brif(has_hundreds, hundred_b, &[], tens_check_b, &[]);
    bcx.seal_block(hundred_b);
    bcx.switch_to_block(hundred_b);
    let p = bcx.use_var(pos);
    let addr = bcx.ins().iadd(buf, p);
    let ch = bcx.ins().iadd(hundreds, ascii0);
    let byte = bcx.ins().ireduce(types::I8, ch);
    bcx.ins().store(MemFlagsData::new(), byte, addr, 0);
    let next_pos = bcx.ins().iadd_imm_s(p, 1);
    bcx.def_var(pos, next_pos);
    bcx.ins().jump(tens_check_b, &[]);

    bcx.switch_to_block(tens_check_b);
    bcx.seal_block(tens_check_b);
    let rem100 = bcx.ins().urem_imm_u(exp_mag, 100);
    let tens = bcx.ins().udiv_imm_u(rem100, 10);
    let nonzero_tens = bcx.ins().icmp_imm_s(IntCC::NotEqual, tens, 0);
    let has_tens = bcx.ins().bor(has_hundreds, nonzero_tens);
    let tens_b = bcx.create_block();
    let ones_b = bcx.create_block();
    bcx.ins().brif(has_tens, tens_b, &[], ones_b, &[]);
    bcx.seal_block(tens_b);
    bcx.switch_to_block(tens_b);
    let p = bcx.use_var(pos);
    let addr = bcx.ins().iadd(buf, p);
    let ch = bcx.ins().iadd(tens, ascii0);
    let byte = bcx.ins().ireduce(types::I8, ch);
    bcx.ins().store(MemFlagsData::new(), byte, addr, 0);
    let next_pos = bcx.ins().iadd_imm_s(p, 1);
    bcx.def_var(pos, next_pos);
    bcx.ins().jump(ones_b, &[]);

    bcx.switch_to_block(ones_b);
    bcx.seal_block(ones_b);
    let ones = bcx.ins().urem_imm_u(exp_mag, 10);
    let p = bcx.use_var(pos);
    let addr = bcx.ins().iadd(buf, p);
    let ch = bcx.ins().iadd(ones, ascii0);
    let byte = bcx.ins().ireduce(types::I8, ch);
    bcx.ins().store(MemFlagsData::new(), byte, addr, 0);
    let final_len = bcx.ins().iadd_imm_s(p, 1);
    let alloc_ref = c.module.declare_func_in_func(alloc, bcx.func);
    let sz = bcx.ins().iconst(types::I64, STRING_BYTES);
    let blk_call = bcx.ins().call(alloc_ref, &[sz]);
    let blk = first_result(&bcx, blk_call);
    bcx.ins()
        .store(MemFlagsData::new(), buf, blk, STRING_DATA_OFFSET);
    bcx.ins()
        .store(MemFlagsData::new(), final_len, blk, STRING_LEN_OFFSET);
    bcx.ins().return_(&[blk]);

    bcx.finalize(c.module.target_config());
    c.define_verified_function(fid, &mut ctx, name)
}
