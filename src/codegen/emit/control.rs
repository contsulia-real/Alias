use super::*;

pub(crate) fn emit_body<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    body: &Body,
    ret_block: Block,
) -> AliasResult<()> {
    match body {
        Body::Single(stmt) => emit_stmt(c, bcx, frame, stmt, ret_block)?,
        Body::Block(stmts) => {
            for s in stmts {
                ensure_current(bcx, frame);
                emit_stmt(c, bcx, frame, s, ret_block)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn emit_stmt<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    s: &Stmt,
    ret_block: Block,
) -> AliasResult<()> {
    match s {
        Stmt::Binding(b) => {
            if b.kind == BindKind::Func {
                let Expr::FuncLit {
                    params,
                    body,
                    captures,
                    ..
                } = &b.value
                else {
                    return Err(native_err(b.span, "函数绑定必须由函数字面量初始化"));
                };
                let VTy::Func(param_vtys, ret_vty) = c.vty(&b.ty) else {
                    invariant_violation("局部 func 绑定携带完整函数类型")
                };
                let ret_vty = *ret_vty;
                let v = emit_funclit_value_typed(
                    c,
                    bcx,
                    frame,
                    params,
                    body,
                    captures,
                    ret_vty.clone(),
                )?;
                emit_local_cell(
                    c,
                    bcx,
                    frame,
                    v,
                    VTy::Func(param_vtys, Box::new(ret_vty)),
                    b.binding_id,
                )?;
            } else {
                let vty = c.vty(&b.ty);
                let v = emit_expr_expected(c, bcx, frame, &b.value, &vty)?;
                emit_local_cell(c, bcx, frame, v, vty, b.binding_id)?;
            }
            Ok(())
        }
        Stmt::FieldAssign {
            recv,
            field_index,
            value,
            ..
        } => {
            let fvty = field_vty(c, recv, *field_index)?;
            let v = emit_expr_expected(c, bcx, frame, value, &fvty)?;
            let p = emit_expr(c, bcx, frame, recv)?;
            let off = field_offset(c, recv, *field_index)?;
            let sv = norm_store(bcx, v, &fvty);
            bcx.ins().store(MemFlagsData::new(), sv, p, off);
            Ok(())
        }
        Stmt::Assign {
            target,
            target_id,
            value,
            ..
        } => {
            let tvty = bound_vty(c, frame, *target_id);
            let v = emit_expr_expected(c, bcx, frame, value, &tvty)?;
            match cell_addr(c, frame, *target_id) {
                Some(addr) => {
                    write_cell(bcx, frame, &addr, v, &tvty);
                    Ok(())
                }
                None => Err(native_err(
                    Span::default(),
                    format!("内部: 赋值目标 '{target}' 的 BindingId 无存储"),
                )),
            }
        }
        Stmt::ExprStmt { expr, .. } => {
            emit_expr(c, bcx, frame, expr)?;
            Ok(())
        }
        Stmt::Return { value, .. } => {
            let expected = frame
                .ret_vty
                .clone()
                .unwrap_or_else(|| invariant_violation("return 位于函数帧内"));
            if expected == VTy::Unit {
                bcx.ins().jump(ret_block, &[]);
                frame.terminated = true;
                return Ok(());
            }
            let v = match value {
                Some(e) => emit_expr_expected(c, bcx, frame, e, &expected)?,
                None => match cl_type(&expected) {
                    types::F32 => bcx.ins().f32const(0.0),
                    types::F64 => bcx.ins().f64const(0.0),
                    ty => bcx.ins().iconst(ty, 0),
                },
            };
            let v = coerce_ret(bcx, frame, v);
            bcx.ins().jump(ret_block, &[BlockArg::Value(v)]);
            frame.terminated = true;
            Ok(())
        }
        Stmt::If {
            branches,
            else_body,
            ..
        } => emit_if(c, bcx, frame, branches, else_body.as_deref(), ret_block),
        Stmt::While { cond, body, .. } => emit_while(c, bcx, frame, cond, body, ret_block),
        Stmt::For {
            binding_id,
            ty,
            iterable,
            body,
            span,
            ..
        } => {
            let elem_vty = c.vty(ty);
            emit_for(
                c,
                bcx,
                frame,
                iterable,
                *binding_id,
                body,
                &elem_vty,
                *span,
                ret_block,
            )
        }
        Stmt::Break { .. } => {
            let Some((break_b, _)) = frame.loop_targets.last().copied() else {
                return Err(native_err(Span::default(), "break 缺少循环目标"));
            };
            bcx.ins().jump(break_b, &[]);
            frame.terminated = true;
            Ok(())
        }
        Stmt::Continue { .. } => {
            let Some((_, continue_b)) = frame.loop_targets.last().copied() else {
                return Err(native_err(Span::default(), "continue 缺少循环目标"));
            };
            bcx.ins().jump(continue_b, &[]);
            frame.terminated = true;
            Ok(())
        }
    }
}

fn emit_scoped_stmts<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    body: &[Stmt],
    ret_block: Block,
) -> AliasResult<()> {
    push_scope(frame);
    for s in body {
        ensure_current(bcx, frame);
        emit_stmt(c, bcx, frame, s, ret_block)?;
    }
    pop_scope(frame);
    Ok(())
}

fn emit_if<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    branches: &[(Expr, Vec<Stmt>)],
    else_body: Option<&[Stmt]>,
    ret_block: Block,
) -> AliasResult<()> {
    ensure_current(bcx, frame);
    let end_b = bcx.create_block();
    let mut has_fallthrough = false;

    for (idx, (cond, body)) in branches.iter().enumerate() {
        let then_b = bcx.create_block();
        let last = idx + 1 == branches.len();
        let false_b = if last {
            if else_body.is_some() {
                bcx.create_block()
            } else {
                end_b
            }
        } else {
            bcx.create_block()
        };
        let cv = emit_expr(c, bcx, frame, cond)?;
        bcx.ins().brif(cv, then_b, &[], false_b, &[]);
        frame.terminated = true;
        bcx.seal_block(then_b);
        if false_b != end_b {
            bcx.seal_block(false_b);
        }

        bcx.switch_to_block(then_b);
        frame.terminated = false;
        emit_scoped_stmts(c, bcx, frame, body, ret_block)?;
        if !frame.terminated {
            bcx.ins().jump(end_b, &[]);
            frame.terminated = true;
            has_fallthrough = true;
        }

        if last {
            if let Some(else_stmts) = else_body {
                bcx.switch_to_block(false_b);
                frame.terminated = false;
                emit_scoped_stmts(c, bcx, frame, else_stmts, ret_block)?;
                if !frame.terminated {
                    bcx.ins().jump(end_b, &[]);
                    frame.terminated = true;
                    has_fallthrough = true;
                }
            } else {
                has_fallthrough = true;
            }
        } else {
            bcx.switch_to_block(false_b);
            frame.terminated = false;
        }
    }

    bcx.seal_block(end_b);
    if has_fallthrough {
        bcx.switch_to_block(end_b);
        frame.terminated = false;
    } else {
        frame.terminated = true;
    }
    Ok(())
}

fn emit_while<M: Module>(
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

    bcx.switch_to_block(header);
    frame.terminated = false;
    let cv = emit_expr(c, bcx, frame, cond)?;
    bcx.ins().brif(cv, body_b, &[], end_b, &[]);
    frame.terminated = true;
    bcx.seal_block(body_b);

    bcx.switch_to_block(body_b);
    frame.terminated = false;
    frame.loop_targets.push((end_b, header));
    emit_scoped_stmts(c, bcx, frame, body, ret_block)?;
    frame.loop_targets.pop();
    if !frame.terminated {
        bcx.ins().jump(header, &[]);
        frame.terminated = true;
    }

    bcx.seal_block(header);
    bcx.seal_block(end_b);
    bcx.switch_to_block(end_b);
    frame.terminated = false;
    Ok(())
}

fn emit_for<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    iterable: &Expr,
    binding_id: BindingId,
    body: &[Stmt],
    elem_vty: &VTy,
    span: Span,
    ret_block: Block,
) -> AliasResult<()> {
    ensure_current(bcx, frame);
    let source_vty = c.vty(iterable.ty());
    let source = emit_expr(c, bcx, frame, iterable)?;
    let iter = match source_vty {
        VTy::Array(_) => make_iterator(c, bcx, source)?,
        VTy::Iterator(_) => source,
        _ => invariant_violation("for 主语为 array/iterator (sema 已校验)"),
    };

    let header = bcx.create_block();
    let valid_b = bcx.create_block();
    let invalid_b = bcx.create_block();
    let body_b = bcx.create_block();
    let end_b = bcx.create_block();
    bcx.ins().jump(header, &[]);
    frame.terminated = true;

    bcx.switch_to_block(header);
    frame.terminated = false;
    let array = bcx.ins().load(types::I64, MemFlagsData::new(), iter, 0);
    let expected = bcx.ins().load(types::I64, MemFlagsData::new(), iter, 16);
    let actual = array_version(bcx, array);
    let invalid = bcx.ins().icmp(IntCC::NotEqual, actual, expected);
    bcx.ins().brif(invalid, invalid_b, &[], valid_b, &[]);
    frame.terminated = true;
    bcx.seal_block(invalid_b);
    bcx.seal_block(valid_b);

    bcx.switch_to_block(invalid_b);
    emit_iterator_abort(c, bcx, span)?;
    frame.terminated = true;

    bcx.switch_to_block(valid_b);
    frame.terminated = false;
    let cursor = bcx.ins().load(types::I64, MemFlagsData::new(), iter, 8);
    let raw = array_raw(bcx, array);
    let len = bcx.ins().load(types::I64, MemFlagsData::new(), raw, 8);
    let more = bcx.ins().icmp(IntCC::UnsignedLessThan, cursor, len);
    bcx.ins().brif(more, body_b, &[], end_b, &[]);
    frame.terminated = true;
    bcx.seal_block(body_b);

    bcx.switch_to_block(body_b);
    frame.terminated = false;
    let raw = array_raw(bcx, array);
    let data = bcx.ins().load(types::I64, MemFlagsData::new(), raw, 0);
    let off = bcx.ins().imul_imm_s(cursor, 8);
    let addr = bcx.ins().iadd(data, off);
    let raw_elem = bcx
        .ins()
        .load(cl_type(elem_vty), MemFlagsData::new(), addr, 0);
    let elem = norm_load(bcx, raw_elem, elem_vty);
    let next = bcx.ins().iadd_imm_s(cursor, 1);
    bcx.ins().store(MemFlagsData::new(), next, iter, 8);

    push_scope(frame);
    emit_local_cell(c, bcx, frame, elem, elem_vty.clone(), binding_id)?;
    frame.loop_targets.push((end_b, header));
    for s in body {
        ensure_current(bcx, frame);
        emit_stmt(c, bcx, frame, s, ret_block)?;
    }
    frame.loop_targets.pop();
    pop_scope(frame);
    if !frame.terminated {
        bcx.ins().jump(header, &[]);
        frame.terminated = true;
    }

    bcx.seal_block(header);
    bcx.seal_block(end_b);
    bcx.switch_to_block(end_b);
    frame.terminated = false;
    Ok(())
}
