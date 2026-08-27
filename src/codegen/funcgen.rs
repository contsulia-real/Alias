use super::*;
// ---------------------------------------------------------------------------
// 编译器
// ---------------------------------------------------------------------------

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
        let fref = self.module.declare_func_in_func(fid, &mut bcx.func);
        let inst = bcx.ins().call(fref, args);
        Ok(match bcx.inst_results(inst) {
            [v] => *v,
            [] => bcx.ins().iconst(types::I64, 0),
            _ => invariant_violation("运行时单返回值签名"),
        })
    }

    pub(crate) fn define_user_func(
        &mut self,
        fid: FuncId,
        params: &[Param],
        body: &Body,
        caps: Vec<(String, VTy)>,
        ret_vty: VTy,
    ) -> AliasResult<()> {
        let param_vtys: Vec<VTy> = params
            .iter()
            .map(|p| decl_vty(&p.ty, &self.struct_layouts))
            .collect();
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
        let mut caps_map: HashMap<String, usize> = HashMap::new();
        let mut caps_vty: HashMap<String, VTy> = HashMap::new();
        for (i, (name, vty)) in caps.iter().enumerate() {
            caps_map.insert(name.clone(), i);
            caps_vty.insert(name.clone(), vty.clone());
        }
        let mut frame = Frame {
            scopes: vec![HashMap::new()],
            locals_vty: vec![HashMap::new()],
            globals: globals_v,
            env: Some(env_v),
            caps: caps_map,
            caps_vty,
            this_fid: Some(fid),
            this_vty: Some(VTy::Func(param_vtys.clone(), Box::new(ret_vty.clone()))),
            terminated: false,
            loop_targets: Vec::new(),
            init_ctx: false,
            ret_block: None,
            ret_vty: Some(ret_vty.clone()),
        };

        for (i, p) in params.iter().enumerate() {
            let raw = bcx.block_params(entry)[i + 2];
            let vty = decl_vty(&p.ty, &self.struct_layouts);
            let word = norm_load(&mut bcx, raw, &vty);
            emit_local_cell(self, &mut bcx, &mut frame, word, vty, &p.name)?;
        }

        let ret_block = bcx.create_block();
        frame.ret_block = Some(ret_block);
        let ret_val = bcx.append_block_param(ret_block, cl_type(&ret_vty));
        emit_body(self, &mut bcx, &mut frame, body, ret_block)?;
        if !frame.terminated {
            let zero = if cl_type(&ret_vty) == types::F32 {
                bcx.ins().f32const(0.0)
            } else if cl_type(&ret_vty) == types::F64 {
                bcx.ins().f64const(0.0)
            } else {
                bcx.ins().iconst(cl_type(&ret_vty), 0)
            };
            bcx.ins().jump(ret_block, &[BlockArg::Value(zero)]);
        }
        bcx.switch_to_block(ret_block);
        bcx.seal_block(ret_block);
        bcx.ins().return_(&[ret_val]);
        bcx.finalize(self.module.target_config());
        if let Err(ve) = ctx.verify_if(self.module.isa()) {
            eprintln!("[内部验证失败] {}", ve);
            eprintln!("[Cranelift 中间表示]\n{}", ctx.func);
        }
        self.module
            .define_function(fid, &mut ctx)
            .map_err(|e| native_err(Span::default(), format!("内部: 函数定义失败 {e}")))
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
            this_vty: None,
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
                Item::Binding(b) if b.receiver.is_none() => Some(b),
                _ => None,
            })
            .enumerate()
        {
            let (v, svty) = if b.kind == BindKind::Func {
                let Expr::FuncLit { params, body, .. } = &b.value else {
                    return Err(native_err(b.span, "函数绑定必须由函数字面量初始化"));
                };
                let ret_vty = decl_vty(&b.ty, &self.struct_layouts);
                let param_vtys = params
                    .iter()
                    .map(|p| decl_vty(&p.ty, &self.struct_layouts))
                    .collect::<Vec<_>>();
                let v = emit_funclit_value_typed(
                    self,
                    &mut bcx,
                    &mut frame,
                    params,
                    body,
                    ret_vty.clone(),
                )?;
                (v, VTy::Func(param_vtys, Box::new(ret_vty)))
            } else {
                let vty = decl_vty(&b.ty, &self.struct_layouts);
                (
                    emit_expr_expected(self, &mut bcx, &mut frame, &b.value, &vty)?,
                    vty,
                )
            };
            let off = self.top_slots[binding_index];
            let sv = norm_store(&mut bcx, v, &svty);
            let base = bcx.use_var(frame.globals);
            bcx.ins().store(MemFlagsData::new(), sv, base, off as i32);
            frame.scopes[0].insert(b.name.clone(), Slot::Global(off));
            frame.locals_vty[0].insert(b.name.clone(), svty);
        }

        let clo = {
            let base = bcx.use_var(frame.globals);
            bcx.ins()
                .load(types::I64, MemFlagsData::new(), base, main_slot as i32)
        };
        let code = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 0);
        let env = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 8);
        let msig = user_signature(self.cc, &[], &main_ret);
        let uref = bcx.func.import_signature(msig);
        let icall = bcx.ins().call_indirect(uref, code, &[gword, env]);
        let raw = first_result(&bcx, icall);
        let code_word = norm_load(&mut bcx, raw, &main_ret);
        let exit_code = bcx.ins().ireduce(types::I32, code_word);
        let ep = self.import_external("ExitProcess", &[types::I32], None)?;
        let epr = self.module.declare_func_in_func(ep, &mut bcx.func);
        bcx.ins().call(epr, &[exit_code]);
        bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);

        bcx.switch_to_block(abort_ret);
        bcx.seal_block(abort_ret);
        let ep = self.import_external("ExitProcess", &[types::I32], None)?;
        let epr = self.module.declare_func_in_func(ep, &mut bcx.func);
        let one = bcx.ins().iconst(types::I32, 1);
        bcx.ins().call(epr, &[one]);
        bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);
        bcx.finalize(self.module.target_config());
        if let Err(ve) = ctx.verify_if(self.module.isa()) {
            eprintln!("[内部验证失败] {}", ve);
            eprintln!("[Cranelift 中间表示]\n{}", ctx.func);
        }
        self.module
            .define_function(fid, &mut ctx)
            .map_err(|e| native_err(Span::default(), format!("内部: 入口定义失败 {e}")))?;
        Ok(fid)
    }
}

