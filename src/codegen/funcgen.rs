use super::*;
// ---------------------------------------------------------------------------
// 编译器
// ---------------------------------------------------------------------------

impl<'m, M: Module> Compiler<'m, M> {
    /// 统一调用约定 (多态退化路径): (globals:I64, env:I64, args:I64...) -> I64。
    pub(crate) fn user_sig(&self, n_args: usize) -> Signature {
        let mut s = Signature::new(self.cc);
        s.params.push(AbiParam::new(types::I64));
        s.params.push(AbiParam::new(types::I64));
        for _ in 0..n_args {
            s.params.push(AbiParam::new(types::I64));
        }
        s.returns.push(AbiParam::new(types::I64));
        s
    }

    /// 混合签名 (Phase 3a): (globals:I64, env:I64, params...) -> ret —
    /// 参数/返回按静态类型逐位定型; 浮点经 ABI 寄存器传递 (win64 XMM)。
    /// Unit 函数统一携带 I64 哑字返回 (cl_type(Unit)=I64), 简化调用点。
    pub(crate) fn user_sig_typed(&self, params: &[VTy], ret: &VTy) -> Signature {
        let mut s = Signature::new(self.cc);
        s.params.push(AbiParam::new(types::I64));
        s.params.push(AbiParam::new(types::I64));
        for p in params {
            s.params.push(AbiParam::new(cl_type(p)));
        }
        s.returns.push(AbiParam::new(cl_type(ret)));
        s
    }

    /// 运行时符号签名 (JIT 宿主与 AOT shim 同契约)
    pub(crate) fn sig(&self, params: &[cranelift_codegen::ir::Type], ret: Option<cranelift_codegen::ir::Type>) -> Signature {
        let mut s = Signature::new(self.cc);
        for p in params {
            s.params.push(AbiParam::new(*p));
        }
        if let Some(r) = ret {
            s.returns.push(AbiParam::new(r));
        }
        s
    }

    /// 具名混合签名内部函数声明 (用户函数与方法共用)
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

    pub(crate) fn import_fn(
        &mut self,
        name: &str,
        params: &[cranelift_codegen::ir::Type],
        ret: Option<cranelift_codegen::ir::Type>,
    ) -> AliasResult<FuncId> {
        self.module
            .declare_function(name, Linkage::Import, &self.sig(params, ret))
            .map_err(|e| native_err(Span::default(), format!("内部: 符号声明失败 {e}")))
    }

    /// 调外部/运行时符号的单返回值调用辅助 (JIT 宿主符号与 AOT shim 同名)
    pub(crate) fn call_rt(
        &mut self,
        bcx: &mut FunctionBuilder,
        name: &str,
        params: &[cranelift_codegen::ir::Type],
        ret: Option<cranelift_codegen::ir::Type>,
        args: &[Value],
    ) -> AliasResult<Value> {
        let fid = self.import_fn(name, params, ret)?;
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
        let param_vtys: Vec<VTy> =
            params.iter().map(|p| decl_vty(&p.ty, &self.struct_layouts)).collect();
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
            terminated: false,
            init_ctx: false,
            ret_block: None,
            ret_vty: Some(ret_vty.clone()),
        };

