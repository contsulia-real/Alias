// ---------------------------------------------------------------------------
// 单元格与全局槽位访问 — 一切在途值均为 64 位字
// ---------------------------------------------------------------------------

/// 名字 → 单元格位置。解析顺序: 词法作用域链 → 本函数捕获表 (env 加载)
/// → 顶层槽位。捕获表命中返回 env 派生地址 (引用捕获: 读写穿透到定义帧)。
enum CellAddr {
    Reg(Variable),
    EnvLoad(usize),
    GlobalOff(usize),
}

fn cell_addr<M: Module>(c: &Compiler<M>, frame: &Frame, name: &str) -> Option<CellAddr> {
    for scope in frame.scopes.iter().rev() {
        if let Some(s) = scope.get(name) {
            return Some(match s {
                Slot::Local(v) => CellAddr::Reg(*v),
                Slot::Global(off) => CellAddr::GlobalOff(*off),
            });
        }
    }
    if let Some(idx) = frame.caps.get(name) {
        return Some(CellAddr::EnvLoad(*idx));
    }
    if frame.init_ctx {
        return None; // 初始化器只见表内已插入项 — 前向引用按未定义处理
    }
    c.globals_final.get(name).map(|(off, _)| CellAddr::GlobalOff(*off))
}

fn read_cell(bcx: &mut FunctionBuilder, frame: &Frame, addr: &CellAddr) -> Value {
    match addr {
        CellAddr::Reg(v) => {
            let cp = bcx.use_var(*v);
            bcx.ins().load(types::I64, MemFlagsData::new(), cp, 0)
        }
        CellAddr::EnvLoad(i) => {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            let cell = bcx.ins().load(types::I64, MemFlagsData::new(), base, ((*i as i64) * 8) as i32);
            bcx.ins().load(types::I64, MemFlagsData::new(), cell, 0)
        }
        CellAddr::GlobalOff(off) => read_global(bcx, frame, *off),
    }
}

fn write_cell(bcx: &mut FunctionBuilder, frame: &Frame, addr: &CellAddr, w: Value) {
    match addr {
        CellAddr::Reg(v) => {
            let cp = bcx.use_var(*v);
            bcx.ins().store(MemFlagsData::new(), w, cp, 0);
        }
        CellAddr::EnvLoad(i) => {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            let cell = bcx.ins().load(types::I64, MemFlagsData::new(), base, ((*i as i64) * 8) as i32);
            bcx.ins().store(MemFlagsData::new(), w, cell, 0);
        }
        CellAddr::GlobalOff(off) => write_global(bcx, frame, *off, w),
    }
}

fn read_global(bcx: &mut FunctionBuilder, frame: &Frame, off: usize) -> Value {
    let base = bcx.use_var(frame.globals);
    bcx.ins().load(types::I64, MemFlagsData::new(), base, ((off as i64) * 8) as i32)
}

fn write_global(bcx: &mut FunctionBuilder, frame: &Frame, off: usize, w: Value) {
    let base = bcx.use_var(frame.globals);
    bcx.ins().store(MemFlagsData::new(), w, base, ((off as i64) * 8) as i32);
}

/// 绑定 → 新鲜单元格 + 登记 SSA 变量与静态类型。
fn emit_local_cell<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    word: Value,
    vty: VTy,
    name: &str,
) -> AliasResult<Variable> {
    let an = c.import_fn("alias.cell.new", &[types::I64], Some(types::I64))?;
    let aref = c.module.declare_func_in_func(an, &mut bcx.func);
    let ccall = bcx.ins().call(aref, &[word]);
    let cell = first_result(bcx, ccall);
    let var = bcx.declare_var(types::I64);
    bcx.def_var(var, cell);
    let scope = frame.scopes.last_mut().unwrap_or_else(|| invariant_violation("作用域栈非空"));
    scope.insert(name.to_string(), Slot::Local(var));
    frame.locals_vty.last_mut().unwrap_or_else(|| invariant_violation("作用域栈非空"))
        .insert(name.to_string(), vty);
    Ok(var)
}

fn first_result(bcx: &FunctionBuilder, inst: cranelift_codegen::ir::Inst) -> Value {
    match bcx.inst_results(inst) {
        [v] => *v,
        _ => invariant_violation("单返回值签名"),
    }
}