// ---------------------------------------------------------------------------
// 函数字面量: 捕获扫描 + 闭包对象创建
// ---------------------------------------------------------------------------

pub(crate) fn emit_funclit_value<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    params: &[Param],
    body: &Body,
) -> AliasResult<Value> {
    let ret_vty = infer_ret_vty(c, frame, params, body);
    emit_funclit_value_typed(c, bcx, frame, params, body, ret_vty)
}

pub(crate) fn emit_funclit_value_typed<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    params: &[Param],
    body: &Body,
    ret_vty: VTy,
) -> AliasResult<Value> {
    let caps = scan_captures(c, params, body, frame);
    let param_vtys: Vec<VTy> = params
        .iter()
        .map(|p| decl_vty(&p.ty, &c.struct_layouts))
        .collect();
    let name = format!("u{}", c.next_fid);
    c.next_fid += 1;
    let fid = c.declare_user_func_typed(&param_vtys, &ret_vty, name)?;
    c.fn_ids.push(fid);
    c.fn_rets.push(ret_vty.clone());
    let cap_vtys: Vec<(String, VTy)> = caps
        .iter()
        .map(|n| (n.clone(), vty_of_name(c, frame, n)))
        .collect();
    c.pending.push_back(PendingFn {
        fid,
        params: params.to_vec(),
        body: body.clone(),
        caps: cap_vtys,
        ret_vty,
    });

    let env_word = if caps.is_empty() {
        bcx.ins().iconst(types::I64, 0)
    } else {
        let en = c.import_runtime("alias.env.new")?;
        let eref = c.module.declare_func_in_func(en, &mut bcx.func);
        let len = bcx.ins().iconst(types::I32, caps.len() as i64);
        let ecall = bcx.ins().call(eref, &[len]);
        first_result(bcx, ecall)
    };
    for (i, name) in caps.iter().enumerate() {
        let cellw = if let Some(idx) = frame.caps.get(name) {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            bcx.ins().load(
                types::I64,
                MemFlagsData::new(),
                base,
                ((*idx as i64) * 8) as i32,
            )
        } else {
            let mut found: Option<Value> = None;
            for scope in frame.scopes.iter().rev() {
                if let Some(Slot::Local(v)) = scope.get(name) {
                    found = Some(bcx.use_var(*v));
                    break;
                }
            }
            found.unwrap_or_else(|| invariant_violation("捕获项解析"))
        };
        bcx.ins().store(
            MemFlagsData::new(),
            cellw,
            env_word,
            ((i as i64) * 8) as i32,
        );
    }

    let fref = c.module.declare_func_in_func(fid, &mut bcx.func);
    let code = bcx.ins().func_addr(c.ptr_ty, fref);
    let cn = c.import_runtime("alias.closure.new")?;
    let cnref = c.module.declare_func_in_func(cn, &mut bcx.func);
    let cncall = bcx.ins().call(cnref, &[code, env_word]);
    Ok(first_result(bcx, cncall))
}