        // 参数 → 新鲜单元格 (引用语义; 参数入局部作用域, spec-notes §附录二)]
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
            // 落空回退: 仅 unit 函数可达 (非 unit 落空已被 Q③ 严格版在 sema 拒绝) → 哑值 0
            let zero = bcx.ins().iconst(cl_type(&ret_vty), 0);
            bcx.ins().jump(ret_block, &[BlockArg::Value(zero)]);
        }
        bcx.switch_to_block(ret_block);
        bcx.seal_block(ret_block);
        bcx.ins().return_(&[ret_val]);
        bcx.finalize(self.module.target_config());
        if let Err(ve) = ctx.verify_if(self.module.isa()) {
            eprintln!("[verify-fail] {}", ve);
            eprintln!("[CLIF]\n{}", ctx.func);
        }
        self.module
            .define_function(fid, &mut ctx)
            .map_err(|e| native_err(Span::default(), format!("内部: 函数定义失败 {e}")))
    }

    /// 入口 wrapper: Q⑥ 顶层初始化按序求值 (insert-after-eval 可见性),
    /// 随后间接调用 main 闭包并按声明返回类型映射退出码
    /// (i32→原样 / bool true→0 false→1 / 其余→0, spec-notes Q④ 映射表)。
    /// JIT: 名 alias_entry 返回 I64 由宿主读取;
    /// AOT: 导出 alias_start — 无 CRT 环境, 显式 ExitProcess 传递退出码,
    /// 链接参数 /ENTRY:alias_start (linker.rs 单一拥有者)。
    pub(crate) fn compile_entry(&mut self, items: &[Item], main_slot: usize, main_ret: VTy) -> AliasResult<FuncId> {
        let mut entry_sig = Signature::new(self.cc);
        entry_sig.returns.push(AbiParam::new(if self.is_aot { types::I32 } else { types::I64 }));
        let fid = self
            .module
            .declare_function(
                if self.is_aot { "alias_start" } else { "alias_entry" },
                if self.is_aot { Linkage::Export } else { Linkage::Local },
                &entry_sig,
            )
            .map_err(|e| native_err(Span::default(), format!("内部: 入口声明失败 {e}")))?;
        let mut ctx = Context::new();
        ctx.func = Function::with_name_signature(UserFuncName::user(0, fid.as_u32()), entry_sig);
        let mut fbctx = FunctionBuilderContext::new();
        let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fbctx);

        let entry = bcx.create_block();
        bcx.switch_to_block(entry);
        bcx.seal_block(entry);

        let byte_count = bcx.ins().iconst(types::I64, self.global_bytes as i64);
        let gword = self.call_rt(
            &mut bcx,
            "alias.globals.new",
            &[types::I64],
            Some(types::I64),
            &[byte_count],
        )?;

        let globals_v = bcx.declare_var(types::I64);
        bcx.def_var(globals_v, gword);
        let mut frame = Frame {
            scopes: vec![HashMap::new()],
            locals_vty: vec![HashMap::new()],
            globals: globals_v,
            env: None,
            caps: HashMap::new(),
            caps_vty: HashMap::new(),
            terminated: false,
            init_ctx: false,
            ret_block: None,
            ret_vty: None,
        };
        // 顶层槽位偏移在编译期已知 → 记录供 slot_of 解析 (init 语境)
        frame.init_ctx = true;

        // 名字随项序增长可见 — 镜像解释器逐项插入 (insert-after-eval);
        // 方法不是绑定, 不参与初始化序列
        for b in items.iter().filter_map(|i| match i {
            Item::Binding(b) if b.receiver.is_none() => Some(b),
            _ => None,
        }) {
            let v = emit_expr(self, &mut bcx, &mut frame, &b.value)?;
            let off = slot_of(self, &b.name);
            let svty = self.globals_final[&b.name].1.clone();
            let sv = norm_store(&mut bcx, v, &svty);
            let base = bcx.use_var(frame.globals);
            bcx.ins().store(MemFlagsData::new(), sv, base, off as i32);
            frame.scopes[0].insert(b.name.clone(), Slot::Global(off));
            frame.locals_vty[0].insert(b.name.clone(), decl_vty(&b.ty, &self.struct_layouts));
        }

        // main 闭包: 从全局槽位加载 → 间接调用 (混合签名 — 返回类型定型)
        let clo = {
            let base = bcx.use_var(frame.globals);
            bcx.ins().load(types::I64, MemFlagsData::new(), base, main_slot as i32)
        };
        let code = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 0);
        let env = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 8);
        let mut msig = Signature::new(self.cc);
        msig.params.push(AbiParam::new(types::I64));
        msig.params.push(AbiParam::new(types::I64));
        msig.returns.push(AbiParam::new(cl_type(&main_ret)));
        let uref = bcx.func.import_signature(msig);
        let icall = bcx.ins().call_indirect(uref, code, &[gword, env]);
        let raw = first_result(&bcx, icall);
        // 退出映射 (Q④): i32→原样 / bool true→0 false→1 / string·unit→0
        let code_word: Value = match &main_ret {
            VTy::Bool => {
                let is_true = bcx.ins().icmp_imm_s(IntCC::Equal, raw, 1);
                let t = bcx.ins().iconst(types::I64, 0);
                let f = bcx.ins().iconst(types::I64, 1);
                bcx.ins().select(is_true, t, f)
            }
            VTy::Str | VTy::Unit => bcx.ins().iconst(types::I64, 0),
            _ => norm_load(&mut bcx, raw, &main_ret),
        };
        if self.is_aot {
            // 无 CRT 环境: 显式 ExitProcess 传递退出码 (返回值无人接收)
            let exit_code = bcx.ins().ireduce(types::I32, code_word);
            let ep = self.import_fn("ExitProcess", &[types::I32], None)?;
            let epr = self.module.declare_func_in_func(ep, &mut bcx.func);
            bcx.ins().call(epr, &[exit_code]);
            bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO); // 不可达兜底
        } else {
            bcx.ins().return_(&[code_word]);
        }
        bcx.finalize(self.module.target_config());
        if let Err(ve) = ctx.verify_if(self.module.isa()) {
            eprintln!("[verify-fail] {}", ve);
            eprintln!("[CLIF]\n{}", ctx.func);
        }
        self.module
            .define_function(fid, &mut ctx)
            .map_err(|e| native_err(Span::default(), format!("内部: 入口定义失败 {e}")))?;
        Ok(fid)
    }
}
pub(crate) fn slot_of<M: Module>(c: &Compiler<M>, name: &str) -> usize {
    c.globals_final[name].0
}

// ---------------------------------------------------------------------------
// 函数字面量: 捕获扫描 + 闭包对象创建
// ---------------------------------------------------------------------------

