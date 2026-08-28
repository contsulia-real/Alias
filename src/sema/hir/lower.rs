use super::*;
use crate::{AliasError, AliasResult, Span};
use std::collections::HashMap;

pub(super) fn lower(
    program: crate::ast::Program,
    mut facts: LowerFacts,
) -> AliasResult<CheckedProgram> {
    let mut checked = CheckedProgram {
        imports: program.imports.clone(),
        items: program
            .items
            .iter()
            .map(|item| lower_item(item, &mut facts))
            .collect::<AliasResult<Vec<_>>>()?,
    };
    ensure_facts_consumed(&facts)?;
    super::validate::validate_resolved_hir(&checked)?;
    super::capture::populate_captures(&mut checked);
    Ok(checked)
}

fn ensure_facts_consumed(facts: &LowerFacts) -> AliasResult<()> {
    let leftovers = [
        ("表达式", facts.exprs.len()),
        ("绑定类型", facts.bindings.len()),
        ("BindingId", facts.binding_ids.len()),
        ("方法接收者", facts.receivers.len()),
        ("MethodId", facts.method_ids.len()),
        ("方法 self", facts.method_self_ids.len()),
        ("结构体字段", facts.fields.len()),
        ("字段索引", facts.field_indices.len()),
        ("字段赋值索引", facts.field_assign_indices.len()),
        ("函数参数类型", facts.params.len()),
        ("函数参数 BindingId", facts.param_ids.len()),
        ("for 循环变量类型", facts.fors.len()),
        ("for 循环变量 BindingId", facts.for_ids.len()),
        ("赋值目标 BindingId", facts.assign_target_ids.len()),
        ("Pattern BindingId", facts.match_binding_ids.len()),
        ("标识符 BindingId", facts.expr_binding_ids.len()),
    ];
    if let Some((what, count)) = leftovers.into_iter().find(|(_, count)| *count != 0) {
        return Err(AliasError {
            msg: format!("内部 sema 不变式被破坏: 存在 {count} 条未消费的{what} fact"),
            span: Span::default(),
        });
    }
    Ok(())
}

fn take_required<T: Copy>(
    facts: &mut HashMap<usize, T>,
    key: usize,
    span: Span,
    what: &str,
) -> AliasResult<T> {
    facts.remove(&key).ok_or_else(|| AliasError {
        msg: format!("内部 sema 不变式被破坏: {what}缺失"),
        span,
    })
}

fn lower_item(item: &crate::ast::Item, facts: &mut LowerFacts) -> AliasResult<Item> {
    Ok(match item {
        crate::ast::Item::Binding(binding) => Item::Binding(lower_binding(binding, facts)?),
        crate::ast::Item::StructDef(def) => Item::StructDef(StructDef {
            name: def.name.clone(),
            fields: def
                .fields
                .iter()
                .map(|field| {
                    let key = field as *const crate::ast::StructField as usize;
                    Ok(StructField {
                        name: field.name.clone(),
                        mutable: field.mutable,
                        ty: facts.fields.remove(&key).ok_or_else(|| AliasError {
                            msg: "内部 sema 不变式被破坏: 结构体字段缺少静态类型".into(),
                            span: field.span,
                        })?,
                        default: field
                            .default
                            .as_ref()
                            .map(|expr| lower_expr(expr, facts))
                            .transpose()?,
                        span: field.span,
                    })
                })
                .collect::<AliasResult<Vec<_>>>()?,
            span: def.span,
        }),
    })
}

fn lower_binding(binding: &crate::ast::Binding, facts: &mut LowerFacts) -> AliasResult<Binding> {
    let key = binding as *const crate::ast::Binding as usize;
    let binding_id = take_required(&mut facts.binding_ids, key, binding.span, "绑定 BindingId")?;
    let ty = facts.bindings.remove(&key).ok_or_else(|| AliasError {
        msg: "内部 sema 不变式被破坏: 绑定缺少静态类型".into(),
        span: binding.span,
    })?;
    let receiver = facts.receivers.remove(&key);
    let method_id = facts.method_ids.remove(&key);
    let self_id = facts.method_self_ids.remove(&key);
    if receiver.is_some() != method_id.is_some() || receiver.is_some() != self_id.is_some() {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: 方法绑定缺少 MethodId/self BindingId".into(),
            span: binding.span,
        });
    }
    let mut value = lower_expr(&binding.value, facts)?;
    if let Some(id) = self_id {
        let Expr::FuncLit {
            implicit_bindings, ..
        } = &mut value
        else {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: 方法值不是 FuncLit".into(),
                span: binding.span,
            });
        };
        implicit_bindings.push(id);
    }
    Ok(Binding {
        binding_id,
        method_id,
        self_id,
        is_pub: binding.is_pub,
        kind: binding.kind,
        ty,
        name: binding.name.clone(),
        receiver,
        value,
        span: binding.span,
    })
}

