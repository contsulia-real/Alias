//! Alias 唯一原生代码生成后端 — Cranelift 发射 COFF，rust-lld 链接为 exe。
//!
//! 所有权律 (迁移计划依赖清单): 本模块是 cranelift-* API 的**唯一触点**;
//! ast/sema/lib/linker 不见任何 cranelift 类型。
//!
//! Phase 3 全量 Phase-1 对等: 在 P2 i32 子集之上补齐字符串+插值、
//! 闭包引用捕获、一等函数值。黄金测试逐字节背书语义。
//!
//! 值模型 (规范性, Phase 3a 全量数值类型):
//! - **在途值按静态类型双通道**: 整数规范形 = sext(I)/zext(U) 到 I64;
//!   浮点 = 原生 F32/F64 寄存器表示 (不位打包); 引用/Bool/Str/Func = I64 字。
//!   窄宽算术在声明宽度上进行后重新规范化 — wrapping 落在声明宽度 (裁决①)。
//! - **存储按类型定尺寸**: 单元格/全局槽位/结构体字段/数组元素各按
//!   [`size_align`] 布局 (i8/u8=1B align1 … i64/u64/f64/ptr=8B align8);
//!   存入即规范化到声明宽度, 读出即规范化到规范在途形。
//! - **每个绑定 = 泄漏的定尺寸堆单元格** (alias.cell.new(bytes)): 变量(SSA)
//!   持有单元格指针, 每次绑定执行分配新单元格 → 循环每迭代新鲜作用域与
//!   引用捕获 (最新值双向可见) 由构造保证。泄漏即 GC。
//! - **函数签名混合化**: 用户函数/方法签名由参数 VTy 逐位构建
//!   (win64 ABI 经 XMM 传递浮点, cranelift 处理); 闭包对象仍为
//!   {code,env} 16 字节, env = 捕获单元格指针数组 (恒 8B/槽)。
//!   函数值的参数/返回投影随词法作用域流动，不按全局名字猜签名。
//! - **字符串 = 泄漏 16 字节块 {data_ptr: u64, len: u64}**; data_ptr 为 null
//!   当且仅当 len = 0。原生 runtime 区实现统一符号契约
//!   (spec-notes §五): alias.str.new/concat/cmp、alias.display.*、
//!   alias.print*/println.*、alias.cell.new、alias.env.new、
//!   alias.globals.new、alias.closure.new、alias.abort_div。
//!   字面量字节亦复制进块 — 统一所有权, 免生命周期论证。
//! - **结构体实例 (Phase 2a/P3a) = 按字段 size_align 排布的泄漏槽区**
//!   (复用 alias.cell.new(bytes)); 字段按声明序对齐并存声明宽度;
//!   变量持实例指针 — 引用语义:
//!   赋值/传参/闭包捕获共享同一实例。构造 = 全命名实参按声明序求值
//!   (缺省字段取声明默认值); 打印 → 固定 "<struct>" (与 <func> 对称)。
//! - **result 实例 (Phase 2b) = 泄漏 2×8 字节块 {tag, payload}**:
//!   tag 0=ok / 1=err, 载荷为规范字。ok()/err() 构造与 match 的
//!   tag 分臂 / expr? 的载荷传播均按此布局; 打印 → 运行时 tag 定
//!   "<ok>"/"<err>"。
//! - **数组实例 (Phase 2d) = 泄漏 24 字节头块 {data_ptr, len, cap}**,
//!   data_ptr 指向 n×8 元素缓冲 (空数组 data_ptr 为 null); 变量持
//!   头块指针 — 引用语义: 赋值/传参/闭包捕获共享同一实例。push 满
//!   容量时换新缓冲 (2x 或 +1) 复制旧元素, 旧块泄漏 (泄漏即 GC);
//!   下标读带越界守卫 → span-ID 中止存根; 打印 → 固定 "<array>"。
//!
//! 原生编译形态: compile_to_object 发射 x86_64 COFF;
//! 运行时 shim 区在同一 object 内定义 (Export), 仅经 kernel32.lib
//! 解析 GetStdHandle/WriteFile/ExitProcess/HeapAlloc/GetProcessHeap/
//! RtlMoveMemory。入口为 alias_start 并显式 ExitProcess，无 CRT。
//! 已知限制: cranelift-object 不写 .pdata/.xdata
//! — console 程序无碍; SEH 展开穿越 Alias 帧暂不支持。
//!
//! 已知有意缺口 (MIGRATION.md M10/M12): 函数体对未定义名的引用在编译期
//! 报错；打印静态类型不可知的表达式被拒绝。
//!
//! allow: SIZE_OK — 依赖清单强制 codegen/ 为 cranelift 唯一拥有者。

