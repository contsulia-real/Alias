//! Alias 原生代码生成后端 — Cranelift 双形态: JIT (进程内执行) + AOT (COFF 目标文件)。
//!
//! 所有权律 (迁移计划依赖清单): 本模块是 cranelift-* API 的**唯一触点**;
//! ast/sema/lib/linker 不见任何 cranelift 类型。
//!
//! Phase 3 全量 Phase-1 对等: 在 P2 i32 子集之上补齐字符串+插值、
//! 闭包引用捕获、一等函数值。黄金测试逐字节背书语义。
//!
//! 值模型 (规范性):
//! - 一切**在途值 = 64 位字**: Int=sext(i32)、Bool=0/1、Unit=0、
//!   Str/Func=泄漏堆指针。算术/比较/除法守卫在 I32 上进行 (P2 冻结语义,
//!   含 INT_MIN÷-1 守卫), 边界 trunc/sext。
//! - **每个绑定 = 泄漏的 8 字节堆单元格**: 变量(SSA)持有单元格指针,
//!   每次绑定执行分配新单元格 → 循环每迭代新鲜作用域与引用捕获
//!   (最新值双向可见) 由构造保证 — 镜像迁移前 RefCell 作用域链语义。
//!   泄漏即 GC; 规模为奇偶校验骨架, 不求吞吐。
//! - **闭包对象** = {code,env} 16 字节泄漏块; env = 捕获单元格指针数组。
//!   统一调用约定 `fn(globals, env, args...) -> word`, 全部调用经闭包
//!   对象间接发射 — 名字解析走作用域链, 遮蔽语义与解释器一致。
//!   code 地址由 func_addr 直取 (JIT/AOT 通用, 免运行时指针表)。
//! - **字符串 = 泄漏 16 字节块 {data_ptr: u64, len: u64}**; data_ptr 为 null
//!   当且仅当 len = 0。JIT 宿主函数与 AOT shim 区实现同一符号契约
//!   (spec-notes §五): alias.str.new/concat/cmp、alias.display.*、
//!   alias.print*/println.*、alias.cell.new、alias.env.new、
//!   alias.globals.new、alias.closure.new、alias.abort_div。
//!   字面量字节亦复制进块 — 统一所有权, 免生命周期论证。
//! - **结构体实例 (Phase 2a) = 泄漏 n×8 字节槽区** (复用 alias.env.new),
//!   字段按声明序偏移 idx*8 存规范字; 变量持实例指针 — 引用语义:
//!   赋值/传参/闭包捕获共享同一实例。构造 = 全命名实参按声明序求值
//!   (缺省字段取声明默认值); 打印 → 固定 "<struct>" (与 <func> 对称)。
//! - **result 实例 (Phase 2b) = 泄漏 2×8 字节块 {tag, payload}**:
//!   tag 0=ok / 1=err, 载荷为规范字。ok()/err() 构造与 match 的
//!   tag 分臂 / expr? 的载荷传播均按此布局; 打印 → 运行时 tag 定
//!   "<ok>"/"<err>"。
//!
//! AOT 形态 (Phase 5): compile_to_object 发射 x86_64 COFF;
//! 运行时 shim 区在同一 object 内定义 (Export), 经 kernel32.lib +
//! msvcrt.lib 解析 GetStdHandle/WriteFile/ExitProcess/HeapAlloc/
//! GetProcessHeap/RtlMoveMemory/memcmp/sprintf。入口 main(I32) 由 CRT
//! mainCRTStartup 调用。已知限制: cranelift-object 不写 .pdata/.xdata
//! — console 程序无碍; SEH 展开穿越 Alias 帧暂不支持。
//!
//! 已知有意缺口 (MIGRATION.md M10/M12): 函数体对未定义名的引用在编译期
//! 报错 (解释器为运行时); 打印静态类型不可知的表达式被拒绝。
//!
//! allow: SIZE_OK — 依赖清单强制 codegen.rs 为 cranelift 唯一拥有者。

use crate::ast::*;
use crate::{AliasError, AliasResult, Span};
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{
    AbiParam, Block, BlockArg, Function, InstBuilder, MemFlagsData, Signature, TrapCode,
    UserFuncName, Value,
};
use cranelift_codegen::settings;
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, FuncId, Linkage, Module};
use std::collections::{HashMap, HashSet};
use std::collections::VecDeque;