pub(crate) fn scan_captures<M: Module>(
    c: &Compiler<M>,
    params: &[Param],
    body: &Body,
    frame: &Frame,
) -> Vec<String> {
    let mut locals: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
    let mut caps: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    scan_body(c, body, &mut locals, &mut caps, &mut seen, frame);
    caps
}

pub(crate) fn scan_body<M: Module>(
    c: &Compiler<M>,
    body: &Body,
    locals: &mut HashSet<String>,
    caps: &mut Vec<String>,
    seen: &mut HashSet<String>,
    frame: &Frame,
) {
    match body {
        Body::Single(stmt) => scan_stmt(c, stmt, locals, caps, seen, frame),
        Body::Block(stmts) => {
            for s in stmts {
                scan_stmt(c, s, locals, caps, seen, frame);
            }
        }
    }
}

pub(crate) fn record_cap(name: &str, caps: &mut Vec<String>, seen: &mut HashSet<String>) {
    if seen.insert(name.to_string()) {
        caps.push(name.to_string());
    }
}

fn scan_scoped_stmts<M: Module>(
    c: &Compiler<M>,
    stmts: &[Stmt],
    locals: &HashSet<String>,
    caps: &mut Vec<String>,
    seen: &mut HashSet<String>,
    frame: &Frame,
) {
    let mut child_locals = locals.clone();
    for s in stmts {
        scan_stmt(c, s, &mut child_locals, caps, seen, frame);
    }
}

pub(crate) fn scan_stmt<M: Module>(
    c: &Compiler<M>,
    s: &Stmt,
    locals: &mut HashSet<String>,
    caps: &mut Vec<String>,
    seen: &mut HashSet<String>,
    frame: &Frame,
) {
    match s {
        Stmt::Binding(b) => {
            scan_expr(c, &b.value, locals, caps, seen, frame);
            locals.insert(b.name.clone());
        }
        Stmt::Assign { target, value, .. } => {
            scan_expr(c, value, locals, caps, seen, frame);
            ensure_scanned_name(target, locals, caps, seen, frame);
        }
        Stmt::FieldAssign { recv, value, .. } => {
            scan_expr(c, value, locals, caps, seen, frame);
            scan_expr(c, recv, locals, caps, seen, frame);
        }
        Stmt::ExprStmt { expr, .. } => scan_expr(c, expr, locals, caps, seen, frame),
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                scan_expr(c, e, locals, caps, seen, frame);
            }
        }
        Stmt::If {
            branches,
            else_body,
            ..
        } => {
            for (cond, body) in branches {
                scan_expr(c, cond, locals, caps, seen, frame);
                scan_scoped_stmts(c, body, locals, caps, seen, frame);
            }
            if let Some(body) = else_body {
                scan_scoped_stmts(c, body, locals, caps, seen, frame);
            }
        }
        Stmt::While { cond, body, .. } => {
            scan_expr(c, cond, locals, caps, seen, frame);
            scan_scoped_stmts(c, body, locals, caps, seen, frame);
        }
        Stmt::For {
            name,
            iterable,
            body,
            ..
        } => {
            scan_expr(c, iterable, locals, caps, seen, frame);
            let mut child_locals = locals.clone();
            child_locals.insert(name.clone());
            for stmt in body {
                scan_stmt(c, stmt, &mut child_locals, caps, seen, frame);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
    }
}