// 子模块划分 (纯机械拆分): emit=表达式/语句发射;
// funcgen=函数/方法/闭包定义与捕获扫描; types_proj=静态类型投影;
// native_runtime=原生运行时区 (kernel32 符号契约)。
mod abi;
mod emit;
mod funcgen;
mod native_runtime;
mod runtime;
mod types_proj;

use crate::ast::*;
use crate::sema::types::{FloatW, IntW, UIntW};
use crate::{AliasError, AliasResult, Span};
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{
    AbiParam, Block, BlockArg, Function, InstBuilder, MemFlagsData, Signature, TrapCode,
    UserFuncName, Value,
};
use cranelift_codegen::settings;
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module};
use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};

pub(crate) use abi::*;
pub(crate) use emit::*;
pub(crate) use funcgen::*;
use native_runtime::{define_span_data, emit_native_runtime};
pub(crate) use runtime::*;
pub(crate) use types_proj::*;

/// 名字解析: 局部 SSA 变量(持单元格指针)或顶层槽区偏移。
#[derive(Clone, Copy)]
pub(crate) enum Slot {
    Local(Variable),
    Global(usize),
}

/// 词法帧: 作用域链 + globals/env 寄存器 + 本函数捕获表。
/// terminated: 当前块是否已发射终结指令 (frontend 不公开该查询, 自行跟踪)。
#[derive(Clone)]
pub(crate) struct Frame {
    pub(crate) scopes: Vec<HashMap<String, Slot>>,
    pub(crate) locals_vty: Vec<HashMap<String, VTy>>,
    pub(crate) globals: Variable,
    pub(crate) env: Option<Variable>,
    pub(crate) caps: HashMap<String, usize>,
    /// 捕获项静态类型 (打印分派/字段偏移回查所需) — 捕获名不在本帧
    /// 作用域链也不在 globals, 类型信息必须随捕获表跨帧传递
    pub(crate) caps_vty: HashMap<String, VTy>,
    pub(crate) terminated: bool,
    /// 顶层初始化器语境: 名字仅随项序可见 (禁回退全量表 — 前向引用必须报错)
    init_ctx: bool,
    /// 函数返回块 (match never 臂 / expr? 的 return 跳转目标);
    /// None = 入口 wrapper / 推断语境 — 此类语境 sema 已拒绝 return 流
    ret_block: Option<Block>,
    /// 函数声明返回类型的 cranelift 映射 — Return/箭头体 jump 到 ret_block
    /// 前把 I64 规范字桥接到声明宽度 (存储规范化裁决①)
    ret_vty: Option<VTy>,
}