fn ensure_current(bcx: &mut FunctionBuilder, frame: &mut Frame) {
    if frame.terminated {
        let dead = bcx.create_block();
        bcx.switch_to_block(dead);
        bcx.seal_block(dead);
        frame.terminated = false;
    }
}

// ---------------------------------------------------------------------------
// 体发射
// ---------------------------------------------------------------------------

fn emit_body<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    body: &Body,
    ret_block: Block,
) -> AliasResult<()> {
    match body {
        Body::ArrowExpr(e) => {
            let v = emit_expr(c, bcx, frame, e)?;
            bcx.ins().jump(ret_block, &[BlockArg::Value(v)]);
            frame.terminated = true;
        }
        Body::Block(stmts) => {
            for s in stmts {
                ensure_current(bcx, frame);
                emit_stmt(c, bcx, frame, s, ret_block)?;
            }
        }
    }
    Ok(())
}

fn emit_stmt<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    s: &Stmt,
    ret_block: Block,
) -> AliasResult<()> {
    match s {
        Stmt::Binding(b) => {
            if b.kind == BindKind::Func {
                c.fn_ret_by_name.insert(b.name.clone(), decl_vty(&b.ty, &c.struct_layouts));
            }
            // insert-after-eval: 初始化器先求值, 名字后可见 (Q⑥ 顺序语义)
            let v = emit_expr(c, bcx, frame, &b.value)?;
            if b.kind == BindKind::Func {
                // func 绑定: 值即闭包对象, 存入局部单元格
                emit_local_cell(c, bcx, frame, v, VTy::Func, &b.name)?;
            } else {
                emit_local_cell(c, bcx, frame, v, decl_vty(&b.ty, &c.struct_layouts), &b.name)?;
            }
            Ok(())
        }
        Stmt::FieldAssign { recv, field, value, .. } => {
            // 先值后目标 — 与简名赋值同序 (黄金记录冻结的求值顺序)
            let v = emit_expr(c, bcx, frame, value)?;
            let p = emit_expr(c, bcx, frame, recv)?;
            let off = field_offset(c, frame, recv, field)?;
            bcx.ins().store(MemFlagsData::new(), v, p, off);
            Ok(())
        }
        Stmt::Assign { target, value, .. } => {
            // 先值后目标 — 黄金记录冻结的求值顺序
            let v = emit_expr(c, bcx, frame, value)?;
            match cell_addr(c, frame, target) {
                Some(addr) => {
                    write_cell(bcx, frame, &addr, v);
                    Ok(())
                }
                None => Err(native_err(
                    Span::default(),
                    format!("赋值目标 '{target}' 未定义"),
                )),
            }
        }
        Stmt::ExprStmt { expr, .. } => {
            emit_expr(c, bcx, frame, expr)?;
            Ok(())
        }
        Stmt::Return { value, .. } => {
            let v = match value {
                Some(e) => emit_expr(c, bcx, frame, e)?,
                None => bcx.ins().iconst(types::I64, 0),
            };
            bcx.ins().jump(ret_block, &[BlockArg::Value(v)]);
            frame.terminated = true;
            Ok(())
        }
        Stmt::For { cond, body, .. } | Stmt::While { cond, body, .. } => {
            emit_loop(c, bcx, frame, cond, body, ret_block)
        }
    }
}

/// 循环: 条件每迭代求值; 体在子作用域发射且绑定分配新鲜单元格 —
/// 跨迭代捕获看到逐迭代值 (每迭代子作用域, spec-notes §附录三)。
fn emit_loop<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    cond: &Expr,
    body: &[Stmt],
    ret_block: Block,
) -> AliasResult<()> {
    ensure_current(bcx, frame);
    let header = bcx.create_block();
    let body_b = bcx.create_block();
    let end_b = bcx.create_block();
    bcx.ins().jump(header, &[]);
    frame.terminated = true;

    // header 不封口: 回边是其第二前驱, 待体末尾跳回后再封
    bcx.switch_to_block(header);
    frame.terminated = false;
    let cv = emit_expr(c, bcx, frame, cond)?;
    bcx.ins().brif(cv, body_b, &[], end_b, &[]);
    frame.terminated = true;
    bcx.seal_block(body_b);
    bcx.seal_block(end_b);

    bcx.switch_to_block(body_b);
    frame.terminated = false;
    frame.scopes.push(HashMap::new());
    frame.locals_vty.push(HashMap::new());
    for s in body {
        ensure_current(bcx, frame);
        emit_stmt(c, bcx, frame, s, ret_block)?;
    }
    frame.scopes.pop();
    frame.locals_vty.pop();
    if !frame.terminated {
        bcx.ins().jump(header, &[]);
    }
    bcx.seal_block(header); // 前驱齐备: 入口跳转 (+ 回边)
    bcx.switch_to_block(end_b);
    frame.terminated = false;
    Ok(())
}
// ---------------------------------------------------------------------------
// 表达式发射 — 一切结果为 64 位规范字
// ---------------------------------------------------------------------------

