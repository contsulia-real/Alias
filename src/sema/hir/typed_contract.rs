use super::{ArmBody, Body, CheckedProgram, Expr, Item, Place, ResolvedConversion, Stmt, StrPart};
use crate::sema::exprs::{binary_result_type, conversion_exists};
use crate::sema::types::{types_match, IntW, Ty};
use crate::{AliasError, AliasResult, Span};

enum Node<'a> {
    Expr(&'a Expr),
    Stmt(&'a Stmt),
}

fn invariant(span: Span, msg: impl Into<String>) -> AliasError {
    AliasError {
        msg: format!("内部 sema 不变式被破坏: {}", msg.into()),
        span,
    }
}

fn push_body<'a>(stack: &mut Vec<Node<'a>>, body: &'a Body) {
    match body {
        Body::Block(stmts) => {
            for stmt in stmts.iter().rev() {
                stack.push(Node::Stmt(stmt));
            }
        }
        Body::Single(stmt) => stack.push(Node::Stmt(stmt)),
    }
}

fn push_place_expr_children<'a>(stack: &mut Vec<Node<'a>>, place: &'a Place) {
    let mut places = vec![place];
    while let Some(place) = places.pop() {
        match place {
            Place::Local { .. } => {}
            Place::Field { base, .. } => places.push(base),
            Place::Index { base, index, .. } => {
                stack.push(Node::Expr(index));
                places.push(base);
            }
        }
    }
}

fn push_expr_children<'a>(stack: &mut Vec<Node<'a>>, expr: &'a Expr) {
    match expr {
        Expr::Str(parts, ..) => {
            for part in parts.iter().rev() {
                if let StrPart::Hole(hole) = part {
                    stack.push(Node::Expr(hole));
                }
            }
        }
        Expr::Cast { expr, .. }
        | Expr::Convert { expr, .. }
        | Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Propagate { expr, .. } => stack.push(Node::Expr(expr)),
        Expr::Binary { lhs, rhs, .. } => {
            stack.push(Node::Expr(rhs));
            stack.push(Node::Expr(lhs));
        }
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            stack.push(Node::Expr(else_expr));
            stack.push(Node::Expr(then_expr));
            stack.push(Node::Expr(cond));
        }
        Expr::Call {
            callee,
            args,
            target,
            ..
        } => {
            for arg in args.iter().rev() {
                stack.push(Node::Expr(&arg.value));
            }
            if matches!(target, super::CallTarget::FunctionValue) {
                stack.push(Node::Expr(callee));
            }
        }
        Expr::MethodCall { recv, args, .. } => {
            for arg in args.iter().rev() {
                stack.push(Node::Expr(&arg.value));
            }
            stack.push(Node::Expr(recv));
        }
        Expr::Field { recv, .. } => stack.push(Node::Expr(recv)),
        Expr::Index { recv, idx, .. } => {
            stack.push(Node::Expr(idx));
            stack.push(Node::Expr(recv));
        }
        Expr::ArrayLit { elems, .. } => {
            for elem in elems.iter().rev() {
                stack.push(Node::Expr(elem));
            }
        }
        Expr::FuncLit { body, .. } => push_body(stack, body),
        Expr::Match { subject, arms, .. } => {
            for arm in arms.iter().rev() {
                match &arm.body {
                    ArmBody::Block(stmts) => {
                        for stmt in stmts.iter().rev() {
                            stack.push(Node::Stmt(stmt));
                        }
                    }
                    ArmBody::Value(value) | ArmBody::Ret(value) => stack.push(Node::Expr(value)),
                }
            }
            stack.push(Node::Expr(subject));
        }
        Expr::ReadPlace { source, .. }
        | Expr::Borrow { source, .. }
        | Expr::Move { source, .. } => push_place_expr_children(stack, source),
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..)
        | Expr::Typeof { .. } => {}
    }
}

