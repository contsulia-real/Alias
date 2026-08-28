use super::{
    ArmBody, BindingId, Body, CallTarget, CheckedProgram, Expr, Item, Stmt, StrPart,
};
use crate::sema::types::Ty;
use crate::{AliasError, AliasResult};
use std::collections::HashSet;

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

fn push_match_children<'a>(
    stack: &mut Vec<HirValidationNode<'a>>,
    subject: &'a Expr,
    arms: &'a [super::MatchArm],
) {
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

fn push_expr_children<'a>(stack: &mut Vec<HirValidationNode<'a>>, expr: &'a Expr) {
    match expr {
        Expr::Str(parts, ..) => {
            for part in parts.iter().rev() {
                if let StrPart::Hole(hole) = part {
                    stack.push(HirValidationNode::Expr(hole));
                }
            }
        }
        Expr::Cast { expr, .. }
        | Expr::Convert { expr, .. }
        | Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Propagate { expr, .. } => stack.push(HirValidationNode::Expr(expr)),
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
        Expr::Call { callee, args, .. } => {
            for arg in args.iter().rev() {
                stack.push(HirValidationNode::Expr(&arg.value));
            }
            if matches!(expr, Expr::Call { target: CallTarget::FunctionValue, .. }) {
                stack.push(HirValidationNode::Expr(callee));
            }
        }
        Expr::MethodCall { recv, args, .. } => {
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
        Expr::Match { subject, arms, .. } => push_match_children(stack, subject, arms),
        Expr::FuncLit { body, .. } => push_validation_body(stack, body),
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..)
        | Expr::Typeof { .. } => {}
    }
}

fn collect_declared_ids(program: &CheckedProgram) -> HashSet<BindingId> {
    let mut ids = HashSet::new();
    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => {
                ids.insert(binding.binding_id);
                if let super::BindingOwner::Method { self_id, .. } = &binding.owner {
                    ids.insert(*self_id);
                }
                stack.push(HirValidationNode::Expr(&binding.value));
            }
            Item::StructDef(def) => {
                for field in def.fields.iter().rev() {
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
                match expr {
                    Expr::FuncLit {
                        params,
                        implicit_bindings,
                        ..
                    } => {
                        ids.extend(params.iter().map(|param| param.binding_id));
                        ids.extend(implicit_bindings.iter().copied());
                    }
                    Expr::Match { arms, .. } => {
                        ids.extend(arms.iter().filter_map(|arm| arm.binding_id));
                    }
                    _ => {}
                }
                push_expr_children(&mut stack, expr);
            }
            HirValidationNode::Stmt(stmt) => {
                match stmt {
                    Stmt::Binding(binding) => {
                        ids.insert(binding.binding_id);
                    }
                    Stmt::For { binding_id, .. } => {
                        ids.insert(*binding_id);
                    }
                    _ => {}
                }
                push_stmt_children(&mut stack, stmt);
            }
        }
    }
    ids
}

fn push_stmt_children<'a>(stack: &mut Vec<HirValidationNode<'a>>, stmt: &'a Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(HirValidationNode::Expr(&binding.value)),
        Stmt::Assign { value, .. } => stack.push(HirValidationNode::Expr(value)),
        Stmt::FieldAssign { recv, value, .. } => {
            stack.push(HirValidationNode::Expr(value));
            stack.push(HirValidationNode::Expr(recv));
        }
        Stmt::Expr { expr } => stack.push(HirValidationNode::Expr(expr)),
        Stmt::Return { value } => {
            if let Some(value) = value {
                stack.push(HirValidationNode::Expr(value));
            }
        }
        Stmt::If {
            branches,
            else_body,
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
        Stmt::While { cond, body } => {
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
        Stmt::Break | Stmt::Continue => {}
    }
}