/// 顶层初始化器语境可见性由 env.scopes[0] 渐增插入实现 (insert-after-eval),
/// 函数体语境则全量可见 — 与迁移前逐项插入语义一致。
/// M 当前为 ObjectModule；保留 Module 泛型使发射器不依赖具体对象容器。
pub(crate) struct Compiler<'m, M: Module> {
    pub(crate) module: &'m mut M,
    pub(crate) cc: cranelift_codegen::isa::CallConv,
    pub(crate) ptr_ty: cranelift_codegen::ir::Type,
    /// 顶层绑定 → (字节偏移, 静态类型) — 槽区按类型尺寸对齐累积 (Phase 3a)
    pub(crate) globals_final: HashMap<String, (usize, VTy)>,
    /// 顶层绑定按源码顺序对应的独立槽位。重名采用 last-wins 解析，但每个
    /// 初始化器必须写自己的槽，不能把早期宽值写进末次窄槽。
    pub(crate) top_slots: Vec<usize>,
    /// 槽区总字节数 (含尾随对齐)
    pub(crate) global_bytes: usize,
    pub(crate) next_fid: u32,
    /// 运行时函数 ID → 定稿用 FuncId
    pub(crate) fn_ids: Vec<FuncId>,
    /// 运行时函数 ID → 返回类型 (打印分派)
    pub(crate) fn_rets: Vec<VTy>,
    pub(crate) pending: VecDeque<PendingFn>,
    pub(crate) str_data: HashMap<String, cranelift_module::DataId>,
    /// 除零守卫 span 表: ID = 下标，序列化为只读数据段供 abort runtime 回查。
    pub(crate) span_table: Vec<(u32, u32)>,
    /// 结构体布局表 (Phase 2a): 由 Program 项预扫描登记
    struct_layouts: StructTable,
    /// 方法表 (Phase 2c): (接收者类型名, 方法名) → FuncId — 静态分派;
    /// sema 已校验存在性/元数/实参类型, 此处只做发射
    methods: HashMap<(String, String), FuncId>,
    /// 方法返回类型 (链式调用的静态投影 / 打印分派)
    method_rets: HashMap<(String, String), VTy>,
    /// 方法混合签名 (Phase 3a): (接收者, 名) → (参数类型含 self, 返回类型)
    method_sigs: HashMap<(String, String), (Vec<VTy>, VTy)>,
    /// 原生 runtime 实际定义集合；完成发射后与契约表做精确覆盖校验。
    runtime_defined: HashSet<&'static str>,
}

pub(crate) struct PendingFn {
    fid: FuncId,
    params: Vec<Param>,
    body: Body,
    /// 捕获项 (名字, 静态类型) — 类型随捕获表传递供闭包体内分派
    caps: Vec<(String, VTy)>,
    ret_vty: VTy,
}

/// 编译为 x86_64 COFF 目标文件字节流。
/// 原生 runtime 在同一 object 内定义 (Export), 经 kernel32 解析;
/// 产物入口为无 CRT 的 alias_start，并显式调用 ExitProcess。
pub fn compile_to_object(program: Program) -> AliasResult<Vec<u8>> {
    // import 只解析不执行 — 通知文本与黄金记录逐字节一致 (spec-notes §三.5)
    if !program.imports.is_empty() {
        eprintln!(
            "[alias] 注意: {} 条 import 已解析但标准库尚未接入 (Phase 5 前)",
            program.imports.len()
        );
    }

    let flag_builder = settings::builder();
    let isa = cranelift_native::builder()
        .map_err(|e| native_err(Span::default(), format!("ISA 探测失败: {e}")))?
        .finish(settings::Flags::new(flag_builder))
        .map_err(|e| native_err(Span::default(), format!("ISA 构造失败: {e}")))?;
    let builder = cranelift_object::ObjectBuilder::new(
        isa.clone(),
        b"alias_out".to_vec(),
        default_libcall_names(),
    )
    .map_err(|e| native_err(Span::default(), format!("object builder 构造失败: {e}")))?;
    let mut module = cranelift_object::ObjectModule::new(builder);

    let mut c = Compiler {
        cc: module.isa().default_call_conv(),
        ptr_ty: module.isa().pointer_type(),
        module: &mut module,
        globals_final: HashMap::new(),
        top_slots: Vec::new(),
        global_bytes: 0,
        next_fid: 0,
        fn_ids: Vec::new(),
        fn_rets: Vec::new(),
        pending: VecDeque::new(),
        str_data: HashMap::new(),
        span_table: Vec::new(),
        struct_layouts: HashMap::new(),
        methods: HashMap::new(),
        method_rets: HashMap::new(),
        method_sigs: HashMap::new(),
        runtime_defined: HashSet::new(),
    };

    emit_native_runtime(&mut c)?;
    compile_program(&mut c, &program.items)?;

    let span_table = std::mem::take(&mut c.span_table);
    define_span_data(&mut c, &span_table)?;
    drop(c); // 归还模块借用 — finish 消费所有权 (ObjectModule 无 Copy)

    let product = module.finish();
    product
        .emit()
        .map_err(|e| native_err(Span::default(), format!("COFF 发射失败: {e}")))
}