fn emit_expr<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    e: &Expr,
) -> AliasResult<Value> {
    match e {
        Expr::Int(n, _) => {
            // wrapping 收窄至 i32 后符号扩展为规范字 — 见模块头已知缺口
            Ok(bcx.ins().iconst(types::I64, *n as i32 as i64))
        }
        Expr::Bool(b, _) => Ok(bcx.ins().iconst(types::I64, *b as i64)),
        Expr::Unit(_) => Ok(bcx.ins().iconst(types::I64, 0)),
        Expr::Str(parts, _) => emit_str(c, bcx, frame, parts),
        Expr::Ident(name, span) => match cell_addr(c, frame, name) {
            Some(addr) => Ok(read_cell(bcx, frame, &addr)),
            None => Err(native_err(*span, format!("未定义的绑定 '{name}'"))),
        },
        Expr::Neg { expr, .. } => {
            let v = emit_expr(c, bcx, frame, expr)?;
            let t = bcx.ins().ireduce(types::I32, v);
            let n = bcx.ins().ineg(t);
            Ok(bcx.ins().sextend(types::I64, n))
        }
        Expr::Binary { op, lhs, rhs, span } => {
            // lhs-before-rhs — 黄金记录冻结的求值序
            let l = emit_expr(c, bcx, frame, lhs)?;
            let r = emit_expr(c, bcx, frame, rhs)?;
            use BinOp::*;
            match op {
                Add | Sub | Mul | Div => {
                    let lt = bcx.ins().ireduce(types::I32, l);
                    let rt = bcx.ins().ireduce(types::I32, r);
                    let w = match op {
                        Add => bcx.ins().iadd(lt, rt),
                        Sub => bcx.ins().isub(lt, rt),
                        Mul => bcx.ins().imul(lt, rt),
                        _ => emit_div_guard(c, bcx, lt, rt, *span)?,
                    };
                    Ok(bcx.ins().sextend(types::I64, w))
                }
                Lt | Le | Gt | Ge | EqEq | NotEq => {
                    let lvty = static_vty(c, frame, lhs);
                    match lvty {
                        VTy::Str => {
                            // 字典序字节比较 (compare_str 语义)
                            let ord = call_str_cmp(c, bcx, l, r)?;
                            let cc = match op {
                                Lt => IntCC::SignedLessThan,
                                Le => IntCC::SignedLessThanOrEqual,
                                Gt => IntCC::SignedGreaterThan,
                                Ge => IntCC::SignedGreaterThanOrEqual,
                                EqEq => IntCC::Equal,
                                _ => IntCC::NotEqual,
                            };
                            let b = bcx.ins().icmp_imm_s(cc, ord, 0);
                            Ok(bcx.ins().uextend(types::I64, b))
                        }
                        _ => {
                            let lt = bcx.ins().ireduce(types::I32, l);
                            let rt = bcx.ins().ireduce(types::I32, r);
                            let cc = match op {
                                Lt => IntCC::SignedLessThan,
                                Le => IntCC::SignedLessThanOrEqual,
                                Gt => IntCC::SignedGreaterThan,
                                Ge => IntCC::SignedGreaterThanOrEqual,
                                EqEq => IntCC::Equal,
                                _ => IntCC::NotEqual,
                            };
                            let b = bcx.ins().icmp(cc, lt, rt);
                            Ok(bcx.ins().uextend(types::I64, b))
                        }
                    }
                }
            }
        }
        Expr::Call { callee, args, span } => emit_call(c, bcx, frame, callee, args, *span),
        // Phase 2c: 静态分派 — 接收者字为首个实参, 直调内部函数;
        // 内建字符串方法落运行时符号 (双后端同契约)
        Expr::MethodCall { recv, name, args, span } => {
            emit_method_call(c, bcx, frame, recv, name, args, *span)
        }
        // 字段读取 (Phase 2a): recv 求值 → 实例指针 → 偏移加载。
        // recv 非结构体在 sema 已拒绝 — 此处按不变式直接回查布局
        Expr::Field { recv, name, .. } => {
            let p = emit_expr(c, bcx, frame, recv)?;
            let off = field_offset(c, frame, recv, name)?;
            Ok(bcx.ins().load(types::I64, MemFlagsData::new(), p, off))
        }
        Expr::Index { span, .. } => Err(native_err(
            *span,
            "下标访问尚未接入原生后端 (随 array 类型一起到)",
        )),
        Expr::FuncLit { params, body, .. } => {
            emit_funclit_value(c, bcx, frame, params, body)
        }
        // match 降级 (Phase 2b): 载入 tag → brif 分臂 → join 块参数汇合。
        // never 臂 (return 收尾) 直接跳函数返回块, 不进 join —
        // 双臂皆 never 时无 join, 匹配值取哑字 (sema 已判其类型不可用)。
        Expr::Match { subject, arms, .. } => {
            let subj = emit_expr(c, bcx, frame, subject)?;
            let tag = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 0);
            let is_ok = bcx.ins().icmp_imm_s(IntCC::Equal, tag, 0);
            let ok_b = bcx.create_block();
            let err_b = bcx.create_block();
            let join_b = bcx.create_block();
            let jv = bcx.append_block_param(join_b, types::I64);
            bcx.ins().brif(is_ok, ok_b, &[], err_b, &[]);
            bcx.seal_block(ok_b);
            bcx.seal_block(err_b);

            let ok_arm = arms
                .iter()
                .find(|a| a.ctor == CtorKind::Ok)
                .unwrap_or_else(|| invariant_violation("match ok 臂存在 (sema 已校验)"));
            let err_arm = arms
                .iter()
                .find(|a| a.ctor == CtorKind::Err)
                .unwrap_or_else(|| invariant_violation("match err 臂存在 (sema 已校验)"));

            let bind_vtys = match static_vty(c, frame, subject) {
                VTy::Result(t, e) => (
                    vty_of_type_name(&c.struct_layouts, &t),
                    vty_of_type_name(&c.struct_layouts, &e),
                ),
                _ => (VTy::Other, VTy::Other),
            };

            bcx.switch_to_block(ok_b);
            frame.terminated = false;
            let ok_joined =
                emit_match_arm(c, bcx, frame, ok_arm, bind_vtys.0, subj, join_b)?;
            bcx.switch_to_block(err_b);
            frame.terminated = false;
            let err_joined =
                emit_match_arm(c, bcx, frame, err_arm, bind_vtys.1, subj, join_b)?;

            if ok_joined || err_joined {
                bcx.seal_block(join_b);
                bcx.switch_to_block(join_b);
                frame.terminated = false;
                Ok(jv)
            } else {
                ensure_current(bcx, frame);
                Ok(bcx.ins().iconst(types::I64, 0))
            }
        }
        // expr? 脱糖 (P6): tag==1 → return err(载荷) — 即原样返回主语块
        // (tag 已为 1, 与重包一块可观察等价); 否则值 = 载荷。无需 join。
        Expr::Propagate { expr, .. } => {
            let subj = emit_expr(c, bcx, frame, expr)?;
            let tag = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 0);
            let is_err = bcx.ins().icmp_imm_s(IntCC::Equal, tag, 1);
            let err_b = bcx.create_block();
            let ok_b = bcx.create_block();
            bcx.ins().brif(is_err, err_b, &[], ok_b, &[]);
            bcx.seal_block(err_b);
            bcx.seal_block(ok_b);

            bcx.switch_to_block(err_b);
            let rb = frame
                .ret_block
                .unwrap_or_else(|| invariant_violation("? 仅在函数体内可达 (sema 已校验)"));
            bcx.ins().jump(rb, &[BlockArg::Value(subj)]);
            frame.terminated = true;

            bcx.switch_to_block(ok_b);
            frame.terminated = false;
            Ok(bcx.ins().load(types::I64, MemFlagsData::new(), subj, 8))
        }
    }
}

