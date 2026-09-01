use crate::codegen::abi::{cl_type, norm_load, norm_store, user_signature, value_word_offset, VTy};
use crate::codegen::emit::cells::{emit_local_cell, first_result};
use crate::codegen::emit::control::emit_body;
use crate::codegen::emit::expr::emit_expr;
use crate::codegen::layout::{CLOSURE_CODE_OFFSET, CLOSURE_ENV_OFFSET};
use crate::codegen::{
    bound_vty, invariant_violation, native_err, Compiler, Frame, PendingFn, Slot,
};
use crate::sema::hir::StorageRelation;
use crate::sema::hir::{BindKind, BindingId, Body, Capture, Expr, Item, Param};
use crate::sema::types::{IntW, ParamEffect, Ty};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::{
    types, Function, InstBuilder, MemFlagsData, Signature, TrapCode, UserFuncName, Value,
};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::HashMap;

impl<'m, M: Module> Compiler<'m, M> {
    pub(crate) fn user_sig_typed(&self, params: &[VTy], ret: &VTy) -> Signature {
        user_signature(self.cc, params, ret)
    }

    pub(crate) fn declare_user_func_typed(
        &mut self,
        params: &[VTy],
        ret: &VTy,
        name: String,
    ) -> AliasResult<FuncId> {
        let sig = self.user_sig_typed(params, ret);
        self.module
            .declare_function(&name, Linkage::Local, &sig)
            .map_err(|e| native_err(Span::default(), format!("内部: 函数声明失败 {e}")))
    }

    pub(crate) fn define_user_func(
        &mut self,
        fid: FuncId,
        params: &[Param],
        body: &Body,
        caps: Vec<(BindingId, VTy, Option<StorageRelation>)>,
        ret_vty: VTy,
    ) -> AliasResult<()> {
        let param_vtys: Vec<VTy> = params.iter().map(|p| self.vty(&p.ty)).collect();
        let sig = self.user_sig_typed(&param_vtys, &ret_vty);
        let mut ctx = Context::new();
        ctx.func = Function::with_name_signature(UserFuncName::user(0, fid.as_u32()), sig);
        let mut fbctx = FunctionBuilderContext::new();
        let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fbctx);

        let entry = bcx.create_block();
        bcx.append_block_params_for_function_params(entry);
        bcx.switch_to_block(entry);
        bcx.seal_block(entry);

        // user_signature 的前两个参数固定为 globals 与 closure env。显式参数从索引 2
        // 开始；这里若与 call_closure 的前缀顺序漂移，函数体会把环境指针当成全局区。
        let globals_v = bcx.declare_var(types::I64);
        bcx.def_var(globals_v, bcx.block_params(entry)[0]);
        let env_v = bcx.declare_var(types::I64);
        bcx.def_var(env_v, bcx.block_params(entry)[1]);
        let mut caps_map: HashMap<BindingId, usize> = HashMap::new();
        let mut caps_vty: HashMap<BindingId, VTy> = HashMap::new();
        let mut caps_relation: HashMap<BindingId, Option<StorageRelation>> = HashMap::new();
        for (i, (id, vty, relation)) in caps.iter().enumerate() {
            caps_map.insert(*id, i);
            caps_vty.insert(*id, vty.clone());
            caps_relation.insert(*id, *relation);
        }
        let mut frame = Frame {
            scopes: vec![HashMap::new()],
            locals_vty: vec![HashMap::new()],
            locals_relation: vec![HashMap::new()],
            globals: globals_v,
            env: Some(env_v),
            caps: caps_map,
            caps_vty,
            caps_relation,
            this_fid: Some(fid),
            terminated: false,
            loop_targets: Vec::new(),
            init_ctx: false,
            ret_block: None,
            ret_vty: Some(ret_vty.clone()),
        };

        for (i, p) in params.iter().enumerate() {
            let raw = bcx.block_params(entry)[i + 2];
            let vty = self.vty(&p.ty);
            let effect = p
                .effect
                .unwrap_or_else(|| invariant_violation("用户函数参数缺少 resolved effect"));
            let borrowed = matches!(effect, ParamEffect::ReadBorrow | ParamEffect::WriteBorrow);
            let word = if borrowed {
                raw
            } else {
                norm_load(&mut bcx, raw, &vty)
            };
            emit_local_cell(
                self,
                &mut bcx,
                &mut frame,
                word,
                vty,
                p.binding_id,
                Some(if borrowed {
                    StorageRelation::Borrowed
                } else {
                    StorageRelation::Owning
                }),
            )?;
        }

