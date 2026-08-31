//! Alias 唯一原生代码生成后端 — Cranelift 发射 COFF，rust-lld 链接为 exe。
//!
//! ast/sema/lib/linker 不接触 cranelift 类型；值 ABI 的唯一真相源在 abi.rs。

pub(crate) mod abi;
mod emit;
mod funcgen;
pub(crate) mod layout;
mod native_runtime;
mod runtime;

use crate::sema::hir::{
    BindKind, Binding, BindingId, BindingOwner, Body, CheckedProgram, Expr, Item, MethodId, Param,
    StorageRelation,
};
use crate::sema::types::Ty;
use crate::target::TARGET_TRIPLE;
use crate::{AliasError, AliasResult, Span};
use cranelift_codegen::ir::Block;
use cranelift_codegen::settings;
use cranelift_codegen::Context;
use cranelift_frontend::Variable;
use cranelift_module::{default_libcall_names, FuncId, Module};
use std::collections::{HashMap, HashSet, VecDeque};
use std::str::FromStr;

use abi::{
    align_to, build_struct_layouts, project_ty, projected_ty, size_align, ProjectionTable,
    StructTable, VTy,
};
use native_runtime::{define_span_data, emit_native_runtime};

#[derive(Clone, Copy)]
pub(crate) enum Slot {
    Local(Variable),
    Global(usize),
}

#[derive(Clone)]
pub(crate) struct Frame {
    pub(crate) scopes: Vec<HashMap<BindingId, Slot>>,
    pub(crate) locals_vty: Vec<HashMap<BindingId, VTy>>,
    pub(crate) locals_relation: Vec<HashMap<BindingId, Option<StorageRelation>>>,
    pub(crate) globals: Variable,
    pub(crate) env: Option<Variable>,
    pub(crate) caps: HashMap<BindingId, usize>,
    pub(crate) caps_vty: HashMap<BindingId, VTy>,
    pub(crate) caps_relation: HashMap<BindingId, Option<StorageRelation>>,
    pub(crate) this_fid: Option<FuncId>,
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
    pub(crate) globals_final: HashMap<BindingId, (usize, VTy, Option<StorageRelation>)>,
    pub(crate) top_slots: Vec<usize>,
    pub(crate) global_bytes: usize,
    pub(crate) next_fid: u32,
    pub(crate) pending: VecDeque<PendingFn>,
    pub(crate) str_data: HashMap<String, cranelift_module::DataId>,
    pub(crate) span_table: Vec<(u32, u32)>,
    type_projections: ProjectionTable,
    struct_layouts: StructTable,
    /// sema 已解析 MethodId → 原生函数/签名。codegen 禁止按 receiver/name 查方法。
    methods: HashMap<MethodId, FuncId>,
    method_sigs: HashMap<MethodId, (Vec<VTy>, VTy)>,
    runtime_defined: HashSet<&'static str>,
}

pub(crate) struct PendingFn {
    fid: FuncId,
    params: Vec<Param>,
    body: Body,
    caps: Vec<(BindingId, VTy, Option<StorageRelation>)>,
    ret_vty: VTy,
}