/// 单臂发射: 绑定 = 新鲜单元格持载荷 (val 语义); 返回是否跳入了 join
/// (false = never 流, 已跳函数返回块)。
fn emit_match_arm<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    arm: &MatchArm,
    bind_vty: VTy,
    subj: Value,
    join_b: Block,
) -> AliasResult<bool> {
    let payload = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 8);
    frame.scopes.push(HashMap::new());
    frame.locals_vty.push(HashMap::new());
    emit_local_cell(c, bcx, frame, payload, bind_vty, &arm.binding)?;
    let joined = match &arm.body {
        ArmBody::Value(e) => {
            let v = emit_expr(c, bcx, frame, e)?;
            bcx.ins().jump(join_b, &[BlockArg::Value(v)]);
            true
        }
        ArmBody::Ret(e) => {
            let v = emit_expr(c, bcx, frame, e)?;
            let rb = frame
                .ret_block
                .unwrap_or_else(|| invariant_violation("never 臂仅在函数体内可达 (sema 已校验)"));
            bcx.ins().jump(rb, &[BlockArg::Value(v)]);
            frame.terminated = true;
            false
        }
        ArmBody::Block(stmts) => {
            let rb = frame
                .ret_block
                .unwrap_or_else(|| invariant_violation("臂内 return 仅在函数体内可达 (sema 已校验)"));
            let n = stmts.len();
            let mut tail: Option<Value> = None;
            for (i, s) in stmts.iter().enumerate() {
                ensure_current(bcx, frame);
                if i + 1 == n {
                    if let Stmt::ExprStmt { expr, .. } = s {
                        tail = Some(emit_expr(c, bcx, frame, expr)?);
                        continue;
                    }
                }
                emit_stmt(c, bcx, frame, s, rb)?;
            }
            if frame.terminated {
                false
            } else {
                // 尾表达式 = 臂值; 其余收尾 (unit 臂) 规范字 0
                let v = tail.unwrap_or_else(|| bcx.ins().iconst(types::I64, 0));
                bcx.ins().jump(join_b, &[BlockArg::Value(v)]);
                true
            }
        }
    };
    frame.scopes.pop();
    frame.locals_vty.pop();
    Ok(joined)
}