fn collect_function_locals(expr: &Expr) -> HashSet<BindingId> {
    let Expr::FuncLit {
        params,
        implicit_bindings,
        body,
        ..
    } = expr
    else {
        return HashSet::new();
    };
    let mut locals: HashSet<BindingId> = params.iter().map(|param| param.binding_id).collect();
    locals.extend(implicit_bindings.iter().copied());
    let mut stack = Vec::new();
    push_validation_body(&mut stack, body);
    while let Some(node) = stack.pop() {
        match node {
            HirValidationNode::Expr(child) => match child {
                // Nested functions own their own locals and are deliberately not descended here.
                Expr::FuncLit { .. } => {}
                Expr::Match { subject, arms, .. } => {
                    locals.extend(arms.iter().filter_map(|arm| arm.binding_id));
                    push_match_children(&mut stack, subject, arms);
                }
                _ => push_expr_children(&mut stack, child),
            },
            HirValidationNode::Stmt(stmt) => {
                match stmt {
                    Stmt::Binding(binding) => {
                        locals.insert(binding.binding_id);
                    }
                    Stmt::For { binding_id, .. } => {
                        locals.insert(*binding_id);
                    }
                    _ => {}
                }
                push_stmt_children(&mut stack, stmt);
            }
        }
    }
    locals
}