pub(crate) fn compile_to_object(program: CheckedProgram) -> AliasResult<Vec<u8>> {
    let flag_builder = settings::builder();
    let triple = target_lexicon::Triple::from_str(TARGET_TRIPLE)
        .map_err(|error| native_err(Span::default(), format!("目标 triple 无效: {error}")))?;
    // 链接器和 SDK 固定为 Windows x64 MSVC，因此 object ISA 必须使用同一显式目标。
    // 使用宿主探测会让非 x64 宿主产出与 COFF/linker 合同不一致的机器码。
    let isa = cranelift_codegen::isa::lookup(triple)
        .map_err(|error| native_err(Span::default(), format!("目标 ISA 不可用: {error}")))?
        .finish(settings::Flags::new(flag_builder))
        .map_err(|e| native_err(Span::default(), format!("ISA 构造失败: {e}")))?;
    let builder = cranelift_object::ObjectBuilder::new(
        isa.clone(),
        b"alias_out".to_vec(),
        default_libcall_names(),
    )
    .map_err(|e| native_err(Span::default(), format!("object builder 构造失败: {e}")))?;
    let mut module = cranelift_object::ObjectModule::new(builder);

    let type_projections = project_ty(&program);
    let mut c = Compiler {
        cc: module.isa().default_call_conv(),
        ptr_ty: module.isa().pointer_type(),
        module: &mut module,
        globals_final: HashMap::new(),
        top_slots: Vec::new(),
        global_bytes: 0,
        next_fid: 0,
        pending: VecDeque::new(),
        str_data: HashMap::new(),
        span_table: Vec::new(),
        type_projections,
        struct_layouts: HashMap::new(),
        methods: HashMap::new(),
        method_sigs: HashMap::new(),
        runtime_defined: HashSet::new(),
    };

    emit_native_runtime(&mut c)?;
    compile_program(&mut c, &program.items, program.main_id)?;

    let span_table = std::mem::take(&mut c.span_table);
    define_span_data(&mut c, &span_table)?;
    drop(c);

    let product = module.finish();
    product
        .emit()
        .map_err(|e| native_err(Span::default(), format!("COFF 发射失败: {e}")))
}

