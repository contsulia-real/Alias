use crate::codegen::abi::{
    cl_type, norm_load, norm_store, user_signature, value_word_offset, VTy,
};
use crate::codegen::emit::cells::{emit_local_cell, first_result};
use crate::codegen::emit::control::emit_body;
use crate::codegen::emit::expr::emit_expr;
use crate::codegen::layout::{CLOSURE_CODE_OFFSET, CLOSURE_ENV_OFFSET};
use crate::codegen::runtime::runtime_contract;
use crate::codegen::{
    bound_vty, invariant_violation, native_err, Compiler, Frame, PendingFn, Slot,
};
use crate::sema::hir::{BindKind, BindingId, Body, Expr, Item, Param};
use crate::sema::types::{IntW, Ty};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::{
    types, AbiParam, Function, InstBuilder, MemFlagsData, Signature, TrapCode, UserFuncName, Value,
};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::HashMap;

impl<'m, M: Module> Compiler<'m, M> {
    pub(crate) fn user_sig_typed(&self, params: &[VTy], ret: &VTy) -> Signature {
        user_signature(self.cc, params, ret)
    }

    pub(crate) fn external_signature(
        &self,
        params: &[cranelift_codegen::ir::Type],
        ret: Option<cranelift_codegen::ir::Type>,
    ) -> Signature {
        let mut s = Signature::new(self.cc);
        for p in params {
            s.params.push(AbiParam::new(*p));
        }
        if let Some(r) = ret {
            s.returns.push(AbiParam::new(r));
        }
        s
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

    pub(crate) fn import_external(
        &mut self,
        name: &str,
        params: &[cranelift_codegen::ir::Type],
        ret: Option<cranelift_codegen::ir::Type>,
    ) -> AliasResult<FuncId> {
        if name.starts_with("alias.") || name.starts_with("rt.") {
            return Err(native_err(
                Span::default(),
                format!("内部: runtime 符号 '{name}' 必须经契约表声明"),
            ));
        }
        self.module
            .declare_function(name, Linkage::Import, &self.external_signature(params, ret))
            .map_err(|e| native_err(Span::default(), format!("内部: 符号声明失败 {e}")))
    }

    pub(crate) fn import_runtime(&mut self, name: &str) -> AliasResult<FuncId> {
        let contract = runtime_contract(name)?;
        let sig = contract.signature(self.cc, self.ptr_ty);
        self.module
            .declare_function(contract.symbol, Linkage::Import, &sig)
            .map_err(|e| native_err(Span::default(), format!("内部: runtime 声明失败 {e}")))
    }

    pub(crate) fn call_rt(
        &mut self,
        bcx: &mut FunctionBuilder,
        name: &str,
        args: &[Value],
    ) -> AliasResult<Value> {
        if runtime_contract(name)?.ret.is_none() {
            return Err(native_err(
                Span::default(),
                format!("内部: 无返回值 runtime '{name}' 被当作值调用"),
            ));
        }
        let inst = self.call_rt_inst(bcx, name, args)?;
        match bcx.inst_results(inst) {
            [value] => Ok(*value),
            _ => Err(native_err(
                Span::default(),
                format!("内部: 有返回值 runtime '{name}' 未产生唯一结果"),
            )),
        }
    }

    pub(crate) fn call_rt_void(
        &mut self,
        bcx: &mut FunctionBuilder,
        name: &str,
        args: &[Value],
    ) -> AliasResult<()> {
        if runtime_contract(name)?.ret.is_some() {
            return Err(native_err(
                Span::default(),
                format!("内部: 有返回值 runtime '{name}' 被当作 unit 调用"),
            ));
        }
        let inst = self.call_rt_inst(bcx, name, args)?;
        if !bcx.inst_results(inst).is_empty() {
            return Err(native_err(
                Span::default(),
                format!("内部: 无返回值 runtime '{name}' 意外产生结果"),
            ));
        }
        Ok(())
    }

    fn call_rt_inst(
        &mut self,
        bcx: &mut FunctionBuilder,
        name: &str,
        args: &[Value],
    ) -> AliasResult<cranelift_codegen::ir::Inst> {
        let contract = runtime_contract(name)?;
        if args.len() != contract.params.len() {
            return Err(native_err(
                Span::default(),
                format!(
                    "内部: runtime '{}' 参数数量不匹配，契约 {}，调用点 {}",
                    name,
                    contract.params.len(),
                    args.len()
                ),
            ));
        }
        for (index, (arg, expected)) in args.iter().zip(contract.params).enumerate() {
            let actual = bcx.func.dfg.value_type(*arg);
            let expected = expected.ty.resolve(self.ptr_ty);
            if actual != expected {
                return Err(native_err(
                    Span::default(),
                    format!(
                        "内部: runtime '{}' 第 {} 个参数类型不匹配，契约 {}，调用点 {}",
                        name,
                        index + 1,
                        expected,
                        actual
                    ),
                ));
            }
        }
        let fid = self.import_runtime(name)?;
        let fref = self.module.declare_func_in_func(fid, bcx.func);
        Ok(bcx.ins().call(fref, args))
    }

    pub(crate) fn define_user_func(
        &mut self,
        fid: FuncId,
        params: &[Param],
        body: &Body,
        caps: Vec<(BindingId, VTy)>,
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

        let globals_v = bcx.declare_var(types::I64);
        bcx.def_var(globals_v, bcx.block_params(entry)[0]);
        let env_v = bcx.declare_var(types::I64);
        bcx.def_var(env_v, bcx.block_params(entry)[1]);
        let mut caps_map: HashMap<BindingId, usize> = HashMap::new();
        let mut caps_vty: HashMap<BindingId, VTy> = HashMap::new();
        for (i, (id, vty)) in caps.iter().enumerate() {
            caps_map.insert(*id, i);
            caps_vty.insert(*id, vty.clone());
        }
        let mut frame = Frame {
            scopes: vec![HashMap::new()],
            locals_vty: vec![HashMap::new()],
            globals: globals_v,
            env: Some(env_v),
            caps: caps_map,
            caps_vty,
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
            let word = norm_load(&mut bcx, raw, &vty);
            emit_local_cell(self, &mut bcx, &mut frame, word, vty, p.binding_id)?;
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
            globals: globals_v,
            env: None,
            caps: HashMap::new(),
            caps_vty: HashMap::new(),
            this_fid: None,
            terminated: false,
            loop_targets: Vec::new(),
            init_ctx: false,
            ret_block: Some(abort_ret),
            ret_vty: Some(VTy::I(IntW::W32)),
        };
        frame.init_ctx = true;

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
        }

        let clo = {
            let base = bcx.use_var(frame.globals);
            bcx.ins()
                .load(types::I64, MemFlagsData::new(), base, main_slot as i32)
        };
        let code = bcx.ins().load(
            types::I64,
            MemFlagsData::new(),
            clo,
            CLOSURE_CODE_OFFSET,
        );
        let env = bcx.ins().load(
            types::I64,
            MemFlagsData::new(),
            clo,
            CLOSURE_ENV_OFFSET,
        );
        let msig = user_signature(self.cc, &[], &main_ret);
        let uref = bcx.func.import_signature(msig);
        let icall = bcx.ins().call_indirect(uref, code, &[gword, env]);
        let raw = first_result(&bcx, icall);
        let code_word = norm_load(&mut bcx, raw, &main_ret);
        let exit_code = bcx.ins().ireduce(types::I32, code_word);
        let ep = self.import_external("ExitProcess", &[types::I32], None)?;
        let epr = self.module.declare_func_in_func(ep, bcx.func);
        bcx.ins().call(epr, &[exit_code]);
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
    captures: &[BindingId],
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
    captures: &[BindingId],
    ret_vty: VTy,
) -> AliasResult<Value> {
    let param_vtys: Vec<VTy> = params.iter().map(|p| c.vty(&p.ty)).collect();
    let name = format!("u{}", c.next_fid);
    c.next_fid += 1;
    let fid = c.declare_user_func_typed(&param_vtys, &ret_vty, name)?;
    c.fn_ids.push(fid);
    c.fn_rets.push(ret_vty.clone());
    let cap_vtys: Vec<(BindingId, VTy)> = captures
        .iter()
        .map(|id| (*id, bound_vty(c, frame, *id)))
        .collect();
    c.pending.push_back(PendingFn {
        fid,
        params: params.to_vec(),
        body: body.clone(),
        caps: cap_vtys,
        ret_vty,
    });

    let env_word = if captures.is_empty() {
        bcx.ins().iconst(types::I64, 0)
    } else {
        let en = c.import_runtime("alias.env.new")?;
        let eref = c.module.declare_func_in_func(en, bcx.func);
        let len = bcx.ins().iconst(types::I32, captures.len() as i64);
        let ecall = bcx.ins().call(eref, &[len]);
        first_result(bcx, ecall)
    };
    for (i, id) in captures.iter().enumerate() {
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