pub(super) fn validate_resolved_hir(program: &CheckedProgram) -> AliasResult<()> {
    // The source is untrusted and nesting is bounded but nontrivial; validation uses explicit
    // stacks so the final authority gate itself does not reintroduce host-recursion risk.
    let known_ids = collect_declared_ids(program);
    if !known_ids.contains(&program.main_id) {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: main BindingId 不存在".into(),
            span: crate::Span::default(),
        });
    }
    let globals: HashSet<BindingId> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Binding(binding) if !binding.is_method() => Some(binding.binding_id),
            _ => None,
        })
        .collect();

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
                    Expr::Ident(_, Some(id), ..) => {
                        if !known_ids.contains(id) {
                            return Err(AliasError {
                                msg: format!("内部 sema 不变式被破坏: Ident 引用未知 BindingId {id:?}"),
                                span: expr.span(),
                            });
                        }
                    }
                    Expr::Ident(name, None, ..) => {
                        return Err(AliasError {
                            msg: format!("内部 sema 不变式被破坏: 可求值标识符 '{name}' 缺少 BindingId"),
                            span: expr.span(),
                        });
                    }
                    Expr::Typeof { type_name, .. } => {
                        if expr.ty() != &Ty::Str || type_name.is_empty() {
                            return Err(AliasError {
                                msg: "内部 sema 不变式被破坏: typeof 未固化 string 类型名".into(),
                                span: expr.span(),
                            });
                        }
                    }
                    Expr::Convert {
                        expr: inner,
                        mode: super::ResolvedConversion::Identity,
                        ..
                    } if inner.ty() != expr.ty() => {
                        return Err(AliasError {
                            msg: "内部 sema 不变式被破坏: Identity 转换改变了静态类型".into(),
                            span: expr.span(),
                        });
                    }
                    Expr::Call {
                        callee,
                        args,
                        target,
                        ..
                    } => {
                        match target {
                            CallTarget::FunctionValue => {
                                if matches!(callee.as_ref(), Expr::Ident(_, None, ..)) {
                                    return Err(AliasError {
                                        msg: "内部 sema 不变式被破坏: 函数值 callee 缺少 BindingId"
                                            .into(),
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
                        if let CallTarget::StructConstructor {
                            arg_field_indices, ..
                        } = target
                        {
                            if arg_field_indices.len() != args.len() {
                                return Err(AliasError {
                                    msg: "内部 sema 不变式被破坏: 构造器实参与字段索引数量不一致"
                                        .into(),
                                    span: expr.span(),
                                });
                            }
                        }
                        for arg in args.iter().rev() {
                            stack.push(HirValidationNode::Expr(&arg.value));
                        }
                    }
                    Expr::FuncLit {
                        captures,
                        params,
                        implicit_bindings,
                        body,
                        ..
                    } => {
                        let locals = collect_function_locals(expr);
                        let mut seen = HashSet::new();
                        for id in captures {
                            if !seen.insert(*id) {
                                return Err(AliasError {
                                    msg: format!("内部 sema 不变式被破坏: capture 重复 {id:?}"),
                                    span: expr.span(),
                                });
                            }
                            if !known_ids.contains(id) {
                                return Err(AliasError {
                                    msg: format!("内部 sema 不变式被破坏: capture 引用未知 BindingId {id:?}"),
                                    span: expr.span(),
                                });
                            }
                            if globals.contains(id) {
                                return Err(AliasError {
                                    msg: format!("内部 sema 不变式被破坏: 全局 BindingId {id:?} 不应进入 capture"),
                                    span: expr.span(),
                                });
                            }
                            if locals.contains(id) {
                                return Err(AliasError {
                                    msg: format!("内部 sema 不变式被破坏: 函数自身 local {id:?} 不应进入 capture"),
                                    span: expr.span(),
                                });
                            }
                        }
                        let mut declared_here = HashSet::new();
                        for id in params
                            .iter()
                            .map(|param| param.binding_id)
                            .chain(implicit_bindings.iter().copied())
                        {
                            if !declared_here.insert(id) {
                                return Err(AliasError {
                                    msg: format!("内部 sema 不变式被破坏: 函数入口 BindingId 重复 {id:?}"),
                                    span: expr.span(),
                                });
                            }
                        }
                        push_validation_body(&mut stack, body);
                    }
                    Expr::Str(parts, ..) => {
                        for part in parts.iter().rev() {
                            if let StrPart::Hole(hole) = part {
                                stack.push(HirValidationNode::Expr(hole));
                            }
                        }
                    }
                    Expr::Cast { expr, .. }
                    | Expr::Convert { expr, .. }
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
                    Expr::MethodCall { recv, args, .. } => {
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
                    Expr::Match { subject, arms, .. } => {
                        for arm in arms.iter().rev() {
                            if let Some(id) = arm.binding_id {
                                if !known_ids.contains(&id) {
                                    return Err(AliasError {
                                        msg: format!("内部 sema 不变式被破坏: Pattern 引用未知 BindingId {id:?}"),
                                        span: arm.pattern.span(),
                                    });
                                }
                            }
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
                    Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::This(..) => {}
                }
            }
            HirValidationNode::Stmt(stmt) => match stmt {
                Stmt::Binding(binding) => stack.push(HirValidationNode::Expr(&binding.value)),
                Stmt::Assign { target_id, value } => {
                    if !known_ids.contains(target_id) {
                        return Err(AliasError {
                            msg: format!("内部 sema 不变式被破坏: Assign 引用未知 BindingId {target_id:?}"),
                            span: value.span(),
                        });
                    }
                    stack.push(HirValidationNode::Expr(value));
                }
                Stmt::FieldAssign { recv, value, .. } => {
                    stack.push(HirValidationNode::Expr(value));
                    stack.push(HirValidationNode::Expr(recv));
                }
                Stmt::Expr { expr } => stack.push(HirValidationNode::Expr(expr)),
                Stmt::Return { value } => {
                    if let Some(value) = value {
                        stack.push(HirValidationNode::Expr(value));
                    }
                }
                Stmt::If {
                    branches,
                    else_body,
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
                Stmt::While { cond, body } => {
                    for stmt in body.iter().rev() {
                        stack.push(HirValidationNode::Stmt(stmt));
                    }
                    stack.push(HirValidationNode::Expr(cond));
                }
                Stmt::For {
                    binding_id,
                    ty,
                    iterable,
                    body,
                    span,
                } => {
                    if !known_ids.contains(binding_id) || ty.contains_unknown() {
                        return Err(AliasError {
                            msg: "内部 sema 不变式被破坏: for BindingId/类型未解析".into(),
                            span: *span,
                        });
                    }
                    for stmt in body.iter().rev() {
                        stack.push(HirValidationNode::Stmt(stmt));
                    }
                    stack.push(HirValidationNode::Expr(iterable));
                }
                Stmt::Break | Stmt::Continue => {}
            },
        }
    }
    Ok(())
}