/// 原生编译编排: 结构布局 → 方法签名 → 顶层槽位 → 函数/方法体 → 入口。
fn compile_program<M: Module>(
    c: &mut Compiler<'_, M>,
    items: &[Item],
    main_id: BindingId,
) -> AliasResult<FuncId> {
    c.struct_layouts = build_struct_layouts(items, &c.type_projections);
    if c.struct_layouts
        .values()
        .any(|layout| !(layout.size as usize).is_multiple_of(layout.align))
    {
        return Err(native_err(
            Span::default(),
            "内部: 结构体最终大小未按最大字段对齐量取整",
        ));
    }

    let mut pending_methods: Vec<(FuncId, &Binding)> = Vec::new();
    for b in items.iter().filter_map(|i| match i {
        Item::Binding(b) if b.is_method() => Some(b),
        _ => None,
    }) {
        let BindingOwner::Method {
            method_id,
            receiver: recv,
            ..
        } = &b.owner
        else {
            invariant_violation("方法过滤必须产生 Method owner")
        };
        let Expr::FuncLit { .. } = &b.value else {
            return Err(native_err(b.span, "方法体必须是函数字面量"));
        };
        let self_vty = c.vty(recv);
        let VTy::Func(param_vtys, ret_vty) = c.vty(&b.ty) else {
            invariant_violation("方法绑定携带完整函数类型 (sema 已校验)")
        };
        if param_vtys.first() != Some(&self_vty) {
            invariant_violation("方法函数类型首参数必须是接收者")
        }
        let ret_vty = *ret_vty;
        // MethodId 已由 sema 唯一解析；内部 object 符号只需要结构化身份，不应再维护
        // 一套 receiver 的语言类型拼写来制造名字。
        let symbol = format!("m{}_{}", method_id.0, b.name);
        let fid = c.declare_user_func_typed(&param_vtys, &ret_vty, symbol)?;
        c.methods.insert(*method_id, fid);
        c.method_sigs.insert(*method_id, (param_vtys, ret_vty));
        pending_methods.push((fid, b));
    }

    let mut main_slot_ret: Option<(usize, VTy)> = None;
    let mut top_funcs: Vec<(FuncId, usize, &Binding)> = Vec::new();
    {
        let mut off = 0usize;
        for b in items.iter().filter_map(|i| match i {
            Item::Binding(b) if !b.is_method() => Some(b),
            _ => None,
        }) {
            let slot_vty = c.vty(&b.ty);
            let (sz, al) = size_align(&slot_vty);
            off = align_to(off, al);
            let slot = off;
            off += sz;
            c.top_slots.push(slot);
            c.globals_final
                .insert(b.binding_id, (slot, slot_vty, b.relation));
            if b.kind == BindKind::Func {
                let Expr::FuncLit { .. } = &b.value else {
                    return Err(native_err(b.span, "func 绑定必须由函数字面量初始化"));
                };
                let VTy::Func(param_vtys, ret_vty) = c.vty(&b.ty) else {
                    invariant_violation("func 绑定携带完整函数类型 (sema 已校验)")
                };
                let ret_vty = *ret_vty;
                let name = format!("u{}", c.next_fid);
                c.next_fid += 1;
                let fid = c.declare_user_func_typed(&param_vtys, &ret_vty, name)?;
                top_funcs.push((fid, slot, b));
                if b.binding_id == main_id {
                    main_slot_ret = Some((slot, ret_vty));
                }
            }
        }
        c.global_bytes = align_to(off, 8);
    }

    for (fid, _slot, b) in top_funcs {
        let Expr::FuncLit {
            params,
            body,
            captures,
            ..
        } = &b.value
        else {
            unreachable!("pass 1 已确保 func 绑定初始化为函数字面量");
        };
        if !captures.is_empty() {
            invariant_violation("顶层函数不应捕获局部绑定")
        }
        let VTy::Func(_, ret_vty) = c.vty(&b.ty) else {
            invariant_violation("func 绑定携带完整函数类型")
        };
        c.define_user_func(fid, params, body, Vec::new(), *ret_vty)?;
    }

    for (fid, b) in pending_methods {
        let Expr::FuncLit {
            params,
            body,
            captures,
            ..
        } = &b.value
        else {
            unreachable!("方法登记已确保函数字面量");
        };
        if !captures.is_empty() {
            invariant_violation("顶层方法不应捕获局部绑定")
        }
        let BindingOwner::Method {
            self_id,
            receiver: recv,
            ..
        } = &b.owner
        else {
            unreachable!("pending_methods 只收方法绑定");
        };
        let Ty::Func {
            param_effects: Some(param_effects),
            ..
        } = &b.ty
        else {
            invariant_violation("方法绑定缺少 resolved parameter effects")
        };
        let Some(self_effect) = param_effects.first().copied() else {
            invariant_violation("方法绑定缺少 self parameter effect")
        };
        let self_param = Param {
            binding_id: *self_id,
            ty: recv.clone(),
            effect: Some(self_effect),
        };
        let mut all_params = Vec::with_capacity(params.len() + 1);
        all_params.push(self_param);
        all_params.extend(params.iter().cloned());
        let VTy::Func(_, ret_vty) = c.vty(&b.ty) else {
            invariant_violation("方法绑定携带完整函数类型")
        };
        c.define_user_func(fid, &all_params, body, Vec::new(), *ret_vty)?;
    }

    let (main_slot, main_ret) =
        main_slot_ret.unwrap_or_else(|| invariant_violation("main 存在性 (sema 已校验)"));
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

impl<M: Module> Compiler<'_, M> {
    pub(crate) fn vty(&self, ty: &Ty) -> VTy {
        projected_ty(&self.type_projections, ty)
    }

    /// verifier 是目标文件边界的最后一道内部一致性检查；失败后绝不能继续定义函数。
    pub(crate) fn define_verified_function(
        &mut self,
        fid: FuncId,
        ctx: &mut Context,
        owner: &str,
    ) -> AliasResult<()> {
        if let Err(error) = ctx.verify(self.module.isa()) {
            return Err(native_err(
                Span::default(),
                format!(
                    "内部: {owner} 的 Cranelift verifier 失败: {error}\n[Cranelift 中间表示]\n{}",
                    ctx.func
                ),
            ));
        }
        self.module
            .define_function(fid, ctx)
            .map_err(|error| native_err(Span::default(), format!("内部: {owner} 定义失败 {error}")))
    }
}

pub(crate) fn bound_vty<M: Module>(c: &Compiler<M>, frame: &Frame, id: BindingId) -> VTy {
    for vtys in frame.locals_vty.iter().rev() {
        if let Some(vty) = vtys.get(&id) {
            return vty.clone();
        }
    }
    if let Some(vty) = frame.caps_vty.get(&id) {
        return vty.clone();
    }
    c.globals_final
        .get(&id)
        .map(|(_, vty, _)| vty.clone())
        .unwrap_or_else(|| invariant_violation("BindingId 必须在 sema 后解析到存储"))
}