/// 创建闭包值: 扫描捕获 → 声明并排队函数体 → 组装 env 数组与闭包对象。
pub(crate) fn emit_funclit_value<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    params: &[Param],
    body: &Body,
) -> AliasResult<Value> {
    let caps = scan_captures(c, params, body, frame);
    let ret_vty = infer_ret_vty(c, frame, params, body);
    let param_vtys: Vec<VTy> = params.iter().map(|p| decl_vty(&p.ty, &c.struct_layouts)).collect();
    let name = format!("u{}", c.next_fid);
    c.next_fid += 1;
    let fid = c.declare_user_func_typed(&param_vtys, &ret_vty, name)?;
    c.fn_ids.push(fid);
    c.fn_rets.push(ret_vty.clone());
    // 捕获项静态类型在捕获帧就地解析 — 闭包体内不可再回查外层帧
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

    // env 数组: 捕获单元格指针按序排列 (引用捕获 — 最新值双向可见)
    let env_word = if caps.is_empty() {
        bcx.ins().iconst(types::I64, 0)
    } else {
        let en = c.import_fn("alias.env.new", &[types::I32], Some(types::I64))?;
        let eref = c.module.declare_func_in_func(en, &mut bcx.func);
        let len = bcx.ins().iconst(types::I32, caps.len() as i64);
        let ecall = bcx.ins().call(eref, &[len]);
        first_result(bcx, ecall)
    };
    for (i, name) in caps.iter().enumerate() {
        // 捕获项必可解析为本帧局部或本帧捕获 (扫描保证)
        let cellw = if let Some(idx) = frame.caps.get(name) {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            bcx.ins().load(types::I64, MemFlagsData::new(), base, ((*idx as i64) * 8) as i32)
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
        bcx.ins().store(MemFlagsData::new(), cellw, env_word, ((i as i64) * 8) as i32);
    }

    // 直取函数地址 (func_addr) — JIT/Object 双后端通用, 免运行时指针表
    let fref = c.module.declare_func_in_func(fid, &mut bcx.func);
    let code = bcx.ins().func_addr(c.ptr_ty, fref);
    let cn = c.import_fn("alias.closure.new", &[types::I64, types::I64], Some(types::I64))?;
    let cnref = c.module.declare_func_in_func(cn, &mut bcx.func);
    let cncall = bcx.ins().call(cnref, &[code, env_word]);
    Ok(first_result(bcx, cncall))
}

/// 捕获扫描 (扁平顺序遍历): 对字面量体内每个名字, 若非本函数局部
/// 且可经外层链解析 → 记入捕获表。嵌套字面量的名字同样记入 (传递捕获),
/// 其自身捕获在其发射时按本帧捕获表再解析。insert-after-eval:
/// 绑定名于初始化器求值后方才生效。
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
        Body::ArrowExpr(e) => scan_expr(c, e, locals, caps, seen, frame),
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
        // 字段赋值: 值与 recv 内的名字同样需要捕获扫描 (Phase 2a)
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
        Stmt::For { cond, body, .. } | Stmt::While { cond, body, .. } => {
            scan_expr(c, cond, locals, caps, seen, frame);
            scan_body(c, &Body::Block(body.clone()), locals, caps, seen, frame);
        }
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
    // 仅外层函数局部需要捕获; 顶层槽位经 globals 参数可达, 不捕获
    let outer_local = frame
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
        Expr::Neg { expr, .. } => scan_expr(c, expr, locals, caps, seen, frame),
        Expr::Binary { lhs, rhs, .. } => {
            scan_expr(c, lhs, locals, caps, seen, frame);
            scan_expr(c, rhs, locals, caps, seen, frame);
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
        // 字段链的 recv 可能携带外层绑定 (闭包内 s.field) — 必须扫描
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
        // 数组字面量元素可携带外层绑定 (闭包内 [a, b]) — 必须扫描
        Expr::ArrayLit { elems, .. } => {
            for el in elems {
                scan_expr(c, el, locals, caps, seen, frame);
            }
        }
        Expr::Propagate { expr, .. } => scan_expr(c, expr, locals, caps, seen, frame),
        Expr::Match { subject, arms, .. } => {
            scan_expr(c, subject, locals, caps, seen, frame);
            for arm in arms {
                // 臂绑定是本层局部 — 遮蔽后再扫描臂体 (FuncLit 参数同规约)
                let binding = arm.binding.clone();
                locals.insert(binding.clone());
                match &arm.body {
                    ArmBody::Block(stmts) => {
                        for s in stmts {
                            scan_stmt(c, s, locals, caps, seen, frame);
                        }
                    }
                    ArmBody::Value(e) | ArmBody::Ret(e) => {
                        scan_expr(c, e, locals, caps, seen, frame)
                    }
                }
                locals.remove(&binding);
            }
        }
        Expr::FuncLit { params, body, .. } => {
            // 嵌套字面量: 其参数遮蔽本层; 子树名字按本层扁平处理 (传递捕获)
            let saved: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            for p in &saved {
                locals.insert(p.clone());
            }
            scan_body(c, body, locals, caps, seen, frame);
            for p in &saved {
                locals.remove(p);
            }
        }
        _ => {}
    }
}
