// ---------------------------------------------------------------------------
// aot_shim — AOT 运行时 shim 区: kernel32/msvcrt 符号契约的 IR 实现。
// 与 host.rs (JIT 宿主) 逐符号对齐; 发射顺序必须先于 compile_program
// (用户代码 Import 声明与同名 Export 定义经 cranelift-module 合并)。
// ---------------------------------------------------------------------------
use super::*;
use crate::codegen::Compiler;
use std::collections::HashMap;
use crate::{AliasResult, Span};
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{BlockArg, InstBuilder, StackSlotData, StackSlotKind, Value};
use cranelift_codegen::ir::Function;
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};

/// ASCII 空白字符集 (trim 判定)
const TRIM_SET: &[u8] = b" \t\r\n";
// ---------------------------------------------------------------------------
// AOT 运行时 shim 区 — 与 JIT 宿主函数逐符号对齐 (模块头契约)
//
// 依赖面: kernel32.lib (GetStdHandle/WriteFile/ExitProcess/HeapAlloc/
// GetProcessHeap/RtlMoveMemory) + msvcrt.lib (memcmp/sprintf)。
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
    ($c:expr, $name:expr, $params:expr, $ret:expr, |$bcx:ident, $a:ident| $body:block) => {{
        let __params: Vec<cranelift_codegen::ir::Type> = $params;
        let __ret: Option<cranelift_codegen::ir::Type> = $ret;
        let __sig = $c.sig(&__params, __ret.clone());
        let __fid = $c
            .module
            .declare_function($name, Linkage::Export, &__sig)
            .map_err(|e| native_err(Span::default(), format!("内部: shim 声明失败 {e}")))?;
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
                None => { $bcx.ins().return_(&[]); },
            }
        }

        $c.module
            .define_function(__fid, &mut __ctx)
            .map_err(|e| native_err(Span::default(), format!("内部: shim 定义失败 {e}")))?;
    }};
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
    let sig = c.sig(&[types::I64], Some(types::I64));
    let fid = c
        .module
        .declare_function(name, Linkage::Export, &sig)
        .map_err(|e| native_err(Span::default(), format!("内部: shim 声明失败 {e}")))?;
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
        let alloc_f = c.import_fn("rt.heap.alloc", &[types::I64], Some(c.ptr_ty))?;
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
            let f = c.import_fn("rt.heap.alloc", &[types::I64], Some(c.ptr_ty))?;
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

/// span 表数据段回填: (line, col) u32 小端对 — abort shim 运行时查表。
pub(crate) fn define_span_data<M: Module>(c: &mut Compiler<'_, M>, table: &[(u32, u32)]) -> AliasResult<()> {    let id = c
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
    let sig = c.sig(&[types::I32], None);
    let fid = c
        .module
        .declare_function(name, Linkage::Export, &sig)
        .map_err(|e| native_err(Span::default(), format!("内部: shim 声明失败 {e}")))?;
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
            let __wa = $bcx.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let __wa_addr = $bcx.ins().stack_addr(c.ptr_ty, __wa, 0);
            let __null = $bcx.ins().iconst(c.ptr_ty, 0);
            let __gv = c.module.declare_data_in_func(static_ids[$data], &mut $bcx.func);
            let __addr = $bcx.ins().symbol_value(c.ptr_ty, __gv);
            let __len = $bcx.ins().iconst(types::I64, $len);
            let __wf = c.module.declare_func_in_func(ext.write_file, &mut $bcx.func);
            $bcx.ins().call(__wf, &[$err, __addr, __len, __wa_addr, __null]);
        }};
    }
    macro_rules! w_dec {
        ($bcx:expr, $err:expr, $v:expr) => {{
            let __f = c.import_fn("rt.write.dec", &[c.ptr_ty, types::I64], None)?;
            let __r = c.module.declare_func_in_func(__f, &mut $bcx.func);
            let __args = [$err, bcx.ins().uextend(types::I64, $v)];
            $bcx.ins().call(__r, &__args);
        }};
    }
    let err_args = [bcx.ins().iconst(types::I32, -12)];
    let err = {
        let r = c.module.declare_func_in_func(ext.get_std_handle, &mut bcx.func);
        let inst = bcx.ins().call(r, &err_args);
        first_result(&bcx, inst)
    };
    w_str!(bcx, err, "rt_msg_prefix", 9); // 「错误 @ 」 = 6+1+1+1 字节
    w_dec!(bcx, err, line);
    w_str!(bcx, err, "rt_colon", 1);
    w_dec!(bcx, err, col);
    w_str!(bcx, err, suffix, suffix_len);
    let code1 = bcx.ins().iconst(types::I32, 1);
    let ep = c.module.declare_func_in_func(ext.exit_process, &mut bcx.func);
    bcx.ins().call(ep, &[code1]);
    bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO); // 不可达兜底

    bcx.finalize(c.module.target_config());
    c.module
        .define_function(fid, &mut ctx)
        .map_err(|e| native_err(Span::default(), format!("内部: shim 定义失败 {e}")))
}

