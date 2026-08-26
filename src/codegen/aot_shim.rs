// ---------------------------------------------------------------------------
// aot_shim — AOT 运行时 shim 区: kernel32 符号契约的 IR 实现。
// 与 host.rs (JIT 宿主) 逐符号对齐; 发射顺序必须先于 compile_program
// (用户代码 Import 声明与同名 Export 定义经 cranelift-module 合并)。
// ---------------------------------------------------------------------------
use super::*;
use crate::codegen::Compiler;
use crate::{AliasResult, Span};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::Function;
use cranelift_codegen::ir::{BlockArg, InstBuilder, StackSlotData, StackSlotKind, Value};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::HashMap;

/// ASCII 空白字符集 (trim 判定)
const TRIM_SET: &[u8] = b" \t\r\n";
// ---------------------------------------------------------------------------
// AOT 运行时 shim 区 — 与 JIT 宿主函数逐符号对齐 (模块头契约)
//
// 依赖面: kernel32.lib (GetStdHandle/WriteFile/ExitProcess/HeapAlloc/
// GetProcessHeap/RtlMoveMemory)。
// 无 CRT 字符串布局依赖; 泄漏即 GC。
//
// 发射顺序约束: 必须先于 compile_program — 用户代码的 Import 声明与
// 同名 Export 定义经 cranelift-module 符号合并为同一 FuncId。
// ---------------------------------------------------------------------------

/// AOT 外部符号集 (链接期经导入库解析)
struct AotExterns {
    get_std_handle: FuncId,
    write_file: FuncId,
    exit_process: FuncId,
}

/// shim 发射样板: 声明 Export 函数 → body 在 entry 上发射 → 定义。
/// body 求值为 true 表示已发射终结指令; false 则按返回类型补默认 return 0。
macro_rules! shim {
    ($c:expr, $name:expr, |$bcx:ident, $a:ident| $body:block) => {{
        let (__fid, __sig, __contract) = declare_runtime_shim($c, $name)?;
        let __ret = __contract.ret.map(|v| v.ty.resolve($c.ptr_ty));
        let mut __ctx = Context::new();
        __ctx.func =
            Function::with_name_signature(UserFuncName::user(0x77, __fid.as_u32()), __sig.clone());
        let mut __fbc = FunctionBuilderContext::new();
        let mut $bcx = FunctionBuilder::new(&mut __ctx.func, &mut __fbc);
        let __entry = $bcx.create_block();
        $bcx.append_block_params_for_function_params(__entry);
        $bcx.switch_to_block(__entry);
        $bcx.seal_block(__entry);
        let $a: Vec<Value> = $bcx.block_params(__entry).to_vec();
        #[allow(unused_variables)]
        let __terminated: bool = $body;
        if !__terminated {
            match __ret {
                Some(t) => {
                    let z = $bcx.ins().iconst(t, 0);
                    $bcx.ins().return_(&[z]);
                }
                None => {
                    $bcx.ins().return_(&[]);
                }
            }
        }

        $c.module
            .define_function(__fid, &mut __ctx)
            .map_err(|e| native_err(Span::default(), format!("内部: shim 定义失败 {e}")))?;
    }};
}

fn declare_runtime_shim<M: Module>(
    c: &mut Compiler<'_, M>,
    name: &str,
) -> AliasResult<(
    FuncId,
    cranelift_codegen::ir::Signature,
    &'static RuntimeContract,
)> {
    let contract = runtime_contract(name)?;
    if !contract.backends.aot {
        return Err(native_err(
            Span::default(),
            format!("内部: runtime '{}' 没有 AOT 契约", contract.symbol),
        ));
    }
    let sig = contract.signature(c.cc, c.ptr_ty);
    let fid = c
        .module
        .declare_function(contract.symbol, Linkage::Export, &sig)
        .map_err(|e| native_err(Span::default(), format!("内部: shim 声明失败 {e}")))?;
    if !c.runtime_defined.insert(contract.symbol) {
        return Err(native_err(
            Span::default(),
            format!("内部: AOT shim 重复定义 '{}'", contract.symbol),
        ));
    }
    Ok((fid, sig, contract))
}

fn validate_aot_runtime_coverage<M: Module>(c: &Compiler<'_, M>) -> AliasResult<()> {
    validate_contract_table().map_err(|msg| native_err(Span::default(), msg))?;
    let expected = RUNTIME_CONTRACTS
        .iter()
        .filter(|contract| contract.backends.aot)
        .map(|contract| contract.symbol)
        .collect::<std::collections::HashSet<_>>();
    if c.runtime_defined != expected {
        let missing = expected
            .difference(&c.runtime_defined)
            .copied()
            .collect::<Vec<_>>();
        let extra = c
            .runtime_defined
            .difference(&expected)
            .copied()
            .collect::<Vec<_>>();
        return Err(native_err(
            Span::default(),
            format!("内部: AOT shim 与 runtime 契约表不一致，缺失 {missing:?}，多余 {extra:?}"),
        ));
    }
    Ok(())
}