fn lower_body(body: &crate::ast::Body, facts: &mut LowerFacts) -> AliasResult<Body> {
    Ok(match body {
        crate::ast::Body::Block(stmts) => Body::Block(
            stmts
                .iter()
                .map(|stmt| lower_stmt(stmt, facts))
                .collect::<AliasResult<Vec<_>>>()?,
        ),
        crate::ast::Body::Single(stmt) => Body::Single(Box::new(lower_stmt(stmt, facts)?)),
    })
}

fn lower_stmt(stmt: &crate::ast::Stmt, facts: &mut LowerFacts) -> AliasResult<Stmt> {
    let key = stmt as *const crate::ast::Stmt as usize;
    Ok(match stmt {
        crate::ast::Stmt::Binding(binding) => Stmt::Binding(lower_binding(binding, facts)?),
        crate::ast::Stmt::Assign { target, value, span } => Stmt::Assign {
            target: target.clone(),
            target_id: take_required(
                &mut facts.assign_target_ids,
                key,
                *span,
                "赋值目标 BindingId",
            )?,
            value: lower_expr(value, facts)?,
            span: *span,
        },
        crate::ast::Stmt::FieldAssign {
            recv,
            field,
            value,
            span,
        } => Stmt::FieldAssign {
            recv: Box::new(lower_expr(recv, facts)?),
            field: field.clone(),
            field_index: take_required(
                &mut facts.field_assign_indices,
                key,
                *span,
                "字段赋值索引",
            )?,
            value: lower_expr(value, facts)?,
            span: *span,
        },
        crate::ast::Stmt::ExprStmt { expr, span } => Stmt::ExprStmt {
            expr: lower_expr(expr, facts)?,
            span: *span,
        },
        crate::ast::Stmt::Return { value, span } => Stmt::Return {
            value: value
                .as_ref()
                .map(|expr| lower_expr(expr, facts))
                .transpose()?,
            span: *span,
        },
        crate::ast::Stmt::If {
            branches,
            else_body,
            span,
        } => Stmt::If {
            branches: branches
                .iter()
                .map(|(cond, body)| {
                    Ok((
                        lower_expr(cond, facts)?,
                        body.iter()
                            .map(|stmt| lower_stmt(stmt, facts))
                            .collect::<AliasResult<Vec<_>>>()?,
                    ))
                })
                .collect::<AliasResult<Vec<_>>>()?,
            else_body: else_body
                .as_ref()
                .map(|body| {
                    body.iter()
                        .map(|stmt| lower_stmt(stmt, facts))
                        .collect::<AliasResult<Vec<_>>>()
                })
                .transpose()?,
            span: *span,
        },
        crate::ast::Stmt::While { cond, body, span } => Stmt::While {
            cond: lower_expr(cond, facts)?,
            body: body
                .iter()
                .map(|stmt| lower_stmt(stmt, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
        },
        crate::ast::Stmt::For {
            name,
            iterable,
            body,
            span,
            ..
        } => Stmt::For {
            binding_id: take_required(&mut facts.for_ids, key, *span, "for BindingId")?,
            ty: facts.fors.remove(&key).ok_or_else(|| AliasError {
                msg: "内部 sema 不变式被破坏: for 循环变量缺少静态类型".into(),
                span: *span,
            })?,
            name: name.clone(),
            iterable: lower_expr(iterable, facts)?,
            body: body
                .iter()
                .map(|stmt| lower_stmt(stmt, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
        },
        crate::ast::Stmt::Break { span } => Stmt::Break { span: *span },
        crate::ast::Stmt::Continue { span } => Stmt::Continue { span: *span },
    })
}

fn lower_expr(expr: &crate::ast::Expr, facts: &mut LowerFacts) -> AliasResult<Expr> {
    let key = expr as *const crate::ast::Expr as usize;
    let Some(mut info) = facts.exprs.remove(&key) else {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: 表达式缺少静态类型".into(),
            span: expr.span(),
        });
    };

    if let Some(callee_ty) = info.implicit_zero_callee.take() {
        if info.call_target != Some(CallTarget::FunctionValue) {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: 零参裸名调用缺少函数调用目标".into(),
                span: expr.span(),
            });
        }
        let callee_info = ExprInfo {
            ty: callee_ty,
            call_target: None,
            implicit_zero_callee: None,
        };
        let callee = match expr {
            crate::ast::Expr::Ident(name, span) => Expr::Ident(
                name.clone(),
                Some(take_required(
                    &mut facts.expr_binding_ids,
                    key,
                    *span,
                    "零参函数 callee BindingId",
                )?),
                *span,
                callee_info,
            ),
            crate::ast::Expr::This(span) => Expr::This(*span, callee_info),
            _ => {
                return Err(AliasError {
                    msg: "内部 sema 不变式被破坏: 零参隐式调用只允许直接函数引用".into(),
                    span: expr.span(),
                })
            }
        };
        return Ok(Expr::Call {
            callee: Box::new(callee),
            args: Vec::new(),
            span: expr.span(),
            info,
        });
    }

    let is_call = matches!(
        expr,
        crate::ast::Expr::Call { .. }
            | crate::ast::Expr::Juxtapose { .. }
            | crate::ast::Expr::MethodCall { .. }
    );
    if is_call && info.call_target.is_none() {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: 调用表达式缺少已解析目标".into(),
            span: expr.span(),
        });
    }
    if matches!(
        info.call_target,
        Some(CallTarget::Method(MethodTarget::User { id: None, .. }))
    ) {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: 用户方法调用缺少 MethodId".into(),
            span: expr.span(),
        });
    }

    Ok(match expr {
        crate::ast::Expr::Int(value, span) => Expr::Int(*value, *span, info),
        crate::ast::Expr::Float(value, span) => Expr::Float(*value, *span, info),
        crate::ast::Expr::Bool(value, span) => Expr::Bool(*value, *span, info),
        crate::ast::Expr::Str(parts, span) => Expr::Str(
            parts
                .iter()
                .map(|part| match part {
                    crate::ast::StrPartAst::Lit(value) => Ok(StrPart::Lit(value.clone())),
                    crate::ast::StrPartAst::Hole(expr) => {
                        Ok(StrPart::Hole(Box::new(lower_expr(expr, facts)?)))
                    }
                })
                .collect::<AliasResult<Vec<_>>>()?,
            *span,
            info,
        ),
        crate::ast::Expr::Ident(name, span) => Expr::Ident(
            name.clone(),
            facts.expr_binding_ids.remove(&key),
            *span,
            info,
        ),
        crate::ast::Expr::This(span) => Expr::This(*span, info),
        crate::ast::Expr::Cast { expr, span, .. } => Expr::Cast {
            expr: Box::new(lower_expr(expr, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
            op: *op,
            lhs: Box::new(lower_expr(lhs, facts)?),
            rhs: Box::new(lower_expr(rhs, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Neg { expr, span } => Expr::Neg {
            expr: Box::new(lower_expr(expr, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Not { expr, span } => Expr::Not {
            expr: Box::new(lower_expr(expr, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::BitNot { expr, span } => Expr::BitNot {
            expr: Box::new(lower_expr(expr, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            span,
        } => Expr::Ternary {
            cond: Box::new(lower_expr(cond, facts)?),
            then_expr: Box::new(lower_expr(then_expr, facts)?),
            else_expr: Box::new(lower_expr(else_expr, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Call { callee, args, span } => Expr::Call {
            callee: Box::new(lower_expr(callee, facts)?),
            args: args
                .iter()
                .map(|arg| lower_call_arg(arg, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
            info,
        },
        crate::ast::Expr::Juxtapose { lhs, rhs, span } => match info.call_target.as_ref() {
            Some(CallTarget::FunctionValue) => Expr::Call {
                callee: Box::new(lower_expr(lhs, facts)?),
                args: vec![CallArg {
                    label: None,
                    value: lower_expr(rhs, facts)?,
                    span: rhs.span(),
                }],
                span: *span,
                info,
            },
            Some(CallTarget::Method(_)) => {
                if !matches!(rhs.as_ref(), crate::ast::Expr::Ident(..)) {
                    return Err(AliasError {
                        msg: "内部 sema 不变式被破坏: 零参方法中缀 RHS 不是方法名".into(),
                        span: rhs.span(),
                    });
                }
                Expr::MethodCall {
                    recv: Box::new(lower_expr(lhs, facts)?),
                    args: Vec::new(),
                    span: *span,
                    info,
                }
            }
            _ => {
                return Err(AliasError {
                    msg: "内部 sema 不变式被破坏: 两项无括号邻接未解析".into(),
                    span: *span,
                })
            }
        },
        crate::ast::Expr::MethodCall {
            recv, args, span, ..
        } => Expr::MethodCall {
            recv: Box::new(lower_expr(recv, facts)?),
            args: args
                .iter()
                .map(|arg| lower_call_arg(arg, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
            info,
        },
        crate::ast::Expr::Field { recv, name, span } => Expr::Field {
            recv: Box::new(lower_expr(recv, facts)?),
            name: name.clone(),
            field_index: take_required(
                &mut facts.field_indices,
                key,
                *span,
                "字段访问索引",
            )?,
            span: *span,
            info,
        },
        crate::ast::Expr::Index { recv, idx, span } => Expr::Index {
            recv: Box::new(lower_expr(recv, facts)?),
            idx: Box::new(lower_expr(idx, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::ArrayLit { elems, span } => Expr::ArrayLit {
            elems: elems
                .iter()
                .map(|expr| lower_expr(expr, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
            info,
        },
        crate::ast::Expr::FuncLit { params, body, span } => Expr::FuncLit {
            params: params
                .iter()
                .map(|param| {
                    let pkey = param as *const crate::ast::Param as usize;
                    Ok(Param {
                        binding_id: take_required(
                            &mut facts.param_ids,
                            pkey,
                            param.span,
                            "函数参数 BindingId",
                        )?,
                        ty: facts.params.remove(&pkey).ok_or_else(|| AliasError {
                            msg: "内部 sema 不变式被破坏: 函数参数缺少静态类型".into(),
                            span: param.span,
                        })?,
                        name: param.name.clone(),
                        span: param.span,
                    })
                })
                .collect::<AliasResult<Vec<_>>>()?,
            implicit_bindings: Vec::new(),
            captures: Vec::new(),
            body: Box::new(lower_body(body, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Match {
            subject,
            arms,
            span,
        } => Expr::Match {
            subject: Box::new(lower_expr(subject, facts)?),
            arms: arms
                .iter()
                .map(|arm| lower_match_arm(arm, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
            info,
        },
        crate::ast::Expr::Propagate { expr, span } => Expr::Propagate {
            expr: Box::new(lower_expr(expr, facts)?),
            span: *span,
            info,
        },
    })
}

fn lower_call_arg(arg: &crate::ast::CallArg, facts: &mut LowerFacts) -> AliasResult<CallArg> {
    Ok(CallArg {
        label: arg.label.clone(),
        value: lower_expr(&arg.value, facts)?,
        span: arg.span,
    })
}

fn lower_match_arm(arm: &crate::ast::MatchArm, facts: &mut LowerFacts) -> AliasResult<MatchArm> {
    let key = arm as *const crate::ast::MatchArm as usize;
    Ok(MatchArm {
        pattern: arm.pattern.clone(),
        binding_id: facts.match_binding_ids.remove(&key),
        body: match &arm.body {
            crate::ast::ArmBody::Block(stmts) => ArmBody::Block(
                stmts
                    .iter()
                    .map(|stmt| lower_stmt(stmt, facts))
                    .collect::<AliasResult<Vec<_>>>()?,
            ),
            crate::ast::ArmBody::Value(expr) => ArmBody::Value(Box::new(lower_expr(expr, facts)?)),
            crate::ast::ArmBody::Ret(expr) => ArmBody::Ret(Box::new(lower_expr(expr, facts)?)),
        },
        span: arm.span,
    })
}
