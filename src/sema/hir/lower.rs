use super::{
    ArmBody, Binding, BindingId, BindingOwner, Body, CallArg, CallTarget, CheckedProgram, Expr,
    ExprInfo, Item, LowerFacts, LowerPlaceInfo, MatchArm, Param, Place, PlaceInfo, Stmt, StrPart,
    StructDef, StructField,
};
use crate::sema::LowerCallTarget;
use crate::{AliasError, AliasResult, Span};
use std::collections::HashMap;

pub(super) fn lower(
    program: crate::ast::Program,
    mut facts: LowerFacts,
    main_id: BindingId,
) -> AliasResult<CheckedProgram> {
    let mut checked = CheckedProgram {
        main_id,
        items: program
            .items
            .iter()
            .map(|item| lower_item(item, &mut facts))
            .collect::<AliasResult<Vec<_>>>()?,
    };
    ensure_facts_consumed(&facts)?;
    // capture 向量属于最终 HIR 合同，因此权威 invariant gate 必须在 capture 写回之后。
    // 若提前验证，后续 mutation 会让 codegen 消费一个从未通过最终门禁的对象。
    super::capture::populate_captures(&mut checked)?;
    super::validate_resolved_hir(&checked)?;
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
        ("赋值 Place", facts.assignment_places.len()),
        ("构造器实参字段索引", facts.ctor_arg_indices.len()),
        ("函数参数类型", facts.params.len()),
        ("函数参数 BindingId", facts.param_ids.len()),
        ("for 循环变量类型", facts.fors.len()),
        ("for 循环变量 BindingId", facts.for_ids.len()),
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

fn take_required<T>(
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
                    // 这些地址是 check 建立的短生命周期 fact identity；check 与本次
                    // lowering 之间不得存在移动 AST 节点的 phase。
                    let key = field as *const crate::ast::StructField as usize;
                    Ok(StructField {
                        ty: facts.fields.remove(&key).ok_or_else(|| AliasError {
                            msg: "内部 sema 不变式被破坏: 结构体字段缺少静态类型".into(),
                            span: field.span,
                        })?,
                        mutable: field.mutable,
                        default: field
                            .default
                            .as_ref()
                            .map(|expr| lower_expr(expr, facts))
                            .transpose()?,
                        span: field.span,
                    })
                })
                .collect::<AliasResult<Vec<_>>>()?,
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
    let owner = match (receiver, method_id, self_id) {
        (None, None, None) => BindingOwner::Ordinary,
        (Some(receiver), Some(method_id), Some(self_id)) => BindingOwner::Method {
            method_id,
            self_id,
            receiver,
        },
        _ => {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: 方法绑定缺少 MethodId/self BindingId".into(),
                span: binding.span,
            })
        }
    };
    let mut value = lower_expr(&binding.value, facts)?;
    if let BindingOwner::Method { self_id, .. } = &owner {
        let Expr::FuncLit {
            implicit_bindings, ..
        } = &mut value
        else {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: 方法值不是 FuncLit".into(),
                span: binding.span,
            });
        };
        implicit_bindings.push(*self_id);
    }
    Ok(Binding {
        binding_id,
        owner,
        kind: binding.kind,
        ty,
        name: binding.name.clone(),
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
        crate::ast::Stmt::Assign { value, span, .. } => {
            // check 已把每个赋值解析成唯一 Place；variant 若与 AST 形状不一致，说明
            // check→lower identity/语义合同已破坏，不能把另一种 Place 重新解释成 local。
            let target = match take_required(
                &mut facts.assignment_places,
                key,
                *span,
                "赋值 Place",
            )? {
                LowerPlaceInfo::Local { binding_id, ty } => Place::Local {
                    binding_id,
                    info: PlaceInfo { ty, span: *span },
                },
                LowerPlaceInfo::Field { .. } => {
                    return Err(AliasError {
                        msg: "内部 sema 不变式被破坏: 普通赋值携带字段 Place".into(),
                        span: *span,
                    })
                }
            };
            Stmt::Assign {
                target,
                value: lower_expr(value, facts)?,
            }
        }
        crate::ast::Stmt::FieldAssign {
            recv, value, span, ..
        } => {
            // 源码级 FieldAssign 只属于 parser AST；final HIR 统一固化为 Assign + Place。
            let (field_index, ty) = match take_required(
                &mut facts.assignment_places,
                key,
                *span,
                "赋值 Place",
            )? {
                LowerPlaceInfo::Field { field_index, ty } => (field_index, ty),
                LowerPlaceInfo::Local { .. } => {
                    return Err(AliasError {
                        msg: "内部 sema 不变式被破坏: 字段赋值携带 local Place".into(),
                        span: *span,
                    })
                }
            };
            Stmt::Assign {
                target: Place::Field {
                    recv: Box::new(lower_expr(recv, facts)?),
                    field_index,
                    info: PlaceInfo { ty, span: *span },
                },
                value: lower_expr(value, facts)?,
            }
        }
        crate::ast::Stmt::Expr { expr } => Stmt::Expr {
            expr: lower_expr(expr, facts)?,
        },
        crate::ast::Stmt::Return { value, .. } => Stmt::Return {
            value: value
                .as_ref()
                .map(|expr| lower_expr(expr, facts))
                .transpose()?,
        },
        crate::ast::Stmt::If {
            branches,
            else_body,
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
        },
        crate::ast::Stmt::While { cond, body, .. } => Stmt::While {
            cond: lower_expr(cond, facts)?,
            body: body
                .iter()
                .map(|stmt| lower_stmt(stmt, facts))
                .collect::<AliasResult<Vec<_>>>()?,
        },
        crate::ast::Stmt::For {
            iterable,
            body,
            span,
            ..
        } => Stmt::For {
            binding_id: take_required(&mut facts.for_ids, key, *span, "for 循环变量 BindingId")?,
            ty: facts.fors.remove(&key).ok_or_else(|| AliasError {
                msg: "内部 sema 不变式被破坏: for 循环变量缺少静态类型".into(),
                span: *span,
            })?,
            iterable: lower_expr(iterable, facts)?,
            body: body
                .iter()
                .map(|stmt| lower_stmt(stmt, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
        },
        crate::ast::Stmt::Break { .. } => Stmt::Break,
        crate::ast::Stmt::Continue { .. } => Stmt::Continue,
    })
}

fn lower_expr(expr: &crate::ast::Expr, facts: &mut LowerFacts) -> AliasResult<Expr> {
    // 指针 key 只是 phase 内 identity，不是持久 NodeId。check() 针对这份 Program 的
    // 精确 allocation 记录 fact，并立即把同一对象交给 lower()；中间移动、clone 或替换
    // 任一 AST 节点都会让下方 lookup 全部失效。未来若引入 AST rewrite，必须先改为稳定
    // NodeId，不能用 fallback 掩盖 identity 断裂。
    let key = expr as *const crate::ast::Expr as usize;
    let mut lower_info = facts.exprs.remove(&key).ok_or_else(|| AliasError {
        msg: "内部 sema 不变式被破坏: 表达式缺少静态类型".into(),
        span: expr.span(),
    })?;
    let is_call = matches!(
        expr,
        crate::ast::Expr::Call { .. }
            | crate::ast::Expr::Juxtapose { .. }
            | crate::ast::Expr::MethodCall { .. }
    );
    if is_call && lower_info.call_target.is_none() {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: 调用表达式缺少已解析目标".into(),
            span: expr.span(),
        });
    }
    let mut call_target = lower_info.call_target.take();
    let implicit_zero_callee = lower_info.implicit_zero_callee.take();
    let info = ExprInfo { ty: lower_info.ty };

    if let Some(callee_ty) = implicit_zero_callee {
        if !matches!(call_target.take(), Some(LowerCallTarget::FunctionValue)) {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: 隐式零参调用缺少函数调用 target".into(),
                span: expr.span(),
            });
        }
        let callee_info = ExprInfo { ty: callee_ty };
        let callee = match expr {
            crate::ast::Expr::Ident(name, span) => Expr::Ident(
                name.clone(),
                facts.expr_binding_ids.remove(&key),
                *span,
                callee_info,
            ),
            crate::ast::Expr::This(span) => Expr::This(*span, callee_info),
            _ => {
                return Err(AliasError {
                    msg: "内部 sema 不变式被破坏: 隐式零参调用的 callee 不是直接函数引用".into(),
                    span: expr.span(),
                })
            }
        };
        return Ok(Expr::Call {
            callee: Box::new(callee),
            args: Vec::new(),
            target: CallTarget::FunctionValue,
            span: expr.span(),
            info,
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
        crate::ast::Expr::Call { callee, args, span } => {
            let target = call_target.take().ok_or_else(|| AliasError {
                msg: "内部 sema 不变式被破坏: Call 缺少 target".into(),
                span: *span,
            })?;
            match target {
                LowerCallTarget::ContextualConversion(mode) => {
                    let [_arg] = args.as_slice() else {
                        return Err(AliasError {
                            msg: "内部 sema 不变式被破坏: 上下文转换元数不是 1".into(),
                            span: *span,
                        });
                    };
                    // callee 的 sema facts 仍需消费，但最终 HIR 不保留 `from/try_from`
                    // 名字；backend 只能看到已经裁决好的 Convert/Identity 节点。
                    let _ = lower_expr(callee, facts)?;
                    let value = lower_expr(&args[0].value, facts)?;
                    Expr::Convert {
                        expr: Box::new(value),
                        mode,
                        span: *span,
                        info,
                    }
                }
                LowerCallTarget::Typeof => {
                    let [_arg] = args.as_slice() else {
                        return Err(AliasError {
                            msg: "内部 sema 不变式被破坏: typeof 元数不是 1".into(),
                            span: *span,
                        });
                    };
                    let _ = lower_expr(callee, facts)?;
                    let operand = lower_expr(&args[0].value, facts)?;
                    let type_name = operand.ty().name();
                    Expr::Typeof {
                        type_name,
                        span: *span,
                        info,
                    }
                }
                other => Expr::Call {
                    callee: Box::new(lower_expr(callee, facts)?),
                    args: args
                        .iter()
                        .map(|arg| lower_call_arg(arg, facts))
                        .collect::<AliasResult<Vec<_>>>()?,
                    target: lower_call_target(other, args, facts, *span)?,
                    span: *span,
                    info,
                },
            }
        }
        crate::ast::Expr::Juxtapose { lhs, rhs, span } => match call_target.take() {
            Some(LowerCallTarget::FunctionValue) => Expr::Call {
                callee: Box::new(lower_expr(lhs, facts)?),
                args: vec![CallArg {
                    value: lower_expr(rhs, facts)?,
                }],
                target: CallTarget::FunctionValue,
                span: *span,
                info,
            },
            Some(LowerCallTarget::Method(target)) => {
                if !matches!(rhs.as_ref(), crate::ast::Expr::Ident(..)) {
                    return Err(AliasError {
                        msg: "内部 sema 不变式被破坏: 零参方法中缀 RHS 不是方法名".into(),
                        span: rhs.span(),
                    });
                }
                // RHS 方法名不是可求值表达式；sema 只为整个 Juxtapose 记录 target，
                // 因此这里不 lower RHS，避免制造不存在的 BindingId 要求。
                Expr::MethodCall {
                    recv: Box::new(lower_expr(lhs, facts)?),
                    args: Vec::new(),
                    target,
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
            target: match call_target.take() {
                Some(LowerCallTarget::Method(target)) => target,
                _ => {
                    return Err(AliasError {
                        msg: "内部 sema 不变式被破坏: MethodCall 缺少方法 target".into(),
                        span: *span,
                    })
                }
            },
            span: *span,
            info,
        },
        crate::ast::Expr::Field { recv, span, .. } => Expr::Field {
            recv: Box::new(lower_expr(recv, facts)?),
            field_index: take_required(&mut facts.field_indices, key, *span, "字段访问索引")?,
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

fn lower_call_target(
    target: LowerCallTarget,
    args: &[crate::ast::CallArg],
    facts: &mut LowerFacts,
    span: Span,
) -> AliasResult<CallTarget> {
    match target {
        LowerCallTarget::StructConstructor(name) => {
            let arg_field_indices = args
                .iter()
                .map(|arg| {
                    take_required(
                        &mut facts.ctor_arg_indices,
                        arg as *const crate::ast::CallArg as usize,
                        arg.span,
                        "构造器实参字段索引",
                    )
                })
                .collect::<AliasResult<Vec<_>>>()?;
            Ok(CallTarget::StructConstructor {
                name,
                arg_field_indices,
            })
        }
        LowerCallTarget::Method(_) => Err(AliasError {
            msg: "内部 sema 不变式被破坏: 普通 Call 携带方法 target".into(),
            span,
        }),
        LowerCallTarget::Typeof | LowerCallTarget::ContextualConversion(_) => Err(AliasError {
            msg: "内部 sema 不变式被破坏: 已解析静态操作进入普通 Call lowering".into(),
            span,
        }),
        LowerCallTarget::FunctionValue => Ok(CallTarget::FunctionValue),
        LowerCallTarget::ResultConstructor(kind) => Ok(CallTarget::ResultConstructor(kind)),
        LowerCallTarget::Builtin(builtin) => Ok(CallTarget::Builtin(builtin)),
    }
}

fn lower_call_arg(arg: &crate::ast::CallArg, facts: &mut LowerFacts) -> AliasResult<CallArg> {
    Ok(CallArg {
        value: lower_expr(&arg.value, facts)?,
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
    })
}