/// 原生编译编排: pass1 全局槽位 → pass2 函数体 → 入口 wrapper → 排空派生队列。
fn compile_program<M: Module>(c: &mut Compiler<'_, M>, items: &[Item]) -> AliasResult<FuncId> {
    // ---- pass 0: 结构体布局由 ABI 层两阶段统一计算 ----
    c.struct_layouts = build_struct_layouts(items);
    debug_assert!(c
        .struct_layouts
        .values()
        .all(|layout| (layout.size as usize).is_multiple_of(layout.align)));

    // ---- pass 0.5: 方法声明 (Phase 2c) — 静态分派目标登记;
    //      体延至 pass 2 之后定义 (彼时 globals_final 已齐,
    //      方法体可引用任意顶层绑定) ----
    let mut pending_methods: Vec<(FuncId, &Binding)> = Vec::new();
    for b in items.iter().filter_map(|i| match i {
        Item::Binding(b) if b.receiver.is_some() => Some(b),
        _ => None,
    }) {
        let Some((recv, mname)) = &b.receiver else {
            invariant_violation("过滤保证 receiver 存在")
        };
        let Expr::FuncLit { params, .. } = &b.value else {
            return Err(native_err(b.span, "方法体必须是函数字面量"));
        };
        let self_vty = decl_vty(&TypeExpr::Named(recv.clone()), &c.struct_layouts);
        let param_vtys: Vec<VTy> = std::iter::once(self_vty)
            .chain(params.iter().map(|p| decl_vty(&p.ty, &c.struct_layouts)))
            .collect();
        let ret_vty = decl_vty(&b.ty, &c.struct_layouts);
        let fid = c.declare_user_func_typed(&param_vtys, &ret_vty, format!("m<{recv}>{mname}"))?;
        let key = (recv.clone(), mname.clone());
        c.methods.insert(key.clone(), fid);
        c.method_rets.insert(key.clone(), ret_vty.clone());
        c.method_sigs.insert(key, (param_vtys, ret_vty));
        pending_methods.push((fid, b));
    }

    // ---- pass 1: 顶层绑定全部分配全局槽位 (func 绑定的槽位存其闭包对象);
    //      main 取最后同名 func 绑定 (last-wins, spec-notes Q④ 现状锚点);
    //      方法不是绑定 — 不占槽位不入命名空间 ----
    let mut main_slot_ret: Option<(usize, VTy)> = None;
    let mut top_funcs: Vec<(FuncId, usize, &Binding)> = Vec::new();
    {
        let mut off = 0usize;
        for b in items.iter().filter_map(|i| match i {
            Item::Binding(b) if b.receiver.is_none() => Some(b),
            _ => None,
        }) {
            let vty = decl_vty(&b.ty, &c.struct_layouts);
            // func 槽位保存闭包对象，但静态投影携带完整签名，供局部遮蔽与
            // 捕获后的间接调用精确选择 GPR/XMM ABI。
            let slot_vty = if b.kind == BindKind::Func {
                let Expr::FuncLit { params, .. } = &b.value else {
                    return Err(native_err(b.span, "函数值尚未接入原生后端 (Phase 3)"));
                };
                VTy::Func(
                    params
                        .iter()
                        .map(|p| decl_vty(&p.ty, &c.struct_layouts))
                        .collect(),
                    Box::new(vty.clone()),
                )
            } else {
                vty.clone()
            };
            let (sz, al) = size_align(&slot_vty);
            off = align_to(off, al);
            let slot = off;
            off += sz;
            c.top_slots.push(slot);
            c.globals_final.insert(b.name.clone(), (slot, slot_vty));
            if b.kind == BindKind::Func {
                let Expr::FuncLit { params, .. } = &b.value else {
                    return Err(native_err(b.span, "函数值尚未接入原生后端 (Phase 3)"));
                };
                let param_vtys: Vec<VTy> = params
                    .iter()
                    .map(|p| decl_vty(&p.ty, &c.struct_layouts))
                    .collect();
                let name = format!("u{}", c.next_fid);
                c.next_fid += 1;
                let fid = c.declare_user_func_typed(&param_vtys, &vty, name)?;
                c.fn_ids.push(fid);
                c.fn_rets.push(vty.clone());
                top_funcs.push((fid, slot, b));
                if b.name == "main" {
                    main_slot_ret = Some((slot, vty));
                }
            }
        }
        c.global_bytes = align_to(off, 8);
    }

    // ---- pass 2: 定义顶层函数体 (无捕获 — 自由名皆顶层槽位/globals) ----
    for (fid, _slot, b) in top_funcs {
        let Expr::FuncLit { params, body, .. } = &b.value else {
            unreachable!("pass 1 已确保 func 绑定初始化为函数字面量");
        };
        let ret_vty = decl_vty(&b.ty, &c.struct_layouts);
        c.define_user_func(fid, params, body, Vec::new(), ret_vty)?;
    }

    // ---- pass 2.5: 方法定义 — self 为首参数的普通内部函数;
    //      无捕获 (自由名经 globals 参数可达), 调用点传哑 env 字 0 ----
    for (fid, b) in pending_methods {
        let Expr::FuncLit { params, body, .. } = &b.value else {
            unreachable!("pass 0.5 已确保方法体为函数字面量");
        };
        let Some((recv, _)) = &b.receiver else {
            unreachable!("pending_methods 只收带接收者的绑定");
        };
        let self_param = Param {
            ty: TypeExpr::Named(recv.clone()),
            name: "self".into(),
            span: b.span,
        };
        let mut all_params = Vec::with_capacity(params.len() + 1);
        all_params.push(self_param);
        all_params.extend(params.iter().cloned());
        let ret_vty = decl_vty(&b.ty, &c.struct_layouts);
        c.define_user_func(fid, &all_params, body, Vec::new(), ret_vty)?;
    }

    // ---- 入口 wrapper: Q⑥ 顺序求值顶层初始化 → 间接调 main → 退出映射 ----
    let (main_slot, main_ret) =
        main_slot_ret.unwrap_or_else(|| invariant_violation("main 存在性 (sema Q④ 已校验)"));
    let entry_fid = c.compile_entry(items, main_slot, main_ret)?;

    // 排空派生函数队列 (funclit 可能再派生 funclit)
    while let Some(p) = c.pending.pop_front() {
        c.define_user_func(p.fid, &p.params, &p.body, p.caps, p.ret_vty)?;
    }
    Ok(entry_fid)
}

pub(crate) fn native_err(span: Span, msg: impl Into<String>) -> AliasError {
    AliasError {
        msg: msg.into(),
        span,
    }
}

/// 内部不变式违例 (sema 之后不应可达) — 编译器 bug 走 panic, 不伪装成用户错误。
pub(crate) fn invariant_violation(what: &'static str) -> ! {
    panic!("内部代码生成不变式被破坏: {what} (sema 校验缺口, 请报告)")
}
