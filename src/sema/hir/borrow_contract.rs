//! Final-HIR containment rules for borrowed values and alias slots.
//!
//! Loan liveness owns conflict timing. This module owns where a BorrowedValue may be stored or
//! passed at all, so codegen never has to guess whether a machine word is a value or an address.

use super::{
    ArmBody, BindingId, Body, CallTarget, CheckedProgram, Expr, ExprCategory, Item, MethodTarget,
    Place, ReturnPass, Stmt, StorageRelation, StrPart, ValueCategory,
};
use crate::{AliasError, AliasResult, Span};
use std::collections::{HashMap, HashSet};

enum Node<'a> {
    Expr(&'a Expr, bool),
    Stmt(&'a Stmt),
}

fn error(span: Span, msg: impl Into<String>) -> AliasError {
    AliasError {
        msg: msg.into(),
        span,
    }
}

fn push_place_indices<'a>(stack: &mut Vec<Node<'a>>, place: &'a Place) {
    let mut places = vec![place];
    while let Some(place) = places.pop() {
        match place {
            Place::Local { .. } => {}
            Place::Field { base, .. } => places.push(base),
            Place::Index { base, index, .. } => {
                stack.push(Node::Expr(index, false));
                places.push(base);
            }
        }
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

fn collect_relations(program: &CheckedProgram) -> HashMap<BindingId, StorageRelation> {
    let mut relations = HashMap::new();
    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => {
                if let Some(relation) = binding.relation {
                    relations.insert(binding.binding_id, relation);
                }
                stack.push(Node::Expr(&binding.value, false));
            }
            Item::StructDef(def) => {
                for field in def.fields.iter().rev() {
                    if let Some(default) = &field.default {
                        stack.push(Node::Expr(default, false));
                    }
                }
            }
        }
    }
    while let Some(node) = stack.pop() {
        match node {
            Node::Stmt(stmt) => {
                if let Stmt::Binding(binding) = stmt {
                    if let Some(relation) = binding.relation {
                        relations.insert(binding.binding_id, relation);
                    }
                }
                push_stmt_children(&mut stack, stmt, &relations);
            }
            Node::Expr(expr, _) => push_expr_children(&mut stack, expr, false),
        }
    }
    relations
}

fn expr_uses_borrowed_binding(expr: &Expr, borrowed: &HashSet<BindingId>) -> bool {
    let mut stack = vec![Node::Expr(expr, false)];
    while let Some(node) = stack.pop() {
        match node {
            Node::Stmt(stmt) => push_stmt_children(&mut stack, stmt, &HashMap::new()),
            Node::Expr(expr, _) => {
                match expr {
                    Expr::Ident(_, Some(id), ..) if borrowed.contains(id) => return true,
                    Expr::ReadPlace { source, .. }
                    | Expr::Borrow { source, .. }
                    | Expr::Move { source, .. }
                        if borrowed.contains(&source.root_binding_id()) =>
                    {
                        return true;
                    }
                    // A nested function is governed by its capture list at the creation site; its
                    // body must not make an outer expression look like an immediate alias use.
                    Expr::FuncLit { .. } => continue,
                    _ => {}
                }
                push_expr_children(&mut stack, expr, false);
            }
        }
    }
    false
}

fn push_stmt_children<'a>(
    stack: &mut Vec<Node<'a>>,
    stmt: &'a Stmt,
    relations: &HashMap<BindingId, StorageRelation>,
) {
    match stmt {
        Stmt::Binding(binding) => stack.push(Node::Expr(
            &binding.value,
            binding.relation == Some(StorageRelation::Borrowed),
        )),
        Stmt::Assign { target, value, .. } => {
            let borrowed_rebind = matches!(target, Place::Local { binding_id, .. }
                if relations.get(binding_id) == Some(&StorageRelation::Borrowed));
            stack.push(Node::Expr(value, borrowed_rebind));
            push_place_indices(stack, target);
        }
        Stmt::Expr { expr } => stack.push(Node::Expr(expr, false)),
        Stmt::Return { value } => {
            if let Some(value) = value {
                stack.push(Node::Expr(
                    value,
                    matches!(
                        value.info().return_pass.as_deref(),
                        Some(ReturnPass::BorrowValue { .. })
                    ),
                ));
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
                stack.push(Node::Expr(cond, false));
            }
        }
        Stmt::While { cond, body } => {
            for stmt in body.iter().rev() {
                stack.push(Node::Stmt(stmt));
            }
            stack.push(Node::Expr(cond, false));
        }
        Stmt::For { iterable, body, .. } => {
            for stmt in body.iter().rev() {
                stack.push(Node::Stmt(stmt));
            }
            stack.push(Node::Expr(iterable, false));
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn push_expr_children<'a>(stack: &mut Vec<Node<'a>>, expr: &'a Expr, allow_borrowed: bool) {
    match expr {
        Expr::Str(parts, ..) => {
            for part in parts.iter().rev() {
                if let StrPart::Hole(hole) = part {
                    stack.push(Node::Expr(hole, false));
                }
            }
        }
        Expr::Convert {
            expr,
            mode: super::ResolvedConversion::Identity,
            ..
        } => stack.push(Node::Expr(expr, allow_borrowed)),
        Expr::Cast { expr, .. }
        | Expr::Convert { expr, .. }
        | Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Propagate { expr, .. } => stack.push(Node::Expr(expr, false)),
        Expr::Binary { lhs, rhs, .. } => {
            stack.push(Node::Expr(rhs, false));
            stack.push(Node::Expr(lhs, false));
        }
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            stack.push(Node::Expr(else_expr, false));
            stack.push(Node::Expr(then_expr, false));
            stack.push(Node::Expr(cond, false));
        }
        Expr::Call { callee, args, .. } => {
            for arg in args.iter().rev() {
                stack.push(Node::Expr(&arg.value, false));
            }
            stack.push(Node::Expr(callee, false));
        }
        Expr::MethodCall { recv, args, .. } => {
            for arg in args.iter().rev() {
                stack.push(Node::Expr(&arg.value, false));
            }
            stack.push(Node::Expr(recv, false));
        }
        Expr::Field { recv, .. } => stack.push(Node::Expr(recv, false)),
        Expr::Index { recv, idx, .. } => {
            stack.push(Node::Expr(idx, false));
            stack.push(Node::Expr(recv, false));
        }
        Expr::ArrayLit { elems, .. } => {
            for elem in elems.iter().rev() {
                stack.push(Node::Expr(elem, false));
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
                    ArmBody::Value(value) => stack.push(Node::Expr(value, false)),
                    ArmBody::Ret(value) => stack.push(Node::Expr(
                        value,
                        matches!(
                            value.info().return_pass.as_deref(),
                            Some(ReturnPass::BorrowValue { .. })
                        ),
                    )),
                }
            }
            stack.push(Node::Expr(subject, false));
        }
        Expr::ReadPlace { source, .. }
        | Expr::Borrow { source, .. }
        | Expr::Move { source, .. } => push_place_indices(stack, source),
        Expr::Typeof { .. }
        | Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..) => {}
    }
}

pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    let relations = collect_relations(program);
    let borrowed: HashSet<_> = relations
        .iter()
        .filter_map(|(id, relation)| (*relation == StorageRelation::Borrowed).then_some(*id))
        .collect();
    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => {
                if binding.relation == Some(StorageRelation::Borrowed) {
                    return Err(error(
                        binding.span,
                        "borrowed binding 当前不能存入顶层/global storage",
                    ));
                }
                stack.push(Node::Expr(&binding.value, false));
            }
            Item::StructDef(def) => {
                for field in def.fields.iter().rev() {
                    if let Some(default) = &field.default {
                        stack.push(Node::Expr(default, false));
                    }
                }
            }
        }
    }
    while let Some(node) = stack.pop() {
        match node {
            Node::Stmt(stmt) => {
                match stmt {
                    Stmt::Return { value: Some(value) }
                        if super::value_categories::type_carries_dynamic_owner(value.ty())
                            && expr_uses_borrowed_binding(value, &borrowed) =>
                    {
                        return Err(error(
                            value.span(),
                            "borrowed alias return 的 generation forwarding 尚未解析",
                        ));
                    }
                    Stmt::For { iterable, .. }
                        if expr_uses_borrowed_binding(iterable, &borrowed) =>
                    {
                        return Err(error(
                            iterable.span(),
                            "borrowed iterable 的完整循环 loan region 尚未解析",
                        ));
                    }
                    _ => {}
                }
                push_stmt_children(&mut stack, stmt, &relations);
            }
            Node::Expr(expr, allow_borrowed) => {
                if matches!(
                    expr.category(),
                    Some(ExprCategory::Value(ValueCategory::BorrowedValue))
                ) && !allow_borrowed
                {
                    return Err(error(
                        expr.span(),
                        "BorrowedValue 只能初始化或重新绑定 local borrowed slot",
                    ));
                }
                match expr {
                    Expr::Borrow { source, .. }
                        if relations.get(&source.root_binding_id())
                            != Some(&StorageRelation::Owning) =>
                    {
                        if matches!(
                            expr.info().return_pass.as_deref(),
                            Some(ReturnPass::BorrowValue { .. })
                        ) {
                            push_expr_children(&mut stack, expr, allow_borrowed);
                            continue;
                        }
                        return Err(error(
                            expr.span(),
                            "borrow source 必须根植于 owning local Place",
                        ));
                    }
                    Expr::FuncLit { captures, .. }
                        if captures
                            .iter()
                            .any(|capture| borrowed.contains(&capture.binding_id)) =>
                    {
                        return Err(error(
                            expr.span(),
                            "borrowed alias capture 缺少可固化的 referent loan generation",
                        ));
                    }
                    Expr::Call {
                        args,
                        target: CallTarget::FunctionValue,
                        ..
                    } if args
                        .iter()
                        .any(|arg| expr_uses_borrowed_binding(&arg.value, &borrowed)) =>
                    {
                        return Err(error(
                            expr.span(),
                            "borrowed argument 的 referent-loan forwarding 尚未解析",
                        ));
                    }
                    Expr::MethodCall {
                        recv,
                        args,
                        target: MethodTarget::User { .. },
                        ..
                    } if expr_uses_borrowed_binding(recv, &borrowed)
                        || args
                            .iter()
                            .any(|arg| expr_uses_borrowed_binding(&arg.value, &borrowed)) =>
                    {
                        return Err(error(
                            expr.span(),
                            "borrowed receiver/argument 的 referent-loan forwarding 尚未解析",
                        ));
                    }
                    Expr::Match { subject, .. }
                        if expr_uses_borrowed_binding(subject, &borrowed) =>
                    {
                        return Err(error(
                            subject.span(),
                            "borrowed match subject 的跨 arm loan region 尚未解析",
                        ));
                    }
                    _ => {}
                }
                push_expr_children(&mut stack, expr, allow_borrowed);
            }
        }
    }
    Ok(())
}