fn push_stmt_children<'a>(stack: &mut Vec<Node<'a>>, stmt: &'a Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(Node::Expr(&binding.value)),
        Stmt::Assign { target, value, .. } => {
            stack.push(Node::Expr(value));
            push_place_expr_children(stack, target);
        }
        Stmt::Expr { expr } => stack.push(Node::Expr(expr)),
        Stmt::Return { value } => {
            if let Some(value) = value {
                stack.push(Node::Expr(value));
            }
        }
        Stmt::If {
            branches,
            else_body,
        } => {
            if let Some(body) = else_body {
                for stmt in body.iter().rev() {
                    stack.push(Node::Stmt(stmt));
                }
            }
            for (cond, body) in branches.iter().rev() {
                for stmt in body.iter().rev() {
                    stack.push(Node::Stmt(stmt));
                }
                stack.push(Node::Expr(cond));
            }
        }
        Stmt::While { cond, body } => {
            for stmt in body.iter().rev() {
                stack.push(Node::Stmt(stmt));
            }
            stack.push(Node::Expr(cond));
        }
        Stmt::For { iterable, body, .. } => {
            for stmt in body.iter().rev() {
                stack.push(Node::Stmt(stmt));
            }
            stack.push(Node::Expr(iterable));
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn root_nodes(program: &CheckedProgram) -> Vec<Node<'_>> {
    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => stack.push(Node::Expr(&binding.value)),
            Item::StructDef(def) => {
                for field in def.fields.iter().rev() {
                    if let Some(default) = &field.default {
                        stack.push(Node::Expr(default));
                    }
                }
            }
        }
    }
    stack
}

fn validate_expr(expr: &Expr) -> AliasResult<()> {
    match expr {
        Expr::Int(..) => {
            if !matches!(expr.ty(), Ty::Int(_) | Ty::UInt(_)) {
                return Err(invariant(expr.span(), "整数字面量 HIR 类型不是整数"));
            }
        }
        Expr::Float(..) => {
            if !matches!(expr.ty(), Ty::Float(_)) {
                return Err(invariant(expr.span(), "浮点字面量 HIR 类型不是浮点"));
            }
        }
        Expr::Bool(..) => {
            if expr.ty() != &Ty::Bool {
                return Err(invariant(expr.span(), "bool 字面量 HIR 类型不是 bool"));
            }
        }
        Expr::Str(..) => {
            if expr.ty() != &Ty::Str {
                return Err(invariant(expr.span(), "string 字面量 HIR 类型不是 string"));
            }
        }
        Expr::This(..) => {
            if !matches!(expr.ty(), Ty::Func { .. }) {
                return Err(invariant(expr.span(), "this HIR 类型不是完整函数类型"));
            }
        }
        Expr::Cast { expr: inner, .. } => {
            if !conversion_exists(inner.ty(), expr.ty()) {
                return Err(invariant(
                    expr.span(),
                    "Cast 不符合 canonical conversion contract",
                ));
            }
        }
        Expr::Convert {
            expr: inner, mode, ..
        } => match mode {
            ResolvedConversion::Identity => {
                if !types_match(inner.ty(), expr.ty()) {
                    return Err(invariant(expr.span(), "Identity 转换改变了静态类型"));
                }
            }
            ResolvedConversion::Convert => {
                if !conversion_exists(inner.ty(), expr.ty()) {
                    return Err(invariant(
                        expr.span(),
                        "resolved Convert 不符合 canonical conversion contract",
                    ));
                }
            }
        },
        Expr::Binary { op, lhs, rhs, .. } => {
            let result =
                binary_result_type(*op, lhs.ty(), rhs.ty(), expr.span()).map_err(|_| {
                    invariant(
                        expr.span(),
                        "Binary operands 不符合 canonical operator contract",
                    )
                })?;
            if !types_match(&result, expr.ty()) {
                return Err(invariant(
                    expr.span(),
                    "Binary HIR 结果类型与 canonical operator contract 不一致",
                ));
            }
        }
        Expr::Neg { expr: inner, .. } => {
            if !matches!(inner.ty(), Ty::Int(_) | Ty::Float(_))
                || !types_match(inner.ty(), expr.ty())
            {
                return Err(invariant(expr.span(), "Neg HIR 操作数/结果类型不一致"));
            }
        }
        Expr::Not { expr: inner, .. } => {
            if inner.ty() != &Ty::Bool || expr.ty() != &Ty::Bool {
                return Err(invariant(expr.span(), "Not HIR 必须是 bool → bool"));
            }
        }
        Expr::BitNot { expr: inner, .. } => {
            if !matches!(inner.ty(), Ty::Int(_) | Ty::UInt(_))
                || !types_match(inner.ty(), expr.ty())
            {
                return Err(invariant(expr.span(), "BitNot HIR 操作数/结果类型不一致"));
            }
        }
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            if cond.ty() != &Ty::Bool
                || !types_match(then_expr.ty(), expr.ty())
                || !types_match(else_expr.ty(), expr.ty())
            {
                return Err(invariant(expr.span(), "Ternary HIR 条件/分支类型不一致"));
            }
        }
        Expr::Index { recv, idx, .. } => {
            let Ty::Array(elem) = recv.ty() else {
                return Err(invariant(expr.span(), "Index HIR 接收者不是 array"));
            };
            if idx.ty() != &Ty::Int(IntW::W32) || !types_match(elem, expr.ty()) {
                return Err(invariant(expr.span(), "Index HIR 下标/结果类型不一致"));
            }
        }
        Expr::ArrayLit { elems, .. } => {
            let Ty::Array(elem) = expr.ty() else {
                return Err(invariant(expr.span(), "ArrayLit HIR 结果类型不是 array"));
            };
            if elems.iter().any(|item| !types_match(elem, item.ty())) {
                return Err(invariant(
                    expr.span(),
                    "ArrayLit 元素类型与数组元素类型不一致",
                ));
            }
        }
        Expr::Propagate { expr: inner, .. } => {
            let Ty::Result(ok, _) = inner.ty() else {
                return Err(invariant(expr.span(), "Propagate HIR 操作数不是 result"));
            };
            if !types_match(ok, expr.ty()) {
                return Err(invariant(
                    expr.span(),
                    "Propagate HIR 结果类型与 ok payload 不一致",
                ));
            }
        }
        Expr::ReadPlace { source, .. } => {
            if !types_match(source.ty(), expr.ty()) {
                return Err(invariant(
                    expr.span(),
                    "Place read source/result 类型不一致",
                ));
            }
        }
        Expr::Borrow { source, .. } => {
            if !types_match(source.ty(), expr.ty()) {
                return Err(invariant(expr.span(), "Borrow source/result 类型不一致"));
            }
        }
        Expr::Move { source, .. } => {
            if !types_match(source.ty(), expr.ty()) {
                return Err(invariant(expr.span(), "Move source/result 类型不一致"));
            }
            if !matches!(source.as_ref(), Place::Local { .. }) {
                return Err(invariant(expr.span(), "Move source 不是完整 local Place"));
            }
        }
        Expr::Call { .. }
        | Expr::MethodCall { .. }
        | Expr::Field { .. }
        | Expr::FuncLit { .. }
        | Expr::Match { .. }
        | Expr::Ident(..)
        | Expr::Typeof { .. } => {}
    }
    Ok(())
}