/// 除零中止存根的 span 回查表: ID → (line, col)。
/// 仅 JIT 路径使用 (宿主 abort 读); AOT 路径以只读数据段内嵌同一表
/// (见 define_span_data / abort shim)。
static SPAN_TABLE: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());

// ---------------------------------------------------------------------------
// 静态类型投影 — 打印分派 / 字宽转换 / 调用返回类型所需
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum VTy {
    Int,
    Bool,
    Str,
    Unit,
    Func,
    /// 结构体实例 — 携带结构体名供字段偏移回查与打印分派
    Struct(String),
    /// result<T,E> 实例 (Phase 2b) — 携带类型名供臂绑定静态投影;
    /// 打印只看运行时 tag (<ok>/<err>), 名字不参与显示
    Result(String, String),
    Other,
}

/// 结构体布局表: 名字 → (字段名, 声明默认值, 字段静态类型) 按声明序。
/// 偏移 = 下标*8, 由序隐含; sema 已校验全覆盖/类型/重名。
/// 字段 VTy 供嵌套字段链的打印分派与偏移回查。
type StructTable = HashMap<String, Vec<(String, Option<Expr>, VTy)>>;

fn decl_vty(te: &TypeExpr, structs: &StructTable) -> VTy {
    match te {
        TypeExpr::Named(n) => match n.as_str() {
            "i32" => VTy::Int,
            "bool" => VTy::Bool,
            "string" => VTy::Str,
            "unit" => VTy::Unit,
            "func" => VTy::Func,
            _ => {
                if structs.contains_key(n) {
                    VTy::Struct(n.clone())
                } else {
                    VTy::Other
                }
            }
        },
        TypeExpr::Generic(n, args) => {
            if n == "result" && args.len() == 2 {
                VTy::Result(args[0].display(), args[1].display())
            } else {
                VTy::Other
            }
        }
    }
}

/// 类型显示名 → 静态投影 (match 臂绑定回查用): result<T,E> 的 T/E 名
/// 经 decl_vty 同一词汇表反解; 嵌套 result 名等未知形态 → Other (打印被拒)
fn vty_of_type_name(structs: &StructTable, name: &str) -> VTy {
    match name {
        "i32" => VTy::Int,
        "bool" => VTy::Bool,
        "string" => VTy::Str,
        "unit" => VTy::Unit,
        "func" => VTy::Func,
        _ => {
            if structs.contains_key(name) {
                VTy::Struct(name.to_string())
            } else {
                VTy::Other
            }
        }
    }
}

/// 名字解析: 局部 SSA 变量(持单元格指针)或顶层槽区偏移。
#[derive(Clone, Copy)]
enum Slot {
    Local(Variable),
    Global(usize),
}

/// 词法帧: 作用域链 + globals/env 寄存器 + 本函数捕获表。
/// terminated: 当前块是否已发射终结指令 (frontend 不公开该查询, 自行跟踪)。
struct Frame {
    scopes: Vec<HashMap<String, Slot>>,
    locals_vty: Vec<HashMap<String, VTy>>,
    globals: Variable,
    env: Option<Variable>,
    caps: HashMap<String, usize>,
    /// 捕获项静态类型 (打印分派/字段偏移回查所需) — 捕获名不在本帧
    /// 作用域链也不在 globals, 类型信息必须随捕获表跨帧传递
    caps_vty: HashMap<String, VTy>,
    terminated: bool,
    /// 顶层初始化器语境: 名字仅随项序可见 (禁回退全量表 — 前向引用必须报错)
    init_ctx: bool,
    /// 函数返回块 (match never 臂 / expr? 的 return 跳转目标);
    /// None = 入口 wrapper / 推断语境 — 此类语境 sema 已拒绝 return 流
    ret_block: Option<Block>,
}

