use super::*;
use crate::{AliasError, AliasResult};

enum HirValidationNode<'a> {
    Expr(&'a Expr),
    Stmt(&'a Stmt),
}

fn push_validation_body<'a>(stack: &mut Vec<HirValidationNode<'a>>, body: &'a Body) {
    match body {
        Body::Block(stmts) => {
            for stmt in stmts.iter().rev() {
                stack.push(HirValidationNode::Stmt(stmt));
            }
        }
        Body::Single(stmt) => stack.push(HirValidationNode::Stmt(stmt)),
    }
}

pub(super) fn validate_resolved_hir(program: &CheckedProgram) -> AliasResult<()> {
    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => stack.push(HirValidationNode::Expr(&binding.value)),
            Item::StructDef(def) => {
                for field in def.fields.iter().rev() {
                    if field.ty.contains_unknown() {
                        return Err(AliasError {
                            msg: "内部 sema 不变式被破坏: 字段类型未确定".into(),
                            span: field.span,
                        });
                    }
                    if let Some(default) = &field.default {
                        stack.push(HirValidationNode::Expr(default));
                    }
                }
            }
        }
    }

    while let Some(node) = stack.pop() {
        match node {
            HirValidationNode::Expr(expr) => {
                if expr.ty().contains_unknown() {
                    return Err(AliasError {
                        msg: format!(
                            "内部 sema 不变式被破坏: HIR 仍含未确定类型 {}",
                            expr.ty().name()
                        ),
                        span: expr.span(),
                    });
                }
                match expr {
                    Expr::Ident(..) | Expr::This(..) | Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) => {}
                    Expr::Str(parts, ..) => {
                        for part in parts.iter().rev() {
                            if let StrPart::Hole(hole) = part {
                                stack.push(HirValidationNode::Expr(hole));
                            }
                        }
                    }
                    Expr::Cast { expr, .. }
                    | Expr::Neg { expr, .. }
                    | Expr::Not { expr, .. }
                    | Expr::BitNot { expr, .. }
                    | Expr::Propagate { expr, .. } => {
                        stack.push(HirValidationNode::Expr(expr));
                    }
                    Expr::Binary { lhs, rhs, .. } => {
                        stack.push(HirValidationNode::Expr(rhs));
                        stack.push(HirValidationNode::Expr(lhs));
                    }
                    Expr::Ternary {
                        cond,
                        then_expr,
                        else_expr,
                        ..
                    } => {
                        stack.push(HirValidationNode::Expr(else_expr));
                        stack.push(HirValidationNode::Expr(then_expr));
                        stack.push(HirValidationNode::Expr(cond));
                    }
                    Expr::Call {
                        callee,
                        args,
                        info,
                        ..
                    } => {
                        let Some(target) = info.call_target.as_ref() else {
                            return Err(AliasError {
                                msg: "内部 sema 不变式被破坏: HIR Call 缺少 target".into(),
                                span: expr.span(),
                            });
                        };
                        match target {
                            CallTarget::FunctionValue => {
                                if matches!(callee.as_ref(), Expr::Ident(_, None, ..)) {
                                    return Err(AliasError {
                                        msg: "内部 sema 不变式被破坏: 函数值 callee 缺少 BindingId".into(),
                                        span: callee.span(),
                                    });
                                }
                                stack.push(HirValidationNode::Expr(callee));
                            }
                            _ => {
                                if !matches!(callee.as_ref(), Expr::Ident(..)) {
                                    return Err(AliasError {
                                        msg: "内部 sema 不变式被破坏: builtin/constructor callee 非直接名字".into(),
                                        span: callee.span(),
                                    });
                                }
                            }
                        }
                        for arg in args.iter().rev() {
                            stack.push(HirValidationNode::Expr(&arg.value));
                        }
                    }
                    Expr::MethodCall {
                        recv, args, info, ..
                    } => {
                        if matches!(
                            info.call_target,
                            Some(CallTarget::Method(MethodTarget::User { id: None, .. }))
                        ) {
                            return Err(AliasError {
                                msg: "内部 sema 不变式被破坏: HIR 用户方法缺少 MethodId".into(),
                                span: expr.span(),
                            });
                        }
                        for arg in args.iter().rev() {
                            stack.push(HirValidationNode::Expr(&arg.value));
                        }
                        stack.push(HirValidationNode::Expr(recv));
                    }
                    Expr::Field { recv, .. } => stack.push(HirValidationNode::Expr(recv)),
                    Expr::Index { recv, idx, .. } => {
                        stack.push(HirValidationNode::Expr(idx));
                        stack.push(HirValidationNode::Expr(recv));
                    }
                    Expr::ArrayLit { elems, .. } => {
                        for elem in elems.iter().rev() {
                            stack.push(HirValidationNode::Expr(elem));
                        }
                    }
                    Expr::FuncLit { body, .. } => push_validation_body(&mut stack, body),
                    Expr::Match { subject, arms, .. } => {
                        for arm in arms.iter().rev() {
                            match &arm.body {
                                ArmBody::Block(stmts) => {
                                    for stmt in stmts.iter().rev() {
                                        stack.push(HirValidationNode::Stmt(stmt));
                                    }
                                }
                                ArmBody::Value(value) | ArmBody::Ret(value) => {
                                    stack.push(HirValidationNode::Expr(value));
                                }
                            }
                        }
                        stack.push(HirValidationNode::Expr(subject));
                    }
                }
            }
            HirValidationNode::Stmt(stmt) => match stmt {
                Stmt::Binding(binding) => stack.push(HirValidationNode::Expr(&binding.value)),
                Stmt::Assign { value, .. } => stack.push(HirValidationNode::Expr(value)),
                Stmt::FieldAssign { recv, value, .. } => {
                    stack.push(HirValidationNode::Expr(value));
                    stack.push(HirValidationNode::Expr(recv));
                }
                Stmt::ExprStmt { expr, .. } => stack.push(HirValidationNode::Expr(expr)),
                Stmt::Return { value, .. } => {
                    if let Some(value) = value {
                        stack.push(HirValidationNode::Expr(value));
                    }
                }
                Stmt::If {
                    branches,
                    else_body,
                    ..
                } => {
                    if let Some(body) = else_body {
                        for stmt in body.iter().rev() {
                            stack.push(HirValidationNode::Stmt(stmt));
                        }
                    }
                    for (cond, body) in branches.iter().rev() {
                        for stmt in body.iter().rev() {
                            stack.push(HirValidationNode::Stmt(stmt));
                        }
                        stack.push(HirValidationNode::Expr(cond));
                    }
                }
                Stmt::While { cond, body, .. } => {
                    for stmt in body.iter().rev() {
                        stack.push(HirValidationNode::Stmt(stmt));
                    }
                    stack.push(HirValidationNode::Expr(cond));
                }
                Stmt::For { iterable, body, .. } => {
                    for stmt in body.iter().rev() {
                        stack.push(HirValidationNode::Stmt(stmt));
                    }
                    stack.push(HirValidationNode::Expr(iterable));
                }
                Stmt::Break { .. } | Stmt::Continue { .. } => {}
            },
        }
    }
    Ok(())
}