pub(crate) fn ensure_scanned_name(
    name: &str,
    locals: &mut HashSet<String>,
    caps: &mut Vec<String>,
    seen: &mut HashSet<String>,
    frame: &Frame,
) {
    if locals.contains(name) || seen.contains(name) {
        return;
    }
    let outer_local = frame.caps.contains_key(name)
        || frame
            .scopes
            .iter()
            .rev()
            .any(|sc| matches!(sc.get(name), Some(Slot::Local(_))));
    if outer_local {
        record_cap(name, caps, seen);
    }
}

pub(crate) fn scan_expr<M: Module>(
    c: &Compiler<M>,
    e: &Expr,
    locals: &mut HashSet<String>,
    caps: &mut Vec<String>,
    seen: &mut HashSet<String>,
    frame: &Frame,
) {
    match e {
        Expr::Ident(name, _) => ensure_scanned_name(name, locals, caps, seen, frame),
        Expr::This(_) => {}
        Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Cast { expr, .. } => scan_expr(c, expr, locals, caps, seen, frame),
        Expr::Binary { lhs, rhs, .. } => {
            scan_expr(c, lhs, locals, caps, seen, frame);
            scan_expr(c, rhs, locals, caps, seen, frame);
        }
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            scan_expr(c, cond, locals, caps, seen, frame);
            scan_expr(c, then_expr, locals, caps, seen, frame);
            scan_expr(c, else_expr, locals, caps, seen, frame);
        }
        Expr::Str(parts, _) => {
            for p in parts {
                if let StrPartAst::Hole(h) = p {
                    scan_expr(c, h, locals, caps, seen, frame);
                }
            }
        }
        Expr::Call { callee, args, .. } => {
            scan_expr(c, callee, locals, caps, seen, frame);
            for a in args {
                scan_expr(c, &a.value, locals, caps, seen, frame);
            }
        }
        Expr::Field { recv, .. } => scan_expr(c, recv, locals, caps, seen, frame),
        Expr::MethodCall { recv, args, .. } => {
            scan_expr(c, recv, locals, caps, seen, frame);
            for a in args {
                scan_expr(c, &a.value, locals, caps, seen, frame);
            }
        }
        Expr::Index { recv, idx, .. } => {
            scan_expr(c, recv, locals, caps, seen, frame);
            scan_expr(c, idx, locals, caps, seen, frame);
        }
        Expr::ArrayLit { elems, .. } => {
            for el in elems {
                scan_expr(c, el, locals, caps, seen, frame);
            }
        }
        Expr::Propagate { expr, .. } => scan_expr(c, expr, locals, caps, seen, frame),
        Expr::Match { subject, arms, .. } => {
            scan_expr(c, subject, locals, caps, seen, frame);
            for arm in arms {
                let mut arm_locals = locals.clone();
                match &arm.pattern {
                    Pattern::Binding { name, .. }
                    | Pattern::Constructor {
                        binding: Some(name),
                        ..
                    } => {
                        arm_locals.insert(name.clone());
                    }
                    _ => {}
                }
                match &arm.body {
                    ArmBody::Block(stmts) => {
                        for s in stmts {
                            scan_stmt(c, s, &mut arm_locals, caps, seen, frame);
                        }
                    }
                    ArmBody::Value(e) | ArmBody::Ret(e) => {
                        scan_expr(c, e, &mut arm_locals, caps, seen, frame)
                    }
                }
            }
        }
        Expr::FuncLit { params, body, .. } => {
            let mut nested_locals = locals.clone();
            for p in params {
                nested_locals.insert(p.name.clone());
            }
            scan_body(c, body, &mut nested_locals, caps, seen, frame);
        }
        Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::Unit(_) => {}
    }
}