/// trim 字节成员判定: 空格/\t/\r/\n 四路比较或链 (冻结字符集)。
pub(crate) fn emit_is_trim_byte(bcx: &mut FunctionBuilder, b: Value) -> Value {
    let mut acc = bcx.ins().icmp_imm_s(IntCC::Equal, b, TRIM_SET[0] as i64);
    for &t in &TRIM_SET[1..] {
        let e = bcx.ins().icmp_imm_s(IntCC::Equal, b, t as i64);
        acc = bcx.ins().bor(acc, e);
    }
    acc
}

/// ASCII 大小写映射 shim (upper/lower 共用体): 逐字节范围 icmp + select
/// 平移, 写入新缓冲; 空串短路产出 null 数据指针块 (§五契约)。
/// 与 JIT 宿主 str_map_ascii 同语义 — 双后端逐字节对齐。
pub(crate) fn emit_case_shim<M: Module>(
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

    let pa = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 0);
    let la = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
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
            let r = c.module.declare_func_in_func(alloc_f, &mut bcx.func);
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
            bcx.seal_block(loop_b); // 前驱齐备: 入口跳转 + 回边
        }
        bcx.seal_block(done_b);
        bcx.switch_to_block(done_b);
        let blk = {
            let r = c.module.declare_func_in_func(alloc_f, &mut bcx.func);
            let sz = bcx.ins().iconst(types::I64, 16);
            let inst = bcx.ins().call(r, &[sz]);
            first_result(&bcx, inst)
        };
        bcx.ins().store(MemFlagsData::new(), out, blk, 0);
        bcx.ins().store(MemFlagsData::new(), la, blk, 8);
        bcx.ins().jump(end_b, &[BlockArg::Value(blk)]);
    }
    bcx.switch_to_block(else_b);
    {
        let zero = bcx.ins().iconst(types::I64, 0);
        let blk = {
            let f = c.import_runtime("rt.heap.alloc")?;
            let r = c.module.declare_func_in_func(f, &mut bcx.func);
            let sz = bcx.ins().iconst(types::I64, 16);
            let inst = bcx.ins().call(r, &[sz]);
            first_result(&bcx, inst)
        };
        bcx.ins().store(MemFlagsData::new(), zero, blk, 0);
        bcx.ins().store(MemFlagsData::new(), zero, blk, 8);
        bcx.ins().jump(end_b, &[BlockArg::Value(blk)]);
    }
    bcx.switch_to_block(end_b);
    bcx.seal_block(end_b);
    bcx.ins().return_(&[jv]);
    bcx.finalize(c.module.target_config());
    c.module
        .define_function(fid, &mut ctx)
        .map_err(|e| native_err(Span::default(), format!("内部: shim 定义失败 {e}")))
}

/// I64/U64 十进制 display。幅度始终用无符号除法，因此 i64::MIN 的
/// 二补码幅度 2^63 不会落入有符号除法陷阱。
fn emit_integer_display_shim<M: Module>(
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
    let gv = c.module.declare_data_in_func(id, &mut bcx.func);
    let addr = bcx.ins().symbol_value(c.ptr_ty, gv);
    let alloc_ref = c.module.declare_func_in_func(alloc, &mut bcx.func);
    let sz = bcx.ins().iconst(types::I64, 16);
    let call = bcx.ins().call(alloc_ref, &[sz]);
    let blk = first_result(bcx, call);
    let n = bcx.ins().iconst(types::I64, len);
    bcx.ins().store(MemFlagsData::new(), addr, blk, 0);
    bcx.ins().store(MemFlagsData::new(), n, blk, 8);
    Ok(blk)
}