/// 顶层初始化器语境可见性由 env.scopes[0] 渐增插入实现 (insert-after-eval),
/// 函数体语境则全量可见 — 与迁移前逐项插入语义一致。
/// M: JITModule | ObjectModule — 发射机器经 Module trait 单份共享。
struct Compiler<'m, M: Module> {
    module: &'m mut M,
    cc: cranelift_codegen::isa::CallConv,
    ptr_ty: cranelift_codegen::ir::Type,
    globals_final: HashMap<String, (usize, VTy)>,
    global_slots: usize,
    next_fid: u32,
    /// 运行时函数 ID → 定稿用 FuncId
    fn_ids: Vec<FuncId>,
    /// 运行时函数 ID → 返回类型 (打印分派)
    fn_rets: Vec<VTy>,
    /// 名字 → 声明返回类型 (具名调用的打印分派; 遮蔽近似, 语料内无歧义)
    fn_ret_by_name: HashMap<String, VTy>,
    pending: VecDeque<PendingFn>,
    str_data: HashMap<String, cranelift_module::DataId>,
    /// 除零守卫 span 表: ID = 下标。JIT 定稿后拷入 SPAN_TABLE;
    /// AOT 序列化为只读数据段供 abort shim 回查。
    span_table: Vec<(u32, u32)>,
    /// 结构体布局表 (Phase 2a): 由 Program 项预扫描登记
    struct_layouts: StructTable,
    /// 方法表 (Phase 2c): (接收者类型名, 方法名) → FuncId — 静态分派;
    /// sema 已校验存在性/元数/实参类型, 此处只做发射
    methods: HashMap<(String, String), FuncId>,
    /// 方法返回类型 (链式调用的静态投影 / 打印分派)
    method_rets: HashMap<(String, String), VTy>,
    /// AOT 模式: 入口为导出的 main(I32); JIT: alias_entry(I64) 宿主读取
    is_aot: bool,
}

struct PendingFn {
    fid: FuncId,
    params: Vec<Param>,
    body: Body,
    /// 捕获项 (名字, 静态类型) — 类型随捕获表传递供闭包体内分派
    caps: Vec<(String, VTy)>,
    ret_vty: VTy,
}

pub fn execute(program: Program) -> AliasResult<i32> {
    // import 只解析不执行 — 通知文本与黄金记录逐字节一致 (spec-notes §三.5)
    if !program.imports.is_empty() {
        eprintln!(
            "[alias] 注意: {} 条 import 已解析但标准库尚未接入 (Phase 5 前)",
            program.imports.len()
        );
    }
    SPAN_TABLE.lock().expect("SPAN_TABLE 锁中毒").clear();

    let flag_builder = settings::builder();
    let isa = cranelift_native::builder()
        .map_err(|e| native_err(Span::default(), format!("ISA 探测失败: {e}")))?
        .finish(settings::Flags::new(flag_builder))
        .map_err(|e| native_err(Span::default(), format!("ISA 构造失败: {e}")))?;
    let mut jb = JITBuilder::with_isa(isa, default_libcall_names());
    register_host_fns(&mut jb);
    let mut module = JITModule::new(jb);

    let mut c = Compiler {
        cc: module.isa().default_call_conv(),
        ptr_ty: module.isa().pointer_type(),
        module: &mut module,
        globals_final: HashMap::new(),
        global_slots: 0,
        next_fid: 0,
        fn_ids: Vec::new(),
        fn_rets: Vec::new(),
        fn_ret_by_name: HashMap::new(),
        pending: VecDeque::new(),
        str_data: HashMap::new(),
        span_table: Vec::new(),
        struct_layouts: HashMap::new(),
        methods: HashMap::new(),
        method_rets: HashMap::new(),
        is_aot: false,
    };

    let entry_fid = compile_program(&mut c, &program.items)?;
    let span_table = std::mem::take(&mut c.span_table);
    c.module
        .finalize_definitions()
        .map_err(|e| native_err(Span::default(), format!("JIT 定稿失败: {e}")))?;
    *SPAN_TABLE.lock().expect("SPAN_TABLE 锁中毒") = span_table;

    // 定稿已完成; 模块存活至本次调用结束
    let entry_ptr = c.module.get_finalized_function(entry_fid);
    let entry: extern "C" fn() -> i64 = unsafe { std::mem::transmute(entry_ptr) };
    Ok(entry() as i32)
}