        let ret_block = bcx.create_block();
        frame.ret_block = Some(ret_block);
        let ret_val = if ret_vty == VTy::Unit {
            None
        } else {
            Some(bcx.append_block_param(ret_block, cl_type(&ret_vty)))
        };
        emit_body(self, &mut bcx, &mut frame, body, ret_block)?;
        if !frame.terminated {
            if ret_vty == VTy::Unit {
                bcx.ins().jump(ret_block, &[]);
            } else {
                return Err(native_err(
                    Span::default(),
                    "内部: 非 unit 函数存在可达落空路径，sema 返回路径不变式被破坏",
                ));
            }
        }
        bcx.switch_to_block(ret_block);
        bcx.seal_block(ret_block);
        if let Some(ret_val) = ret_val {
            bcx.ins().return_(&[ret_val]);
        } else {
            bcx.ins().return_(&[]);
        }
        bcx.finalize(self.module.target_config());
        self.define_verified_function(fid, &mut ctx, "用户函数")
    }

    pub(crate) fn compile_entry(
        &mut self,
        items: &[Item],
        main_slot: usize,
        main_ret: VTy,
    ) -> AliasResult<FuncId> {
        if main_ret != VTy::I(IntW::W32) {
            return Err(native_err(
                Span::default(),
                "内部: sema 未将 main 收紧为 i32",
            ));
        }
        let entry_sig = Signature::new(self.cc);
        let fid = self
            .module
            .declare_function("alias_start", Linkage::Export, &entry_sig)
            .map_err(|e| native_err(Span::default(), format!("内部: 入口声明失败 {e}")))?;
        let mut ctx = Context::new();
        ctx.func = Function::with_name_signature(UserFuncName::user(0, fid.as_u32()), entry_sig);
        let mut fbctx = FunctionBuilderContext::new();
        let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fbctx);

        let entry = bcx.create_block();
        bcx.switch_to_block(entry);
        bcx.seal_block(entry);

        let byte_count = bcx.ins().iconst(types::I64, self.global_bytes as i64);
        let gword = self.call_rt(&mut bcx, "alias.globals.new", &[byte_count])?;
        let globals_v = bcx.declare_var(types::I64);
        bcx.def_var(globals_v, gword);
        let abort_ret = bcx.create_block();
        let _abort_code = bcx.append_block_param(abort_ret, types::I32);
        let mut frame = Frame {
            scopes: vec![HashMap::new()],
            locals_vty: vec![HashMap::new()],
            locals_relation: vec![HashMap::new()],
            globals: globals_v,
            env: None,
            caps: HashMap::new(),
            caps_vty: HashMap::new(),
            caps_relation: HashMap::new(),
            this_fid: None,
            terminated: false,
            loop_targets: Vec::new(),
            init_ctx: false,
            ret_block: Some(abort_ret),
            ret_vty: Some(VTy::I(IntW::W32)),
        };
        frame.init_ctx = true;

        // 顶层初始化必须保持源码顺序：sema 的可见性不允许前向引用，globals slot 表也
        // 以同一过滤顺序建立。改变排序会让 BindingId 与物理 slot 对应关系错位。
        for (binding_index, b) in items
            .iter()
            .filter_map(|i| match i {
                Item::Binding(b) if !b.is_method() => Some(b),
                _ => None,
            })
            .enumerate()
        {
            let (v, svty) = if b.kind == BindKind::Func {
                let Expr::FuncLit {
                    params,
                    body,
                    captures,
                    ..
                } = &b.value
                else {
                    return Err(native_err(b.span, "函数绑定必须由函数字面量初始化"));
                };
                let VTy::Func(param_vtys, ret_vty) = self.vty(&b.ty) else {
                    invariant_violation("局部 func 绑定携带完整函数类型")
                };
                let ret_vty = *ret_vty;
                let v = emit_funclit_value_typed(
                    self,
                    &mut bcx,
                    &mut frame,
                    params,
                    body,
                    captures,
                    ret_vty.clone(),
                )?;
                (v, VTy::Func(param_vtys, Box::new(ret_vty)))
            } else {
                let vty = self.vty(&b.ty);
                let v = emit_expr(self, &mut bcx, &mut frame, &b.value)?;
                (v, vty)
            };
            let off = self.top_slots[binding_index];
            let sv = norm_store(&mut bcx, v, &svty);
            let base = bcx.use_var(frame.globals);
            bcx.ins().store(MemFlagsData::new(), sv, base, off as i32);
            frame.scopes[0].insert(b.binding_id, Slot::Global(off));
            frame.locals_vty[0].insert(b.binding_id, svty);
            frame.locals_relation[0].insert(b.binding_id, b.relation);
        }

        let clo = {
            let base = bcx.use_var(frame.globals);
            bcx.ins()
                .load(types::I64, MemFlagsData::new(), base, main_slot as i32)
        };
        let code = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), clo, CLOSURE_CODE_OFFSET);
        let env = bcx
            .ins()
            .load(types::I64, MemFlagsData::new(), clo, CLOSURE_ENV_OFFSET);
        let msig = user_signature(self.cc, &[], &main_ret);
        let uref = bcx.func.import_signature(msig);
        let icall = bcx.ins().call_indirect(uref, code, &[gword, env]);
        let raw = first_result(&bcx, icall);
        let code_word = norm_load(&mut bcx, raw, &main_ret);
        let exit_code = bcx.ins().ireduce(types::I32, code_word);
        let ep = self.import_external("ExitProcess", &[types::I32], None)?;
        let epr = self.module.declare_func_in_func(ep, bcx.func);
        bcx.ins().call(epr, &[exit_code]);
        // ExitProcess 的不返回语义不在 Cranelift 外部签名中；trap 防止入口在异常返回时
        // 穿透到后续 block。下方编译期 abort return 汇合路径同样必须显式终止。
        bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);

        bcx.switch_to_block(abort_ret);
        bcx.seal_block(abort_ret);
        let ep = self.import_external("ExitProcess", &[types::I32], None)?;
        let epr = self.module.declare_func_in_func(ep, bcx.func);
        let one = bcx.ins().iconst(types::I32, 1);
        bcx.ins().call(epr, &[one]);
        bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);
        bcx.finalize(self.module.target_config());
        self.define_verified_function(fid, &mut ctx, "进程入口")?;
        Ok(fid)
    }
}