pub(crate) fn bound_relation<M: Module>(
    c: &Compiler<M>,
    frame: &Frame,
    id: BindingId,
) -> Option<StorageRelation> {
    for relations in frame.locals_relation.iter().rev() {
        if let Some(relation) = relations.get(&id) {
            return *relation;
        }
    }
    if let Some(relation) = frame.caps_relation.get(&id) {
        return *relation;
    }
    c.globals_final
        .get(&id)
        .map(|(_, _, relation)| *relation)
        .unwrap_or_else(|| invariant_violation("BindingId relation 必须解析到存储"))
}

#[cfg(test)]
mod fail_closed_tests {
    use super::Compiler;
    use crate::target::TARGET_TRIPLE;
    use cranelift_codegen::ir::types;
    use cranelift_codegen::ir::{AbiParam, Function, InstBuilder, Signature, UserFuncName};
    use cranelift_codegen::settings;
    use cranelift_codegen::Context;
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use cranelift_module::{default_libcall_names, FuncId, Linkage, Module};
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::str::FromStr;

    fn return_context(
        fid: FuncId,
        sig: Signature,
        valid: bool,
        frontend: cranelift_codegen::isa::TargetFrontendConfig,
    ) -> Context {
        let mut ctx = Context::new();
        ctx.func = Function::with_name_signature(UserFuncName::user(0xfe, fid.as_u32()), sig);
        let mut fbctx = FunctionBuilderContext::new();
        let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        let entry = bcx.create_block();
        bcx.switch_to_block(entry);
        bcx.seal_block(entry);
        if valid {
            let zero = bcx.ins().iconst(types::I64, 0);
            bcx.ins().return_(&[zero]);
        } else {
            bcx.ins().return_(&[]);
        }
        bcx.finalize(frontend);
        ctx
    }

    #[test]
    fn verifier_failure_does_not_define_the_function() {
        let triple = target_lexicon::Triple::from_str(TARGET_TRIPLE).unwrap();
        let isa = cranelift_codegen::isa::lookup(triple)
            .unwrap()
            .finish(settings::Flags::new(settings::builder()))
            .unwrap();
        let builder = cranelift_object::ObjectBuilder::new(
            isa,
            b"verifier-test".to_vec(),
            default_libcall_names(),
        )
        .unwrap();
        let mut module = cranelift_object::ObjectModule::new(builder);
        let mut sig = Signature::new(module.isa().default_call_conv());
        sig.returns.push(AbiParam::new(types::I64));
        let fid = module
            .declare_function("invalid_then_valid", Linkage::Local, &sig)
            .unwrap();
        let cc = module.isa().default_call_conv();
        let ptr_ty = module.isa().pointer_type();
        let frontend = module.target_config();
        let mut compiler = Compiler {
            module: &mut module,
            cc,
            ptr_ty,
            globals_final: HashMap::new(),
            top_slots: Vec::new(),
            global_bytes: 0,
            next_fid: 0,
            pending: VecDeque::new(),
            str_data: HashMap::new(),
            span_table: Vec::new(),
            type_projections: HashMap::new(),
            struct_layouts: HashMap::new(),
            methods: HashMap::new(),
            method_sigs: HashMap::new(),
            runtime_defined: HashSet::new(),
        };

        let mut invalid = return_context(fid, sig.clone(), false, frontend);
        let error = compiler
            .define_verified_function(fid, &mut invalid, "故意损坏函数")
            .expect_err("无返回值 IR 必须被 verifier 拒绝");
        assert!(error.msg.contains("Cranelift verifier 失败"));
        assert!(error.msg.contains("[Cranelift 中间表示]"));

        // 同一个 FuncId 随后仍能定义，证明 verifier 失败路径没有调用 define_function。
        let mut valid = return_context(fid, sig, true, frontend);
        compiler
            .define_verified_function(fid, &mut valid, "修复后的函数")
            .expect("verifier 拒绝的函数不得占用定义槽");
    }
}