/// 纯 IR 浮点显示：规范化为一位整数、六位四舍五入有效小数和十进制指数，
/// 例如 12.34 -> 1.234e1。F32 先按其真实位宽升档，双后端共用同一算法。
fn emit_float_display_shim<M: Module>(
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

    bcx.switch_to_block(normalized);
    let nv = bcx.use_var(norm);
    let million_f = bcx.ins().f64const(1_000_000.0);
    let half_f = bcx.ins().f64const(0.5);
    let scaled_f = bcx.ins().fmul(nv, million_f);
    let rounded_f = bcx.ins().fadd(scaled_f, half_f);
    let scaled0 = bcx.ins().fcvt_to_uint(types::I64, rounded_f);
    let carry = bcx.ins().icmp_imm_s(IntCC::Equal, scaled0, 10_000_000);
    let one_m = bcx.ins().iconst(types::I64, 1_000_000);
    let scaled = bcx.ins().select(carry, one_m, scaled0);
    let ev = bcx.use_var(exp);
    let ev1 = bcx.ins().iadd_imm_s(ev, 1);
    let final_exp = bcx.ins().select(carry, ev1, ev);

    let alloc_ref = c.module.declare_func_in_func(alloc, &mut bcx.func);
    let cap = bcx.ins().iconst(types::I64, 64);
    let buf_call = bcx.ins().call(alloc_ref, &[cap]);
    let buf = first_result(&bcx, buf_call);
    let pos = bcx.declare_var(types::I64);
    let one_i = bcx.ins().iconst(types::I64, 1);
    let zero_i = bcx.ins().iconst(types::I64, 0);
    let sign_len = bcx.ins().select(negative, one_i, zero_i);
    bcx.def_var(pos, sign_len);
    let minus = bcx.ins().iconst(types::I8, b'-' as i64);
    bcx.ins().store(MemFlagsData::new(), minus, buf, 0);

    let million = bcx.ins().iconst(types::I64, 1_000_000);
    let whole = bcx.ins().udiv(scaled, million);
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
    let frac0 = bcx.ins().urem(scaled, million);
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
    let div0 = bcx.ins().iconst(types::I64, 100_000);
    bcx.def_var(divisor, div0);
    let digits_left = bcx.declare_var(types::I64);
    let six = bcx.ins().iconst(types::I64, 6);
    bcx.def_var(digits_left, six);
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
    let alloc_ref = c.module.declare_func_in_func(alloc, &mut bcx.func);
    let sz = bcx.ins().iconst(types::I64, 16);
    let blk_call = bcx.ins().call(alloc_ref, &[sz]);
    let blk = first_result(&bcx, blk_call);
    bcx.ins().store(MemFlagsData::new(), buf, blk, 0);
    bcx.ins().store(MemFlagsData::new(), final_len, blk, 8);
    bcx.ins().return_(&[blk]);

    bcx.finalize(c.module.target_config());
    c.module
        .define_function(fid, &mut ctx)
        .map_err(|e| native_err(Span::default(), format!("内部: shim 定义失败 {e}")))
}

/// span 表数据段回填: (line, col) u32 小端对 — abort shim 运行时查表。
pub(crate) fn define_span_data<M: Module>(
    c: &mut Compiler<'_, M>,
    table: &[(u32, u32)],
) -> AliasResult<()> {
    let id = c
        .module
        .declare_data("alias_span_table", Linkage::Local, false, false)
        .map_err(|e| native_err(Span::default(), format!("内部: span 段声明失败 {e}")))?;
    let mut bytes = Vec::with_capacity(table.len() * 8);
    for (line, col) in table {
        bytes.extend_from_slice(&line.to_le_bytes());
        bytes.extend_from_slice(&col.to_le_bytes());
    }
    let mut desc = cranelift_module::DataDescription::new();
    desc.define(bytes.into());
    c.module
        .define_data(id, &desc)
        .map_err(|e| native_err(Span::default(), format!("内部: span 段定义失败 {e}")))
}

/// span-ID 中止 shim (div/越界/pop 三路共用): 查 span 表 → 分段
/// WriteFile 拼「错误 @ L:C — {suffix}」到 stderr → ExitProcess(1)。
fn emit_span_abort<M: Module>(
    c: &mut Compiler<'_, M>,
    name: &str,
    ext: &AotExterns,
    span_data: cranelift_module::DataId,
    static_ids: &HashMap<&str, cranelift_module::DataId>,
    suffix: &str,
    suffix_len: i64,
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

    let base = {
        let gv = c.module.declare_data_in_func(span_data, &mut bcx.func);
        bcx.ins().symbol_value(c.ptr_ty, gv)
    };
    let id64 = bcx.ins().uextend(types::I64, a[0]);
    let off = bcx.ins().imul_imm_s(id64, 8);
    let laddr = bcx.ins().iadd(base, off);
    let line = bcx.ins().load(types::I32, MemFlagsData::new(), laddr, 0);
    let col = bcx.ins().load(types::I32, MemFlagsData::new(), laddr, 4);

    macro_rules! w_str {
        ($bcx:expr, $err:expr, $data:expr, $len:expr) => {{
            let __wa =
                $bcx.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
            let __wa_addr = $bcx.ins().stack_addr(c.ptr_ty, __wa, 0);
            let __null = $bcx.ins().iconst(c.ptr_ty, 0);
            let __gv = c
                .module
                .declare_data_in_func(static_ids[$data], &mut $bcx.func);
            let __addr = $bcx.ins().symbol_value(c.ptr_ty, __gv);
            let __len = $bcx.ins().iconst(types::I32, $len);
            let __wf = c
                .module
                .declare_func_in_func(ext.write_file, &mut $bcx.func);
            $bcx.ins()
                .call(__wf, &[$err, __addr, __len, __wa_addr, __null]);
        }};
    }
    macro_rules! w_dec {
        ($bcx:expr, $err:expr, $v:expr) => {{
            let __f = c.import_runtime("rt.write.dec")?;
            let __r = c.module.declare_func_in_func(__f, &mut $bcx.func);
            let __args = [$err, bcx.ins().uextend(types::I64, $v)];
            $bcx.ins().call(__r, &__args);
        }};
    }
    let err_args = [bcx.ins().iconst(types::I32, -12)];
    let err = {
        let r = c
            .module
            .declare_func_in_func(ext.get_std_handle, &mut bcx.func);
        let inst = bcx.ins().call(r, &err_args);
        first_result(&bcx, inst)
    };
    w_str!(bcx, err, "rt_msg_prefix", 9); // 「错误 @ 」 = 6+1+1+1 字节
    w_dec!(bcx, err, line);
    w_str!(bcx, err, "rt_colon", 1);
    w_dec!(bcx, err, col);
    w_str!(bcx, err, suffix, suffix_len);
    let code1 = bcx.ins().iconst(types::I32, 1);
    let ep = c
        .module
        .declare_func_in_func(ext.exit_process, &mut bcx.func);
    bcx.ins().call(ep, &[code1]);
    bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO); // 不可达兜底

    bcx.finalize(c.module.target_config());
    c.module
        .define_function(fid, &mut ctx)
        .map_err(|e| native_err(Span::default(), format!("内部: shim 定义失败 {e}")))
}