pub(crate) fn emit_funclit_value<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    params: &[Param],
    body: &Body,
    captures: &[Capture],
    funclit_type: &Ty,
) -> AliasResult<Value> {
    let VTy::Func(_, ret_vty) = c.vty(funclit_type) else {
        invariant_violation("函数字面量携带完整函数类型")
    };
    let ret_vty = *ret_vty;
    emit_funclit_value_typed(c, bcx, frame, params, body, captures, ret_vty)
}

pub(crate) fn emit_funclit_value_typed<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    params: &[Param],
    body: &Body,
    captures: &[Capture],
    ret_vty: VTy,
) -> AliasResult<Value> {
    let param_vtys: Vec<VTy> = params.iter().map(|p| c.vty(&p.ty)).collect();
    let name = format!("u{}", c.next_fid);
    c.next_fid += 1;
    let fid = c.declare_user_func_typed(&param_vtys, &ret_vty, name)?;
    let cap_vtys: Vec<(BindingId, VTy, Option<StorageRelation>)> = captures
        .iter()
        .map(|capture| {
            let id = capture.binding_id;
            (
                id,
                bound_vty(c, frame, id),
                crate::codegen::bound_relation(c, frame, id),
            )
        })
        .collect();
    // 先声明并排队，当前函数继续只发射 closure 对象；nested body 在外层 builder
    // 完成后定义，避免同时持有两个 FunctionBuilder。FuncId 已足以安全写入 code pointer。
    c.pending.push_back(PendingFn {
        fid,
        params: params.to_vec(),
        body: body.clone(),
        caps: cap_vtys,
        ret_vty,
    });

    // capture env 保存 cell 指针而不是当前值，闭包才能观察后续赋值。无捕获时使用
    // runtime contract 明确允许的 null env，且下方循环为空，绝不能解引用该 null。
    let env_word = if captures.is_empty() {
        bcx.ins().iconst(types::I64, 0)
    } else {
        let en = c.import_runtime("alias.env.new")?;
        let eref = c.module.declare_func_in_func(en, bcx.func);
        let len = bcx.ins().iconst(types::I32, captures.len() as i64);
        let ecall = bcx.ins().call(eref, &[len]);
        first_result(bcx, ecall)
    };
    for (i, capture) in captures.iter().enumerate() {
        let id = &capture.binding_id;
        let cellw = if let Some(idx) = frame.caps.get(id) {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            bcx.ins().load(
                types::I64,
                MemFlagsData::new(),
                base,
                value_word_offset(*idx),
            )
        } else {
            let mut found: Option<Value> = None;
            for scope in frame.scopes.iter().rev() {
                if let Some(Slot::Local(v)) = scope.get(id) {
                    found = Some(bcx.use_var(*v));
                    break;
                }
            }
            found.unwrap_or_else(|| invariant_violation("HIR 捕获 BindingId 必须解析到外层单元格"))
        };
        bcx.ins()
            .store(MemFlagsData::new(), cellw, env_word, value_word_offset(i));
    }

    let fref = c.module.declare_func_in_func(fid, bcx.func);
    let code = bcx.ins().func_addr(c.ptr_ty, fref);
    let cn = c.import_runtime("alias.closure.new")?;
    let cnref = c.module.declare_func_in_func(cn, bcx.func);
    let cncall = bcx.ins().call(cnref, &[code, env_word]);
    Ok(first_result(bcx, cncall))
}
