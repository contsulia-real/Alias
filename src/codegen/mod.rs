//! Alias 唯一原生代码生成后端 — Cranelift 发射 COFF，rust-lld 链接为 exe。
//!
//! ast/sema/lib/linker 不接触 cranelift 类型；值 ABI 的唯一真相源在 abi.rs。

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

#[derive(Clone, Copy)]
pub(crate) enum Slot {
    Local(Variable),
    Global(usize),
}

#[derive(Clone)]
pub(crate) struct Frame {
    pub(crate) scopes: Vec<HashMap<String, Slot>>,
    pub(crate) locals_vty: Vec<HashMap<String, VTy>>,
    pub(crate) globals: Variable,
    pub(crate) env: Option<Variable>,
    pub(crate) caps: HashMap<String, usize>,
    pub(crate) caps_vty: HashMap<String, VTy>,
    pub(crate) terminated: bool,
    /// 当前函数内由内向外的循环目标：(break 目标, continue 目标)。
    /// 创建新函数帧时永远从空栈开始，禁止跨函数 break/continue。
    pub(crate) loop_targets: Vec<(Block, Block)>,
    init_ctx: bool,
    ret_block: Option<Block>,
    ret_vty: Option<VTy>,
}

pub(crate) struct Compiler<'m, M: Module> {
    pub(crate) module: &'m mut M,
    pub(crate) cc: cranelift_codegen::isa::CallConv,
    pub(crate) ptr_ty: cranelift_codegen::ir::Type,
    pub(crate) globals_final: HashMap<String, (usize, VTy)>,
    pub(crate) top_slots: Vec<usize>,
    pub(crate) global_bytes: usize,
    pub(crate) next_fid: u32,
    pub(crate) fn_ids: Vec<FuncId>,
    pub(crate) fn_rets: Vec<VTy>,
    pub(crate) pending: VecDeque<PendingFn>,
    pub(crate) str_data: HashMap<String, cranelift_module::DataId>,
    pub(crate) span_table: Vec<(u32, u32)>,
    struct_layouts: StructTable,
    /// (完整接收者静态类型名, 方法名) → FuncId。
    methods: HashMap<(String, String), FuncId>,
    method_rets: HashMap<(String, String), VTy>,
    method_sigs: HashMap<(String, String), (Vec<VTy>, VTy)>,
    runtime_defined: HashSet<&'static str>,
}

pub(crate) struct PendingFn {
    fid: FuncId,
    params: Vec<Param>,
    body: Body,
    caps: Vec<(String, VTy)>,
    ret_vty: VTy,
}

pub fn compile_to_object(program: Program) -> AliasResult<Vec<u8>> {
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
    drop(c);

    let product = module.finish();
    product
        .emit()
        .map_err(|e| native_err(Span::default(), format!("COFF 发射失败: {e}")))
}

/// 原生编译编排: 结构布局 → 方法签名 → 顶层槽位 → 函数/方法体 → 入口。
fn compile_program<M: Module>(c: &mut Compiler<'_, M>, items: &[Item]) -> AliasResult<FuncId> {
    c.struct_layouts = build_struct_layouts(items);
    debug_assert!(c
        .struct_layouts
        .values()
        .all(|layout| (layout.size as usize).is_multiple_of(layout.align)));

    let mut pending_methods: Vec<(FuncId, &Binding)> = Vec::new();
    for b in items.iter().filter_map(|i| match i {
        Item::Binding(b) if b.receiver.is_some() => Some(b),
        _ => None,
    }) {
        let Some(recv) = &b.receiver else {
            invariant_violation("过滤保证 receiver 存在")
        };
        let Expr::FuncLit { params, .. } = &b.value else {
            return Err(native_err(b.span, "方法体必须是函数字面量"));
        };
        let self_vty = decl_vty(recv, &c.struct_layouts);
        let recv_name = self_vty.display_name();
        let mname = b.name.clone();
        let param_vtys: Vec<VTy> = std::iter::once(self_vty)
            .chain(params.iter().map(|p| decl_vty(&p.ty, &c.struct_layouts)))
            .collect();
        let ret_vty = decl_vty(&b.ty, &c.struct_layouts);
        let fid =
            c.declare_user_func_typed(&param_vtys, &ret_vty, format!("m<{recv_name}>{mname}"))?;
        let key = (recv_name, mname);
        c.methods.insert(key.clone(), fid);
        c.method_rets.insert(key.clone(), ret_vty.clone());
        c.method_sigs.insert(key, (param_vtys, ret_vty));
        pending_methods.push((fid, b));
    }

    let mut main_slot_ret: Option<(usize, VTy)> = None;
    let mut top_funcs: Vec<(FuncId, usize, &Binding)> = Vec::new();
    {
        let mut off = 0usize;
        for b in items.iter().filter_map(|i| match i {
            Item::Binding(b) if b.receiver.is_none() => Some(b),
            _ => None,
        }) {
            let vty = decl_vty(&b.ty, &c.struct_layouts);
            let slot_vty = if b.kind == BindKind::Func {
                let Expr::FuncLit { params, .. } = &b.value else {
                    return Err(native_err(b.span, "func 绑定必须由函数字面量初始化"));
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
                    return Err(native_err(b.span, "func 绑定必须由函数字面量初始化"));
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

    for (fid, _slot, b) in top_funcs {
        let Expr::FuncLit { params, body, .. } = &b.value else {
            unreachable!("pass 1 已确保 func 绑定初始化为函数字面量");
        };
        let ret_vty = decl_vty(&b.ty, &c.struct_layouts);
        c.define_user_func(fid, params, body, Vec::new(), ret_vty)?;
    }

    for (fid, b) in pending_methods {
        let Expr::FuncLit { params, body, .. } = &b.value else {
            unreachable!("方法登记已确保函数字面量");
        };
        let Some(recv) = &b.receiver else {
            unreachable!("pending_methods 只收带接收者的绑定");
        };
        let self_param = Param {
            ty: recv.clone(),
            name: "self".into(),
            span: b.span,
        };
        let mut all_params = Vec::with_capacity(params.len() + 1);
        all_params.push(self_param);
        all_params.extend(params.iter().cloned());
        let ret_vty = decl_vty(&b.ty, &c.struct_layouts);
        c.define_user_func(fid, &all_params, body, Vec::new(), ret_vty)?;
    }

    let (main_slot, main_ret) =
        main_slot_ret.unwrap_or_else(|| invariant_violation("main 存在性 (sema Q④ 已校验)"));
    let entry_fid = c.compile_entry(items, main_slot, main_ret)?;

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

pub(crate) fn invariant_violation(what: &'static str) -> ! {
    panic!("内部代码生成不变式被破坏: {what} (sema 校验缺口, 请报告)")
}