/// 只验证 Place projection 自己的局部类型方程。字段索引对应哪个声明字段由 cross-reference
/// validator 拥有；BindingId 是否存在/可写则由 binding contract 拥有。
fn validate_place(place: &Place) -> AliasResult<()> {
    let mut stack = vec![place];
    while let Some(place) = stack.pop() {
        if place.ty().contains_unknown() {
            return Err(invariant(place.span(), "Place 类型未确定"));
        }
        match place {
            Place::Local { .. } => {}
            Place::Field { base, .. } => {
                if !matches!(base.ty(), Ty::Struct(_)) {
                    return Err(invariant(place.span(), "Field Place base 不是 struct"));
                }
                stack.push(base);
            }
            Place::Index { base, index, .. } => {
                let Ty::Array(elem) = base.ty() else {
                    return Err(invariant(place.span(), "Index Place base 不是 array"));
                };
                if index.ty() != &Ty::Int(IntW::W32) || !types_match(elem, place.ty()) {
                    return Err(invariant(place.span(), "Index Place 下标/结果类型不一致"));
                }
                stack.push(base);
            }
        }
    }
    Ok(())
}

fn validate_stmt(stmt: &Stmt) -> AliasResult<()> {
    match stmt {
        Stmt::Assign { target, value, .. } => {
            validate_place(target)?;
            if !types_match(target.ty(), value.ty()) {
                return Err(invariant(
                    value.span(),
                    "Assign RHS 类型与 Place 类型不一致",
                ));
            }
        }
        Stmt::If { branches, .. } => {
            if let Some((cond, _)) = branches.iter().find(|(cond, _)| cond.ty() != &Ty::Bool) {
                return Err(invariant(cond.span(), "If HIR 条件不是 bool"));
            }
        }
        Stmt::While { cond, .. } if cond.ty() != &Ty::Bool => {
            return Err(invariant(cond.span(), "While HIR 条件不是 bool"));
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    let mut stack = root_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            Node::Expr(expr) => {
                validate_expr(expr)?;
                push_expr_children(&mut stack, expr);
            }
            Node::Stmt(stmt) => {
                validate_stmt(stmt)?;
                push_stmt_children(&mut stack, stmt);
            }
        }
    }
    Ok(())
}