pub(crate) fn emit_runtime_shims<M: Module>(c: &mut Compiler<'_, M>) -> AliasResult<()> {
    let ext = AotExterns {
        get_std_handle: c.import_external("GetStdHandle", &[types::I32], Some(c.ptr_ty))?,
        write_file: c.import_external(
            "WriteFile",
            &[c.ptr_ty, c.ptr_ty, types::I32, c.ptr_ty, c.ptr_ty],
            Some(types::I32),
        )?,
        exit_process: c.import_external("ExitProcess", &[types::I32], None)?,
    };
    let heap_alloc = c.import_external(
        "HeapAlloc",
        &[c.ptr_ty, types::I32, types::I64],
        Some(c.ptr_ty),
    )?;
    let get_process_heap = c.import_external("GetProcessHeap", &[], Some(c.ptr_ty))?;
    let rtl_move_memory =
        c.import_external("RtlMoveMemory", &[c.ptr_ty, c.ptr_ty, types::I64], None)?;

    // span 表槽位先行声明 (abort shim 引用; 内容编译后由 define_span_data 回填)
    let span_data = c
        .module
        .declare_data("alias_span_table", Linkage::Local, false, false)
        .map_err(|e| native_err(Span::default(), format!("内部: span 段声明失败 {e}")))?;

    // ---- 静态数据: display 常量与中止消息分段 ----
    let mut statics: Vec<(&str, &[u8])> = vec![
        ("rt_nl", b"\n"),
        ("rt_true", b"true"),
        ("rt_false", b"false"),
        ("rt_unit", b"()"),
        ("rt_func", b"<func>"),
        ("rt_struct", b"<struct>"),
        ("rt_array", b"<array>"),
        ("rt_ok", b"<ok>"),
        ("rt_err", b"<err>"),
        ("rt_nan", b"NaN"),
        ("rt_inf", b"inf"),
        ("rt_ninf", b"-inf"),
        ("rt_zero", b"0"),
        ("rt_msg_prefix", "错误 @ ".as_bytes()), // 9 字节
        ("rt_colon", b":"),
        ("rt_msg_suffix", " — 除以零\n".as_bytes()), // 15 字节
        ("rt_oob_suffix", " — 下标越界\n".as_bytes()), // 18 字节
        ("rt_pop_suffix", " — pop 空数组\n".as_bytes()), // 19 字节
        ("rt_conv_suffix", " — 转换越界\n".as_bytes()), // 18 字节
    ];
    let mut static_ids: HashMap<&str, cranelift_module::DataId> = HashMap::new();
    for (name, bytes) in statics.drain(..) {
        let id = c
            .module
            .declare_data(name, Linkage::Local, false, false)
            .map_err(|e| native_err(Span::default(), format!("内部: 数据段声明失败 {e}")))?;
        let mut desc = cranelift_module::DataDescription::new();
        desc.define(bytes.to_vec().into());
        c.module
            .define_data(id, &desc)
            .map_err(|e| native_err(Span::default(), format!("内部: 数据段定义失败 {e}")))?;
        static_ids.insert(name, id);
    }

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
    macro_rules! call_ext_m {
        ($bcx:expr, $fid:expr, $args:expr) => {{
            let __r = c.module.declare_func_in_func($fid, &mut $bcx.func);
            let __args = $args;
            let __inst = $bcx.ins().call(__r, &__args);
            first_result(&$bcx, __inst)
        }};
    }

    // ---- rt.heap.alloc(n:I64) -> PTR ----
    shim!(c, "rt.heap.alloc", |bcx, a| {
        let h = call_ext_m!(bcx, get_process_heap, vec![]);
        let flags = bcx.ins().iconst(types::I32, 8); // HEAP_ZERO_MEMORY
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

    // ---- 分配器: cell / env / globals / closure ----
    shim!(c, "alias.cell.new", |bcx, a| {
        // 参数是调用端按 size_align 算出的字节数；HeapAlloc 的
        // HEAP_ZERO_MEMORY 保证绑定/结构体存储区初始清零，值由调用端写入。
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
    // globals.new (Phase 3a): 入参为字节数 — 混型宽度槽区的布局由调用方计算
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

    // ---- 字符串块 {data_ptr, len}; 字节复制进新缓冲 ----
    shim!(c, "alias.str.new", |bcx, a| {
        let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![bcx.ins().iconst(types::I64, 16)]);
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
            let mv = c
                .module
                .declare_func_in_func(rtl_move_memory, &mut bcx.func);
            bcx.ins().call(mv, &[buf, a[0], len64]);
            bcx.ins().store(MemFlagsData::new(), buf, blk, 0);
            bcx.ins().jump(end_b, &[]);
        }
        bcx.switch_to_block(else_b);
        {
            let zero = bcx.ins().iconst(types::I64, 0);
            bcx.ins().store(MemFlagsData::new(), zero, blk, 0);
            bcx.ins().jump(end_b, &[]);
        }
        bcx.switch_to_block(end_b);
        bcx.seal_block(end_b);
        bcx.ins().store(MemFlagsData::new(), len64, blk, 8);
        bcx.ins().return_(&[blk]);
        true
    });

    shim!(c, "alias.str.concat", |bcx, a| {
        let pa = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 0);
        let la = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        let pb = bcx.ins().load(types::I64, MemFlagsData::new(), a[1], 0);
        let lb = bcx.ins().load(types::I64, MemFlagsData::new(), a[1], 8);
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
        let mv = c
            .module
            .declare_func_in_func(rtl_move_memory, &mut bcx.func);
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
        let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![bcx.ins().iconst(types::I64, 16)]);
        bcx.ins().store(MemFlagsData::new(), out_word, blk, 0);
        bcx.ins().store(MemFlagsData::new(), total, blk, 8);
        bcx.ins().return_(&[blk]);
        true
    });

    shim!(c, "alias.str.cmp", |bcx, a| {
        let pa = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 0);
        let la = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        let pb = bcx.ins().load(types::I64, MemFlagsData::new(), a[1], 0);
        let lb = bcx.ins().load(types::I64, MemFlagsData::new(), a[1], 8);
        let min_len = bcx.ins().smin(la.clone(), lb.clone());
        // 逐字节比较循环 (无 CRT memcmp): 首个差异字节定序
        let i = bcx.declare_var(types::I64);
        let i0 = bcx.ins().iconst(types::I64, 0);
        bcx.def_var(i, i0);
        let m1w = bcx.ins().iconst(types::I64, -1);
        let p1w = bcx.ins().iconst(types::I64, 1);
        let zw = bcx.ins().iconst(types::I64, 0);
        let loop_b = bcx.create_block();
        bcx.ins().jump(loop_b, &[]);
        // header 待回边齐备后再封 - ssa 约束
        bcx.switch_to_block(loop_b);
        {
            let iv = bcx.use_var(i);
            let in_range = bcx.ins().icmp(IntCC::UnsignedLessThan, iv, min_len.clone());
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
                let same = bcx.ins().icmp(IntCC::Equal, b_a.clone(), b_b.clone());
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

    // ---- 内建字符串方法 (Phase 2c): 与 JIT 宿主函数同符号同契约 ----
    shim!(c, "alias.str.len", |bcx, a| {
        let l = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        let t = bcx.ins().ireduce(types::I32, l);
        bcx.ins().return_(&[t]);
        true
    });
    emit_case_shim(c, "alias.str.upper", b'a' as i64, b'z' as i64, -32)?;
    emit_case_shim(c, "alias.str.lower", b'A' as i64, b'Z' as i64, 32)?;

    // trim: 双边界扫描 (首尾剥离 空格/\t/\r/\n) → 子块复制新块;
    // 全空白/空串结果的 data_ptr 恒 null (§五契约)
    shim!(c, "alias.str.trim", |bcx, a| {
        let pa = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 0);
        let la = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        let st = bcx.declare_var(types::I64);
        let st0 = bcx.ins().iconst(types::I64, 0);
        bcx.def_var(st, st0);
        let en = bcx.declare_var(types::I64);
        bcx.def_var(en, la);
        let one = bcx.ins().iconst(types::I64, 1);

        // 前向扫描: st 越过前导空白 — 条件入体 + 无条件回边
        // (与 alias.str.upper/lower 循环同构的已验证形状)
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
            bcx.seal_block(loop_l); // 前驱齐备: 入口跳转 + 回边
        }
        bcx.seal_block(done_l);
        bcx.switch_to_block(done_l);

        // 反向扫描: en 回退越过尾部空白 (不低于 st)
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
            bcx.seal_block(loop_t); // 前驱齐备: 入口跳转 + 回边
        }
        bcx.seal_block(done_t);
        bcx.switch_to_block(done_t);

        // n = en - st; 非空则复制子块, 空则 null 数据指针块
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
            let mv = c
                .module
                .declare_func_in_func(rtl_move_memory, &mut bcx.func);
            let stv3 = bcx.use_var(st);
            let src = bcx.ins().iadd(pa, stv3);
            bcx.ins().call(mv, &[out, src, n]);
            let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![bcx.ins().iconst(types::I64, 16)]);
            bcx.ins().store(MemFlagsData::new(), out, blk, 0);
            bcx.ins().store(MemFlagsData::new(), n, blk, 8);
            bcx.ins().jump(end_b, &[BlockArg::Value(blk)]);
        }
        bcx.switch_to_block(else_b);
        {
            let zero = bcx.ins().iconst(types::I64, 0);
            let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![bcx.ins().iconst(types::I64, 16)]);
            bcx.ins().store(MemFlagsData::new(), zero, blk, 0);
            bcx.ins().store(MemFlagsData::new(), zero, blk, 8);
            bcx.ins().jump(end_b, &[BlockArg::Value(blk)]);
        }
        bcx.switch_to_block(end_b);
        bcx.seal_block(end_b);
        bcx.ins().return_(&[jv]);
        true
    });

    // ---- 内建数组方法 (Phase 2d): 与 JIT 宿主函数同符号同契约 ----
    // 头块 {data_ptr, len, cap} 24 字节; pop 空守卫在发射层 — shim 按契约假定非空
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
        bcx.ins().store(MemFlagsData::new(), zero, hdr, 8);
        bcx.ins().store(MemFlagsData::new(), cap64, hdr, 16);
        bcx.ins().return_(&[hdr]);
        true
    });
    shim!(c, "alias.arr.len", |bcx, a| {
        let l = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        let t = bcx.ins().ireduce(types::I32, l);
        bcx.ins().return_(&[t]);
        true
    });
    // push: 满 len==cap → 新缓冲 2x (空 cap 取 1) + RtlMoveMemory 复制旧元素,
    // 头块原地换 data_ptr/cap — 所有别名共享可见; 随后尾插 + len+1
    shim!(c, "alias.arr.push", |bcx, a| {
        let hdr = a[0];
        let dp0 = bcx.ins().load(types::I64, MemFlagsData::new(), hdr, 0);
        let len = bcx.ins().load(types::I64, MemFlagsData::new(), hdr, 8);
        let cap = bcx.ins().load(types::I64, MemFlagsData::new(), hdr, 16);
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
            let zero = bcx.ins().iconst(types::I64, 0);
            let one = bcx.ins().iconst(types::I64, 1);
            let is_empty = bcx.ins().icmp_imm_s(IntCC::Equal, cap, 0);
            let doubled = bcx.ins().imul_imm_s(cap, 2);
            let new_cap = bcx.ins().select(is_empty, one, doubled);
            let bytes = bcx.ins().imul_imm_s(new_cap, 8);
            let grown = call_rt_m!(bcx, "rt.heap.alloc", vec![bytes]);
            let mv = c
                .module
                .declare_func_in_func(rtl_move_memory, &mut bcx.func);
            let copy_bytes = bcx.ins().imul_imm_s(len, 8);
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
            bcx.ins().store(MemFlagsData::new(), new_cap, hdr, 16);
            bcx.ins().jump(join_b, &[BlockArg::Value(grown)]);
        }
        bcx.switch_to_block(ok_b);
        bcx.ins().jump(join_b, &[BlockArg::Value(dp0)]);

        bcx.switch_to_block(join_b);
        bcx.seal_block(join_b);
        let slot = bcx.ins().imul_imm_s(len, 8);
        let addr = bcx.ins().iadd(jdp, slot);
        bcx.ins().store(MemFlagsData::new(), a[1], addr, 0);
        let one = bcx.ins().iconst(types::I64, 1);
        let len1 = bcx.ins().iadd(len, one);
        bcx.ins().store(MemFlagsData::new(), len1, hdr, 8);
        false
    });
    shim!(c, "alias.arr.pop", |bcx, a| {
        let hdr = a[0];
        let len = bcx.ins().load(types::I64, MemFlagsData::new(), hdr, 8);
        let new_len = bcx.ins().iadd_imm_s(len, -1);
        bcx.ins().store(MemFlagsData::new(), new_len, hdr, 8);
        let dp = bcx.ins().load(types::I64, MemFlagsData::new(), hdr, 0);
        let slot = bcx.ins().imul_imm_s(new_len, 8);
        let addr = bcx.ins().iadd(dp, slot);
        let v = bcx.ins().load(types::I64, MemFlagsData::new(), addr, 0);
        bcx.ins().return_(&[v]);
        true
    });

    // ---- display 家族 (Value::display 逐字节规则) ----
    // rt.write.dec(h:PTR, v:I64 非负): 十进制写入句柄 (abort 消息行:列共用)
    shim!(c, "rt.write.dec", |bcx, a| {
        let buf = call_rt_m!(bcx, "rt.heap.alloc", vec![bcx.ins().iconst(types::I64, 24)]);
        let ten = bcx.ins().iconst(types::I64, 10);
        let digits = bcx.ins().iconst(types::I64, 48);
        let pos = bcx.declare_var(types::I64);
        let p23 = bcx.ins().iconst(types::I64, 23);
        bcx.def_var(pos, p23);
        let n = bcx.declare_var(types::I64);
        bcx.def_var(n, a[1]);
        let loop_b = bcx.create_block();
        let end_b = bcx.create_block();
        bcx.ins().jump(loop_b, &[]);
        // header 待回边齐备后再封 - ssa 约束
        bcx.switch_to_block(loop_b);
        {
            let cur = bcx.use_var(n);
            let p = bcx.use_var(pos);
            let d = bcx.ins().srem(cur, ten.clone());
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
            bcx.seal_block(loop_b); // 前驱齐备: 入口+回边
        }
        bcx.switch_to_block(end_b);
        bcx.seal_block(end_b);
        {
            let start = {
                let p = bcx.use_var(pos);
                let one = bcx.ins().iconst(types::I64, 1);
                let start_off = bcx.use_var(pos);
                let sv = bcx.ins().iadd(buf, start_off);
                sv
            };
            let len = {
                let p = bcx.use_var(pos);
                let c23 = bcx.ins().iconst(types::I64, 23);
                bcx.ins().isub(c23, p)
            };
            let ss =
                bcx.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
            let wa = bcx.ins().stack_addr(c.ptr_ty, ss, 0);
            let null = bcx.ins().iconst(c.ptr_ty, 0);
            let wf = c.module.declare_func_in_func(ext.write_file, &mut bcx.func);
            let len32 = bcx.ins().ireduce(types::I32, len);
            let wf_args = [a[0], start, len32, wa, null];
            bcx.ins().call(wf, &wf_args);
        }
        false
    });
    // display.int: IR 手写十进制转换 (无 CRT 依赖; i64 幅度容纳 i32::MIN 取负)
    shim!(c, "alias.display.int", |bcx, a| {
        let buf = call_rt_m!(bcx, "rt.heap.alloc", vec![bcx.ins().iconst(types::I64, 24)]);
        let v64 = bcx.ins().sextend(types::I64, a[0]);
        let zero = bcx.ins().iconst(types::I64, 0);
        let neg = bcx
            .ins()
            .icmp(IntCC::SignedLessThan, v64.clone(), zero.clone());
        let neg_mag = bcx.ins().isub(zero, v64);
        let mag = bcx.ins().select(neg.clone(), neg_mag, v64);
        let ten = bcx.ins().iconst(types::I64, 10);
        let pos = bcx.declare_var(types::I64);
        let p23 = bcx.ins().iconst(types::I64, 23);
        bcx.def_var(pos, p23);
        let n = bcx.declare_var(types::I64);
        bcx.def_var(n, mag);
        let digits = bcx.ins().iconst(types::I64, 48);
        let loop_b = bcx.create_block();
        let sign_b = bcx.create_block();
        let end_b = bcx.create_block();
        bcx.ins().jump(loop_b, &[]);
        // header 待回边齐备后再封 - ssa 约束
        bcx.switch_to_block(loop_b);
        {
            let cur = bcx.use_var(n);
            let p = bcx.use_var(pos);
            let d = bcx.ins().srem(cur, ten.clone());
            let ch = bcx.ins().iadd(d, digits.clone());
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
            bcx.seal_block(loop_b); // 前驱齐备: 入口+回边
        }
        bcx.switch_to_block(sign_b);
        bcx.seal_block(sign_b);
        {
            let do_sign = bcx.create_block();
            bcx.ins().brif(neg, do_sign, &[], end_b, &[]);
            bcx.switch_to_block(do_sign);
            bcx.seal_block(do_sign);
            let minus = bcx.ins().iconst(types::I64, 45); // '-'
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
                let p = bcx.use_var(pos);
                let one = bcx.ins().iconst(types::I64, 1);
                let start_off = bcx.use_var(pos);
                let sv = bcx.ins().iadd(buf, start_off);
                sv
            };
            let len = {
                let p = bcx.use_var(pos);
                let c23 = bcx.ins().iconst(types::I64, 23);
                bcx.ins().isub(c23, p)
            };
            let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![bcx.ins().iconst(types::I64, 16)]);
            bcx.ins().store(MemFlagsData::new(), start, blk, 0);
            bcx.ins().store(MemFlagsData::new(), len, blk, 8);
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
        let addr = bcx.ins().select(is_t.clone(), t_addr, f_addr);
        let t_len = bcx.ins().iconst(types::I64, 4); // true
        let f_len = bcx.ins().iconst(types::I64, 5); // false
        let len = bcx.ins().select(is_t, t_len, f_len);
        let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![bcx.ins().iconst(types::I64, 16)]);
        bcx.ins().store(MemFlagsData::new(), addr, blk, 0);
        bcx.ins().store(MemFlagsData::new(), len, blk, 8);
        bcx.ins().return_(&[blk]);
        true
    });
    // display.str: 恒等 — 块即显示结果 (泄漏模型下共享安全)
    shim!(c, "alias.display.str", |bcx, a| {
        bcx.ins().return_(&[a[0]]);
        true
    });
    for (name, dname, dlen) in [
        ("alias.display.unit", "rt_unit", 2i64),
        ("alias.display.func", "rt_func", 6),
        ("alias.display.struct", "rt_struct", 8),
        ("alias.display.array", "rt_array", 7),
    ] {
        shim!(c, name, |bcx, _a| {
            let addr = sym!(bcx, dname);
            let len = bcx.ins().iconst(types::I64, dlen);
            let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![bcx.ins().iconst(types::I64, 16)]);
            bcx.ins().store(MemFlagsData::new(), addr, blk, 0);
            bcx.ins().store(MemFlagsData::new(), len, blk, 8);
            bcx.ins().return_(&[blk]);
            true
        });
    }
    // display.result: 运行时 tag 定 <ok>(4 字节)/<err>(5 字节)
    shim!(c, "alias.display.result", |bcx, a| {
        let ok_addr = sym!(bcx, "rt_ok");
        let err_addr = sym!(bcx, "rt_err");
        let is_ok = bcx.ins().icmp_imm_s(IntCC::Equal, a[0], 0);
        let ok_len = bcx.ins().iconst(types::I64, 4);
        let err_len = bcx.ins().iconst(types::I64, 5);
        let addr = bcx.ins().select(is_ok.clone(), ok_addr, err_addr);
        let len = bcx.ins().select(is_ok, ok_len, err_len);
        let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![bcx.ins().iconst(types::I64, 16)]);
        bcx.ins().store(MemFlagsData::new(), addr, blk, 0);
        bcx.ins().store(MemFlagsData::new(), len, blk, 8);
        bcx.ins().return_(&[blk]);
        true
    });

    // ---- rt.write.stdout(p:PTR, l:I64) : GetStdHandle(-11)+WriteFile ----
    shim!(c, "rt.write.stdout", |bcx, a| {
        let h = call_ext_m!(
            bcx,
            ext.get_std_handle,
            vec![bcx.ins().iconst(types::I32, -11)]
        );
        let ss = bcx.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
        let wa = bcx.ins().stack_addr(c.ptr_ty, ss, 0);
        let null = bcx.ins().iconst(c.ptr_ty, 0);
        let wf = c.module.declare_func_in_func(ext.write_file, &mut bcx.func);
        let len32 = bcx.ins().ireduce(types::I32, a[1]);
        bcx.ins().call(wf, &[h, a[0], len32, wa, null]);
        false
    });

    // ---- print 家族: str 直写; i32/bool 经 display 复用 ----
    shim!(c, "alias.println.str", |bcx, a| {
        let p = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 0);
        let l = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        call_rt_m!(bcx, "rt.write.stdout", vec![p, l]);
        let nl = sym!(bcx, "rt_nl");
        call_rt_m!(
            bcx,
            "rt.write.stdout",
            vec![nl, bcx.ins().iconst(types::I64, 1)]
        );
        false
    });
    shim!(c, "alias.print.str", |bcx, a| {
        let p = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 0);
        let l = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        call_rt_m!(bcx, "rt.write.stdout", vec![p, l]);
        false
    });
    for (pname, dname) in [
        ("alias.println.i32", "alias.println.str"),
        ("alias.print.i32", "alias.print.str"),
    ] {
        shim!(c, pname, |bcx, a| {
            let blk = call_rt_m!(bcx, "alias.display.int", vec![a[0]]);
            call_rt_m!(bcx, dname, vec![blk]);
            false
        });
    }
    for (pname, dname) in [
        ("alias.println.bool", "alias.println.str"),
        ("alias.print.bool", "alias.print.str"),
    ] {
        shim!(c, pname, |bcx, a| {
            let blk = call_rt_m!(bcx, "alias.display.bool", vec![a[0]]);
            call_rt_m!(bcx, dname, vec![blk]);
            false
        });
    }

    // ---- span-ID 中止家族: 同一 IR, 消息后缀不同 (§五契约) ----
    emit_span_abort(
        c,
        "alias.abort_div",
        &ext,
        span_data,
        &static_ids,
        "rt_msg_suffix",
        15,
    )?;
    emit_span_abort(
        c,
        "alias.abort_oob",
        &ext,
        span_data,
        &static_ids,
        "rt_oob_suffix",
        18,
    )?;
    emit_span_abort(
        c,
        "alias.abort_pop",
        &ext,
        span_data,
        &static_ids,
        "rt_pop_suffix",
        19,
    )?;
    emit_span_abort(
        c,
        "alias.abort_conv",
        &ext,
        span_data,
        &static_ids,
        "rt_conv_suffix",
        18,
    )?;

    validate_aot_runtime_coverage(c)
}