/// 插值/字符串字面量: 各部分 display 成串后左折叠 concat。
fn emit_str<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    parts: &[StrPartAst],
) -> AliasResult<Value> {
    let z8 = bcx.ins().iconst(types::I64, 0);
    let z4 = bcx.ins().iconst(types::I32, 0);
    let empty = c.call_rt(bcx, "alias.str.new", &[types::I64, types::I32], Some(types::I64), &[z8, z4])?;
    let mut acc = empty;
    for p in parts {
        let piece = match p {
            StrPartAst::Lit(s) => str_literal_handle(c, bcx, s)?,
            StrPartAst::Hole(h) => {
                let w = emit_expr(c, bcx, frame, h)?;
                display_word(c, bcx, frame, h, w)?
            }
        };
        acc = c.call_rt(bcx, "alias.str.concat", &[types::I64, types::I64], Some(types::I64), &[acc, piece])?;
    }
    Ok(acc)
}

/// 字面量字节经数据段内嵌; 块 = 数据段地址的复制块。
fn str_literal_handle<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    s: &str,
) -> AliasResult<Value> {
    let data_id = match c.str_data.get(s) {
        Some(id) => *id,
        None => {
            let dname = format!("str{}", c.str_data.len());
            let id = c
                .module
                .declare_data(&dname, Linkage::Local, false, false)
                .map_err(|e| native_err(Span::default(), format!("内部: 数据段声明失败 {e}")))?;
            let mut desc = cranelift_module::DataDescription::new();
            desc.define(s.as_bytes().to_vec().into());
            c.module
                .define_data(id, &desc)
                .map_err(|e| native_err(Span::default(), format!("内部: 数据段定义失败 {e}")))?;
            c.str_data.insert(s.to_string(), id);
            id
        }
    };
    let gv = c.module.declare_data_in_func(data_id, &mut bcx.func);
    let addr = bcx.ins().symbol_value(c.ptr_ty, gv);
    let len = bcx.ins().iconst(types::I32, s.len() as i64);
    c.call_rt(bcx, "alias.str.new", &[c.ptr_ty, types::I32], Some(types::I64), &[addr, len])
}