/// AOT: 编译为 x86_64 COFF 目标文件字节流。
/// 运行时 shim 区在同一 object 内定义 (Export), 经 kernel32/msvcrt 解析;
/// 产物入口为 CRT mainCRTStartup 调用的导出 main(I32)。
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
        global_slots: 0,
        next_fid: 0,
        fn_ids: Vec::new(),
        fn_rets: Vec::new(),
        fn_ret_by_name: HashMap::new(),
        pending: VecDeque::new(),
        str_data: HashMap::new(),
        span_table: Vec::new(),
        struct_layouts: HashMap::new(),
        methods: HashMap::new(),
        method_rets: HashMap::new(),
        is_aot: true,
    };

    emit_runtime_shims(&mut c)?;
    compile_program(&mut c, &program.items)?;

    let span_table = std::mem::take(&mut c.span_table);
    define_span_data(&mut c, &span_table)?;
    drop(c); // 归还模块借用 — finish 消费所有权 (ObjectModule 无 Copy)

    let product = module.finish();
    product
        .emit()
        .map_err(|e| native_err(Span::default(), format!("COFF 发射失败: {e}")))
}

/// 双后端共享编排: pass1 全局槽位 → pass2 函数体 → 入口 wrapper → 排空派生队列。
fn compile_program<M: Module>(c: &mut Compiler<'_, M>, items: &[Item]) -> AliasResult<FuncId> {
    // ---- pass 0: 结构体布局登记 (构造/字段访问发射所需; sema 已校验合法性) ----
    for item in items {
        if let Item::StructDef(sd) = item {
            let layouts = &c.struct_layouts;
            let layout = sd
                .fields
                .iter()
                .map(|f| (f.name.clone(), f.default.clone(), decl_vty(&f.ty, layouts)))
                .collect();
            c.struct_layouts.insert(sd.name.clone(), layout);
        }
    }

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
        let fid =
            c.declare_fn_named(format!("m<{recv}>{mname}"), params.len() + 1)?;
        let key = (recv.clone(), mname.clone());
        c.methods.insert(key.clone(), fid);
        c.method_rets.insert(key, decl_vty(&b.ty, &c.struct_layouts));
        pending_methods.push((fid, b));
    }

    // ---- pass 1: 顶层绑定全部分配全局槽位 (func 绑定的槽位存其闭包对象);
    //      main 取最后同名 func 绑定 (last-wins, spec-notes Q④ 现状锚点);
    //      方法不是绑定 — 不占槽位不入命名空间 ----
    let mut main_slot_ret: Option<(usize, VTy)> = None;
    let mut top_funcs: Vec<(FuncId, usize, &Binding)> = Vec::new();
    for b in items.iter().filter_map(|i| match i {
        Item::Binding(b) if b.receiver.is_none() => Some(b),
        _ => None,
    }) {
        let slot = c.global_slots;
        c.global_slots += 1;
        let vty = decl_vty(&b.ty, &c.struct_layouts);
        // func 绑定的槽位存闭包对象 — 槽位类型恒为 Func (decl_vty 是其返回类型)
        let slot_vty = if b.kind == BindKind::Func { VTy::Func } else { vty.clone() };
        c.globals_final.insert(b.name.clone(), (slot, slot_vty));
        if b.kind == BindKind::Func {
            let Expr::FuncLit { params, .. } = &b.value else {
                return Err(native_err(b.span, "函数值尚未接入原生后端 (Phase 3)"));
            };
            let fid = c.declare_user_func(params.len())?;
            c.fn_ids.push(fid);
            c.fn_rets.push(vty.clone());
            c.fn_ret_by_name.insert(b.name.clone(), vty.clone());
            top_funcs.push((fid, slot, b));
            if b.name == "main" {
                main_slot_ret = Some((slot, vty));
            }
        }
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
        let self_param =
            Param { ty: TypeExpr::Named(recv.clone()), name: "self".into(), span: b.span };
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

fn native_err(span: Span, msg: impl Into<String>) -> AliasError {
    AliasError { msg: msg.into(), span }
}

/// 内部不变式违例 (sema 之后不应可达) — 编译器 bug 走 panic, 不伪装成用户错误。
fn invariant_violation(what: &'static str) -> ! {
    panic!("内部代码生成不变式被破坏: {what} (sema 校验缺口, 请报告)")
}