pub(crate) fn emit_runtime_shims<M: Module>(c: &mut Compiler<'_, M>) -> AliasResult<()> {
    let ext = AotExterns {
        get_std_handle: c.import_fn("GetStdHandle", &[types::I32], Some(c.ptr_ty))?,
        write_file: c.import_fn(
            "WriteFile",
            &[c.ptr_ty, c.ptr_ty, types::I64, c.ptr_ty, c.ptr_ty],
            Some(types::I32),
        )?,
        exit_process: c.import_fn("ExitProcess", &[types::I32], None)?,
    };
    let heap_alloc = c.import_fn("HeapAlloc", &[c.ptr_ty, types::I32, types::I64], Some(c.ptr_ty))?;
    let _get_process_heap = c.import_fn("GetProcessHeap", &[], Some(c.ptr_ty))?;
    let rtl_move_memory = c.import_fn("RtlMoveMemory", &[c.ptr_ty, c.ptr_ty, types::I64], None)?;

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
        ("rt_msg_prefix", "错误 @ ".as_bytes()),      // 9 字节
        ("rt_colon", b":"),
        ("rt_msg_suffix", " — 除以零\n".as_bytes()), // 15 字节
        ("rt_oob_suffix", " — 下标越界\n".as_bytes()),  // 18 字节
        ("rt_pop_suffix", " — pop 空数组\n".as_bytes()), // 19 字节
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
            let __gv = c.module.declare_data_in_func(static_ids[$name], &mut $bcx.func);
            $bcx.ins().symbol_value(c.ptr_ty, __gv)
        }};
    }
    macro_rules! call_rt_m {
        ($bcx:expr, $nm:expr, $ps:expr, $r:expr, $args:expr) => {{
            let __f = c.import_fn($nm, &$ps, $r)?;
            let __r = c.module.declare_func_in_func(__f, &mut $bcx.func);
            let __args = $args;
            let __inst = $bcx.ins().call(__r, &__args);
            match $bcx.inst_results(__inst) {
                [v] => *v,
                [] => $bcx.ins().iconst(types::I64, 0),
                _ => invariant_violation("运行时单返回值签名"),
            }
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
    shim!(c, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty), |bcx, a| {
        let h = call_rt_m!(bcx, "GetProcessHeap", vec![], Some(c.ptr_ty), vec![]);
        let flags = bcx.ins().iconst(types::I32, 8); // HEAP_ZERO_MEMORY
        let p = call_ext_m!(bcx, heap_alloc, vec![h, flags, a[0]]);
        bcx.ins().return_(&[p]);
        true
    });

    // ---- 分配器: cell / env / globals / closure ----
    shim!(c, "alias.cell.new", vec![types::I64], Some(types::I64), |bcx, a| {
        let sz = bcx.ins().iconst(types::I64, 8);
        let p = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty), vec![sz]);
        bcx.ins().store(MemFlagsData::new(), a[0], p, 0);
        bcx.ins().return_(&[p]);
        true
    });
    for name in ["alias.env.new", "alias.globals.new"] {
        shim!(c, name, vec![types::I32], Some(types::I64), |bcx, a| {
            let n64 = bcx.ins().sextend(types::I64, a[0]);
            let bytes = bcx.ins().imul_imm_s(n64, 8);
            let p = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty), vec![bytes]);
            bcx.ins().return_(&[p]);
            true
        });
    }
    shim!(c, "alias.closure.new", vec![types::I64, types::I64], Some(types::I64), |bcx, a| {
        let sz = bcx.ins().iconst(types::I64, 16);
        let p = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty), vec![sz]);
        bcx.ins().store(MemFlagsData::new(), a[0], p, 0);
        bcx.ins().store(MemFlagsData::new(), a[1], p, 8);
        bcx.ins().return_(&[p]);
        true
    });

    // ---- 字符串块 {data_ptr, len}; 字节复制进新缓冲 ----
    shim!(c, "alias.str.new", vec![c.ptr_ty, types::I32], Some(types::I64), |bcx, a| {
        let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty),
            vec![bcx.ins().iconst(types::I64, 16)]);
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
            let buf = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty), vec![len64]);
            let mv = c.module.declare_func_in_func(rtl_move_memory, &mut bcx.func);
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

    shim!(c, "alias.str.concat", vec![types::I64, types::I64], Some(types::I64), |bcx, a| {
        let pa = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 0);
        let la = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        let pb = bcx.ins().load(types::I64, MemFlagsData::new(), a[1], 0);
        let lb = bcx.ins().load(types::I64, MemFlagsData::new(), a[1], 8);
        let total = bcx.ins().iadd(la, lb);
        let out = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty), vec![total]);
        let mv = c.module.declare_func_in_func(rtl_move_memory, &mut bcx.func);
        bcx.ins().call(mv, &[out, pa, la]);
        let out2 = bcx.ins().iadd(out, la);
        bcx.ins().call(mv, &[out2, pb, lb]);
        let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty),
            vec![bcx.ins().iconst(types::I64, 16)]);
        bcx.ins().store(MemFlagsData::new(), out, blk, 0);
        bcx.ins().store(MemFlagsData::new(), total, blk, 8);
        bcx.ins().return_(&[blk]);
        true
    });

    shim!(c, "alias.str.cmp", vec![types::I64, types::I64], Some(types::I32), |bcx, a| {
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
    shim!(c, "alias.str.len", vec![types::I64], Some(types::I32), |bcx, a| {
        let l = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        let t = bcx.ins().ireduce(types::I32, l);
        bcx.ins().return_(&[t]);
        true
    });
    emit_case_shim(c, "alias.str.upper", b'a' as i64, b'z' as i64, -32)?;
    emit_case_shim(c, "alias.str.lower", b'A' as i64, b'Z' as i64, 32)?;

    // trim: 双边界扫描 (首尾剥离 空格/\t/\r/\n) → 子块复制新块;
    // 全空白/空串结果的 data_ptr 恒 null (§五契约)
    shim!(c, "alias.str.trim", vec![types::I64], Some(types::I64), |bcx, a| {
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
            let out = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty), vec![n]);
            let mv = c.module.declare_func_in_func(rtl_move_memory, &mut bcx.func);
            let stv3 = bcx.use_var(st);
            let src = bcx.ins().iadd(pa, stv3);
            bcx.ins().call(mv, &[out, src, n]);
            let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty),
                vec![bcx.ins().iconst(types::I64, 16)]);
            bcx.ins().store(MemFlagsData::new(), out, blk, 0);
            bcx.ins().store(MemFlagsData::new(), n, blk, 8);
            bcx.ins().jump(end_b, &[BlockArg::Value(blk)]);
        }
        bcx.switch_to_block(else_b);
        {
            let zero = bcx.ins().iconst(types::I64, 0);
            let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty),
                vec![bcx.ins().iconst(types::I64, 16)]);
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
    shim!(c, "alias.arr.new", vec![types::I32], Some(types::I64), |bcx, a| {
        let hdr = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty),
            vec![bcx.ins().iconst(types::I64, 24)]);
        let cap64 = bcx.ins().sextend(types::I64, a[0]);
        let has = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, cap64, 0);
        let then_b = bcx.create_block();
        let else_b = bcx.create_block();
        let end_b = bcx.create_block();
        bcx.ins().brif(has, then_b, &[], else_b, &[]);
        bcx.seal_block(then_b);
        bcx.seal_block(else_b);
        bcx.switch_to_block(then_b);
        {
            let bytes = bcx.ins().imul_imm_s(cap64, 8);
            let buf = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty), vec![bytes]);
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
    shim!(c, "alias.arr.len", vec![types::I64], Some(types::I32), |bcx, a| {
        let l = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        let t = bcx.ins().ireduce(types::I32, l);
        bcx.ins().return_(&[t]);
        true
    });
    // push: 满 len==cap → 新缓冲 2x (空 cap 取 1) + RtlMoveMemory 复制旧元素,
    // 头块原地换 data_ptr/cap — 所有别名共享可见; 随后尾插 + len+1
    shim!(c, "alias.arr.push", vec![types::I64, types::I64], None, |bcx, a| {
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
            let grown = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty), vec![bytes]);
            let mv = c.module.declare_func_in_func(rtl_move_memory, &mut bcx.func);
            let copy_bytes = bcx.ins().imul_imm_s(len, 8);
            bcx.ins().call(mv, &[grown, dp0, copy_bytes]);
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
    shim!(c, "alias.arr.pop", vec![types::I64], Some(types::I64), |bcx, a| {
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
    shim!(c, "rt.write.dec", vec![c.ptr_ty, types::I64], None, |bcx, a| {
        let buf = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty),
            vec![bcx.ins().iconst(types::I64, 24)]);
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
            let ss = bcx.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let wa = bcx.ins().stack_addr(c.ptr_ty, ss, 0);
            let null = bcx.ins().iconst(c.ptr_ty, 0);
            let wf = c.module.declare_func_in_func(ext.write_file, &mut bcx.func);
            let wf_args = [a[0].clone(), start, len, wa, null];
            bcx.ins().call(wf, &wf_args);
        }
        false
    });
    // display.int: IR 手写十进制转换 (无 CRT 依赖; i64 幅度容纳 i32::MIN 取负)
    shim!(c, "alias.display.int", vec![types::I32], Some(types::I64), |bcx, a| {
        let buf = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty),
            vec![bcx.ins().iconst(types::I64, 24)]);
        let v64 = bcx.ins().sextend(types::I64, a[0]);
        let zero = bcx.ins().iconst(types::I64, 0);
        let neg = bcx.ins().icmp(IntCC::SignedLessThan, v64.clone(), zero.clone());
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
            let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty),
                vec![bcx.ins().iconst(types::I64, 16)]);
            bcx.ins().store(MemFlagsData::new(), start, blk, 0);
            bcx.ins().store(MemFlagsData::new(), len, blk, 8);
            bcx.ins().return_(&[blk]);
        }
        true
    });
    shim!(c, "alias.display.bool", vec![types::I32], Some(types::I64), |bcx, a| {
        let t_addr = sym!(bcx, "rt_true");
        let f_addr = sym!(bcx, "rt_false");
        let is_t = bcx.ins().icmp_imm_s(IntCC::NotEqual, a[0], 0);
        let addr = bcx.ins().select(is_t.clone(), t_addr, f_addr);
        let t_len = bcx.ins().iconst(types::I64, 4); // true
        let f_len = bcx.ins().iconst(types::I64, 5); // false
        let len = bcx.ins().select(is_t, t_len, f_len);
        let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty),
            vec![bcx.ins().iconst(types::I64, 16)]);
        bcx.ins().store(MemFlagsData::new(), addr, blk, 0);
        bcx.ins().store(MemFlagsData::new(), len, blk, 8);
        bcx.ins().return_(&[blk]);
        true
    });
    // display.str: 恒等 — 块即显示结果 (泄漏模型下共享安全)
    shim!(c, "alias.display.str", vec![types::I64], Some(types::I64), |bcx, a| {
        bcx.ins().return_(&[a[0]]);
        true
    });
    for (name, dname, dlen) in [
        ("alias.display.unit", "rt_unit", 2i64),
        ("alias.display.func", "rt_func", 6),
        ("alias.display.struct", "rt_struct", 8),
        ("alias.display.array", "rt_array", 7),
    ] {
        shim!(c, name, vec![], Some(types::I64), |bcx, _a| {
            let addr = sym!(bcx, dname);
            let len = bcx.ins().iconst(types::I64, dlen);
            let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty),
                vec![bcx.ins().iconst(types::I64, 16)]);
            bcx.ins().store(MemFlagsData::new(), addr, blk, 0);
            bcx.ins().store(MemFlagsData::new(), len, blk, 8);
            bcx.ins().return_(&[blk]);
            true
        });
    }
    // display.result: 运行时 tag 定 <ok>(4 字节)/<err>(5 字节)
    shim!(c, "alias.display.result", vec![types::I32], Some(types::I64), |bcx, a| {
        let ok_addr = sym!(bcx, "rt_ok");
        let err_addr = sym!(bcx, "rt_err");
        let is_ok = bcx.ins().icmp_imm_s(IntCC::Equal, a[0], 0);
        let ok_len = bcx.ins().iconst(types::I64, 4);
        let err_len = bcx.ins().iconst(types::I64, 5);
        let addr = bcx.ins().select(is_ok.clone(), ok_addr, err_addr);
        let len = bcx.ins().select(is_ok, ok_len, err_len);
        let blk = call_rt_m!(bcx, "rt.heap.alloc", vec![types::I64], Some(c.ptr_ty),
            vec![bcx.ins().iconst(types::I64, 16)]);
        bcx.ins().store(MemFlagsData::new(), addr, blk, 0);
        bcx.ins().store(MemFlagsData::new(), len, blk, 8);
        bcx.ins().return_(&[blk]);
        true
    });

    // ---- rt.write.stdout(p:PTR, l:I64) : GetStdHandle(-11)+WriteFile ----
    shim!(c, "rt.write.stdout", vec![c.ptr_ty, types::I64], None, |bcx, a| {
        let h = call_ext_m!(bcx, ext.get_std_handle, vec![bcx.ins().iconst(types::I32, -11)]);
        let ss = bcx.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
        let wa = bcx.ins().stack_addr(c.ptr_ty, ss, 0);
        let null = bcx.ins().iconst(c.ptr_ty, 0);
        let wf = c.module.declare_func_in_func(ext.write_file, &mut bcx.func);
        bcx.ins().call(wf, &[h, a[0], a[1], wa, null]);
        false
    });

    // ---- print 家族: str 直写; i32/bool 经 display 复用 ----
    shim!(c, "alias.println.str", vec![types::I64], None, |bcx, a| {
        let p = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 0);
        let l = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        call_rt_m!(bcx, "rt.write.stdout", vec![c.ptr_ty, types::I64], None, vec![p, l]);
        let nl = sym!(bcx, "rt_nl");
        call_rt_m!(bcx, "rt.write.stdout", vec![c.ptr_ty, types::I64], None,
            vec![nl, bcx.ins().iconst(types::I64, 1)]);
        false
    });
    shim!(c, "alias.print.str", vec![types::I64], None, |bcx, a| {
        let p = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 0);
        let l = bcx.ins().load(types::I64, MemFlagsData::new(), a[0], 8);
        call_rt_m!(bcx, "rt.write.stdout", vec![c.ptr_ty, types::I64], None, vec![p, l]);
        false
    });
    for (pname, dname) in [
        ("alias.println.i32", "alias.println.str"),
        ("alias.print.i32", "alias.print.str"),
    ] {
        shim!(c, pname, vec![types::I32], None, |bcx, a| {
            let blk = call_rt_m!(bcx, "alias.display.int", vec![types::I32], Some(types::I64), vec![a[0]]);
            call_rt_m!(bcx, dname, vec![types::I64], None, vec![blk]);
            false
        });
    }
    for (pname, dname) in [
        ("alias.println.bool", "alias.println.str"),
        ("alias.print.bool", "alias.print.str"),
    ] {
        shim!(c, pname, vec![types::I32], None, |bcx, a| {
            let blk = call_rt_m!(bcx, "alias.display.bool", vec![types::I32], Some(types::I64), vec![a[0]]);
            call_rt_m!(bcx, dname, vec![types::I64], None, vec![blk]);
            false
        });
    }

    // ---- span-ID 中止家族: 同一 IR, 消息后缀不同 (§五契约) ----
    emit_span_abort(c, "alias.abort_div", &ext, span_data, &static_ids, "rt_msg_suffix", 15)?;
    emit_span_abort(c, "alias.abort_oob", &ext, span_data, &static_ids, "rt_oob_suffix", 18)?;
    emit_span_abort(c, "alias.abort_pop", &ext, span_data, &static_ids, "rt_pop_suffix", 19)?;

    Ok(())
}