/// 按静态类型把规范字 display 成字符串块 (Value::display 逐字节规则)。
fn display_word<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    e: &Expr,
    w: Value,
) -> AliasResult<Value> {
    match static_vty(c, frame, e) {
        VTy::Int => {
            let t = bcx.ins().ireduce(types::I32, w);
            c.call_rt(bcx, "alias.display.int", &[types::I32], Some(types::I64), &[t])
        }
        VTy::Bool => {
            let t = bcx.ins().ireduce(types::I32, w);
            c.call_rt(bcx, "alias.display.bool", &[types::I32], Some(types::I64), &[t])
        }
        VTy::Str => c.call_rt(bcx, "alias.display.str", &[types::I64], Some(types::I64), &[w]),
        VTy::Unit => c.call_rt(bcx, "alias.display.unit", &[], Some(types::I64), &[]),
        VTy::Func => c.call_rt(bcx, "alias.display.func", &[], Some(types::I64), &[]),
        // 结构体值永不泄露内部布局 — 固定 "<struct>" (与 <func> 同规约)
        VTy::Struct(_) => c.call_rt(bcx, "alias.display.struct", &[], Some(types::I64), &[]),
        // result 值按运行时 tag 显示 <ok>/<err> — 静态类型名不参与
        VTy::Result(..) => {
            let tag = bcx.ins().load(types::I64, MemFlagsData::new(), w, 0);
            let t = bcx.ins().ireduce(types::I32, tag);
            c.call_rt(bcx, "alias.display.result", &[types::I32], Some(types::I64), &[t])
        }
        VTy::Other => Err(native_err(
            e.span(),
            "原生后端无法推断该表达式的显示类型",
        )),
    }
}

fn call_str_cmp<M: Module>(c: &mut Compiler<M>, bcx: &mut FunctionBuilder, l: Value, r: Value) -> AliasResult<Value> {
    c.call_rt(bcx, "alias.str.cmp", &[types::I64, types::I64], Some(types::I32), &[l, r])
}

// ---------------------------------------------------------------------------
// 调用 / 内建 / 闭包创建 / 捕获扫描
// ---------------------------------------------------------------------------

fn emit_call<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    callee: &Expr,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    // 内建特判仅限裸 Ident 被调方 — 黄金记录冻结的分派规则
    if let Expr::Ident(name, _) = callee {
        if name == "increase" || name == "decrease" {
            return emit_incdec(c, bcx, frame, name, args, span);
        }
        if name == "println" || name == "print" {
            return emit_print(c, bcx, frame, name, args, span);
        }
    }
    let clo = match callee {
        Expr::Ident(name, _) => {
            // 结构体构造 (Phase 2a): 名字非绑定且已登记 — 与 sema 的
            // 分派规则逐点镜像 (遮蔽时走普通调用路径)
            if c.struct_layouts.contains_key(name) && cell_addr(c, frame, name).is_none() {
                return emit_construct(c, bcx, frame, name, args);
            }
            // result 构造器 (Phase 2b): 遮蔽镜像规则同上
            if (name == "ok" || name == "err") && cell_addr(c, frame, name).is_none() {
                return emit_result_ctor(c, bcx, frame, name, args);
            }
            match cell_addr(c, frame, name) {
                Some(addr) => read_cell(bcx, frame, &addr),
                None => {
                    return Err(native_err(
                        span,
                        format!("未定义的绑定 '{name}'"),
                    ))
                }
            }
        },
        Expr::FuncLit { params, body, .. } => {
            emit_funclit_value(c, bcx, frame, params, body)?
        }
        _ => return Err(native_err(span, "函数值尚未接入原生后端 (Phase 3)")),
    };
    let code = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 0);
    let env = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 8);
    let mut words: Vec<Value> = Vec::with_capacity(args.len() + 2);
    words.push(bcx.use_var(frame.globals));
    words.push(env);
    for a in args {
        words.push(emit_expr(c, bcx, frame, &a.value)?);
    }
    let sig_ref = bcx.func.import_signature(c.user_sig(args.len()));
    let inst = bcx.ins().call_indirect(sig_ref, code, &words);
    Ok(first_result(bcx, inst))
}

/// 结构体构造发射: 泄漏 n×8 槽区 (alias.env.new 同源), 字段按声明序
/// 求值写入 (显式命名实参优先, 缺省取声明默认值)。全覆盖由 sema 保证。
fn emit_construct<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
) -> AliasResult<Value> {
    let layout = c.struct_layouts[name].clone();
    let n = bcx.ins().iconst(types::I32, layout.len() as i64);
    let ptr = c.call_rt(bcx, "alias.env.new", &[types::I32], Some(types::I64), &[n])?;
    for (i, (fname, default, _fvty)) in layout.iter().enumerate() {
        let expr = args
            .iter()
            .find(|a| a.label.as_deref() == Some(fname.as_str()))
            .map(|a| &a.value)
            .or_else(|| default.as_ref())
            .unwrap_or_else(|| invariant_violation("构造字段全覆盖 (sema 已校验)"));
        let v = emit_expr(c, bcx, frame, expr)?;
        bcx.ins().store(MemFlagsData::new(), v, ptr, ((i as i64) * 8) as i32);
    }
    Ok(ptr)
}

/// result 构造发射 (Phase 2b): 泄漏 2×8 块 {tag, payload} —
/// tag 0=ok / 1=err, 载荷为规范字。镜像 emit_construct 的槽区模式。
fn emit_result_ctor<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
) -> AliasResult<Value> {
    let [arg] = args else {
        return Err(native_err(
            Span::default(),
            format!("{name} 构造恰好接受 1 个参数"),
        ));
    };
    let payload = emit_expr(c, bcx, frame, &arg.value)?;
    let n2 = bcx.ins().iconst(types::I32, 2);
    let blk = c.call_rt(bcx, "alias.env.new", &[types::I32], Some(types::I64), &[n2])?;
    let tag = if name == "ok" { 0i64 } else { 1i64 };
    let tagw = bcx.ins().iconst(types::I64, tag);
    bcx.ins().store(MemFlagsData::new(), tagw, blk, 0);
    bcx.ins().store(MemFlagsData::new(), payload, blk, 8);
    Ok(blk)
}

/// 方法调用发射 (Phase 2c): 接收者先求值 → 静态类型定接收者名 →
/// 用户方法直调 (统一约定 fn(globals, env, self, args...), env 传哑字 0 —
/// 方法无捕获, 自由名经 globals 可达); 内建字符串方法落运行时符号。
/// 接收者静态投影不可知属后端已知缺口 (sema 全知) — 编译期拒绝不 panic。
fn emit_method_call<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    recv: &Expr,
    name: &str,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    let rv = emit_expr(c, bcx, frame, recv)?;
    let tname = match static_vty(c, frame, recv) {
        VTy::Str => "string".to_string(),
        VTy::Int => "i32".to_string(),
        VTy::Bool => "bool".to_string(),
        VTy::Struct(s) => s,
        _ => {
            return Err(native_err(
                span,
                "原生后端无法推断该表达式的接收者类型",
            ))
        }
    };
    if let Some(fid) = c.methods.get(&(tname.clone(), name.to_string())).copied() {
        let fref = c.module.declare_func_in_func(fid, &mut bcx.func);
        let mut words: Vec<Value> = Vec::with_capacity(args.len() + 3);
        words.push(bcx.use_var(frame.globals));
        words.push(bcx.ins().iconst(types::I64, 0));
        words.push(rv);
        for a in args {
            words.push(emit_expr(c, bcx, frame, &a.value)?);
        }
        let inst = bcx.ins().call(fref, &words);
        return Ok(first_result(bcx, inst));
    }
    if tname == "string" {
        match name {
            "len" => {
                let t = c.call_rt(bcx, "alias.str.len", &[types::I64], Some(types::I32), &[rv])?;
                return Ok(bcx.ins().sextend(types::I64, t));
            }
            "upper" => {
                return c.call_rt(bcx, "alias.str.upper", &[types::I64], Some(types::I64), &[rv])
            }
            "lower" => {
                return c.call_rt(bcx, "alias.str.lower", &[types::I64], Some(types::I64), &[rv])
            }
            "trim" => {
                return c.call_rt(bcx, "alias.str.trim", &[types::I64], Some(types::I64), &[rv])
            }
            _ => invariant_violation("字符串方法存在性 (sema 已校验)"),
        }
    }
    invariant_violation("方法存在性 (sema 已校验)")
}

/// 字段偏移回查: recv 静态类型给出结构体名 → 布局表定位下标*8。
/// recv 非结构体/未知字段在 sema 已拒绝 — 违例即编译器不变式破坏。
fn field_offset<M: Module>(
    c: &Compiler<M>,
    frame: &Frame,
    recv: &Expr,
    field: &str,
) -> AliasResult<i32> {
    if let VTy::Struct(s) = static_vty(c, frame, recv) {
        if let Some(layout) = c.struct_layouts.get(&s) {
            if let Some((i, ..)) = layout.iter().enumerate().find(|(_, (n, ..))| n == field) {
                return Ok(((i as i64) * 8) as i32);
            }
        }
    }
    invariant_violation("字段访问目标为结构体实例 (sema 已校验)");
}

fn emit_incdec<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    let [arg] = args else {
        return Err(native_err(span, format!("{name} 恰好接受 1 个参数")));
    };
    // 非标识符实参不求值 — 黄金记录冻结的求值规则
    let Expr::Ident(target, tspan) = &arg.value else {
        return Err(native_err(span, format!("{name} 的参数必须是可变绑定名")));
    };
    let Some(addr) = cell_addr(c, frame, target) else {
        return Err(native_err(*tspan, format!("'{target}' 未定义")));
    };
    let cur = read_cell(bcx, frame, &addr);
    let cur32 = bcx.ins().ireduce(types::I32, cur);
    let delta = if name == "increase" { 1i64 } else { -1i64 };
    let next = bcx.ins().iadd_imm_s(cur32, delta);
    let nextw = bcx.ins().sextend(types::I64, next);
    write_cell(bcx, frame, &addr, nextw);
    Ok(bcx.ins().iconst(types::I64, 0))
}

fn emit_print<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    let [arg] = args else {
        return Err(native_err(span, format!("{name} 恰好接受 1 个参数")));
    };
    let v = emit_expr(c, bcx, frame, &arg.value)?;
    match static_vty(c, frame, &arg.value) {
        VTy::Int => {
            let t = bcx.ins().ireduce(types::I32, v);
            let h = if name == "println" { "alias.println.i32" } else { "alias.print.i32" };
            c.call_rt(bcx, h, &[types::I32], None, &[t])?;
        }
        VTy::Bool => {
            let t = bcx.ins().ireduce(types::I32, v);
            let h = if name == "println" { "alias.println.bool" } else { "alias.print.bool" };
            c.call_rt(bcx, h, &[types::I32], None, &[t])?;
        }
        // Str/Unit/Func/Struct/Result 均经 display 成块后走字符串通道
        // (Struct → 固定 "<struct>"; Result → 运行时 tag 定 <ok>/<err>)
        VTy::Str | VTy::Unit | VTy::Func | VTy::Struct(_) | VTy::Result(..) => {
            let s = display_word(c, bcx, frame, &arg.value, v)?;
            let h = if name == "println" { "alias.println.str" } else { "alias.print.str" };
            c.call_rt(bcx, h, &[types::I64], None, &[s])?;
        }
        VTy::Other => {
            return Err(native_err(
                span,
                "原生后端无法推断该表达式的显示类型",
            ))
        }
    }
    Ok(bcx.ins().iconst(types::I64, 0))
}

/// 除零与 INT_MIN÷-1 显式守卫 → 中止存根 (span-ID 回查原始行:列)。
/// I32 域内守卫 — P2 冻结语义; 后端默认陷阱行为不渗入。
fn emit_div_guard<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    l: Value,
    r: Value,
    span: Span,
) -> AliasResult<Value> {
    let span_id = {
        c.span_table.push((span.line, span.col));
        c.span_table.len() as i32 - 1
    };
    let zero = bcx.ins().iconst(types::I32, 0);
    let m1 = bcx.ins().iconst(types::I32, -1);
    let mini = bcx.ins().iconst(types::I32, i32::MIN as i64);
    let by_zero = bcx.ins().icmp(IntCC::Equal, r, zero);
    let by_m1 = bcx.ins().icmp(IntCC::Equal, r, m1);
    let is_min = bcx.ins().icmp(IntCC::Equal, l, mini);
    let m1_min = bcx.ins().band(by_m1, is_min);
    let trap = bcx.ins().bor(by_zero, m1_min);

    let abort_b = bcx.create_block();
    let ok_b = bcx.create_block();
    bcx.ins().brif(trap, abort_b, &[], ok_b, &[]);
    bcx.seal_block(abort_b);
    bcx.seal_block(ok_b);

    bcx.switch_to_block(abort_b);
    let aid = bcx.ins().iconst(types::I32, span_id as i64);
    let af = c.import_fn("alias.abort_div", &[types::I32], None)?;
    let aref = c.module.declare_func_in_func(af, &mut bcx.func);
    bcx.ins().call(aref, &[aid]); // 运行时侧 process exit(1)/ExitProcess, 不返回
    // 块终结 + 不可达兜底: 正常控制流永不至此, 若抵达则主动中止
    bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);

    bcx.switch_to_block(ok_b);
    Ok(bcx.ins().sdiv(l, r))
}
