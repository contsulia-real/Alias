//! Resolved ownership operations for match Pattern bindings.
//!
//! Whole-subject bindings obey the same copy/transfer/clone split as other owning initializers.
//! Constructor payload bindings are reads from a still-live result payload and therefore never
//! move out of that payload. Codegen consumes the frozen operation instead of inspecting Ty or
//! the subject expression shape.

use super::{
    binding_contract::pattern_binding_ty, ArmBody, Body, CheckedProgram, DeepClonePlan, Expr,
    ExprCategory, Item, OwnershipCapability, Pattern, PatternBindingOperation, Place, Stmt,
    StrPart, ValueCategory,
};
use crate::sema::exprs::deep_clone_plan_with;
use crate::sema::types::Ty;
use crate::{AliasError, AliasResult, Span};
use std::collections::HashMap;

enum Node<'a> {
    Expr(&'a Expr),
    Stmt(&'a Stmt),
}

enum MutNode<'a> {
    Expr(&'a mut Expr),
    Stmt(&'a mut Stmt),
}

fn invariant(span: Span, msg: impl Into<String>) -> AliasError {
    AliasError {
        msg: format!("内部 sema 不变式被破坏: {}", msg.into()),
        span,
    }
}

fn struct_fields(program: &CheckedProgram) -> HashMap<String, Vec<Ty>> {
    program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::StructDef(def) => Some((
                def.name.clone(),
                def.fields.iter().map(|field| field.ty.clone()).collect(),
            )),
            Item::Binding(_) => None,
        })
        .collect()
}

fn clone_operation(
    ty: &Ty,
    span: Span,
    fields: &HashMap<String, Vec<Ty>>,
) -> AliasResult<PatternBindingOperation> {
    let plan = deep_clone_plan_with(ty, span, &|name| fields.get(name).cloned())?;
    Ok(if plan == DeepClonePlan::Inline {
        PatternBindingOperation::InlineCopy
    } else {
        PatternBindingOperation::DeepClone(plan)
    })
}

fn expected_operation(
    subject: &Expr,
    pattern: &Pattern,
    has_binding_id: bool,
    fields: &HashMap<String, Vec<Ty>>,
) -> AliasResult<Option<PatternBindingOperation>> {
    let binding_ty = pattern_binding_ty(subject.ty(), pattern)?;
    if binding_ty.is_some() != has_binding_id {
        return Err(invariant(
            pattern.span(),
            "Pattern binding type 与 BindingId 存在性不一致",
        ));
    }
    let Some(binding_ty) = binding_ty else {
        return Ok(None);
    };

    if matches!(pattern, Pattern::Constructor { .. }) {
        return clone_operation(&binding_ty, pattern.span(), fields).map(Some);
    }
    if !matches!(pattern, Pattern::Binding { .. }) {
        return Err(invariant(pattern.span(), "未知的 Pattern binding 形态"));
    }
    if matches!(
        binding_ty,
        Ty::Int(_) | Ty::UInt(_) | Ty::Float(_) | Ty::Bool
    ) {
        return Ok(Some(PatternBindingOperation::InlineCopy));
    }

    match (subject.category(), subject.ownership_capability()) {
        (
            Some(ExprCategory::Value(ValueCategory::OwnedTemporary)),
            Some(OwnershipCapability::Available),
        ) => Ok(Some(PatternBindingOperation::OwnershipTransfer)),
        (Some(ExprCategory::Place), None)
        | (
            Some(ExprCategory::Value(ValueCategory::BorrowedValue)),
            Some(OwnershipCapability::None),
        ) => clone_operation(&binding_ty, pattern.span(), fields).map(Some),
        _ => Err(AliasError {
            msg: format!(
                "Pattern binding 无法确定 {} 的 copy/clone/transfer ownership effect",
                binding_ty.name()
            ),
            span: pattern.span(),
        }),
    }
}

pub(super) fn finalize(program: &mut CheckedProgram) -> AliasResult<()> {
    let fields = struct_fields(program);
    let mut stack = root_mut_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            MutNode::Expr(expr) => {
                if let Expr::Match { subject, arms, .. } = expr {
                    for arm in arms.iter_mut() {
                        if arm.binding_operation.is_some() {
                            return Err(invariant(
                                arm.pattern.span(),
                                "Pattern binding operation 被重复 finalization",
                            ));
                        }
                        arm.binding_operation = expected_operation(
                            subject,
                            &arm.pattern,
                            arm.binding_id.is_some(),
                            &fields,
                        )?;
                    }
                }
                push_mut_expr_children(&mut stack, expr);
            }
            MutNode::Stmt(stmt) => push_mut_stmt_children(&mut stack, stmt),
        }
    }
    Ok(())
}

pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    let fields = struct_fields(program);
    let mut stack = root_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            Node::Expr(expr) => {
                if let Expr::Match { subject, arms, .. } = expr {
                    for arm in arms {
                        let expected = expected_operation(
                            subject,
                            &arm.pattern,
                            arm.binding_id.is_some(),
                            &fields,
                        )?;
                        if arm.binding_operation != expected {
                            return Err(invariant(
                                arm.pattern.span(),
                                "Pattern binding operation 与 resolved source 不一致",
                            ));
                        }
                    }
                }
                push_expr_children(&mut stack, expr);
            }
            Node::Stmt(stmt) => push_stmt_children(&mut stack, stmt),
        }
    }
    Ok(())
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

fn root_mut_nodes(program: &mut CheckedProgram) -> Vec<MutNode<'_>> {
    let mut stack = Vec::new();
    for item in program.items.iter_mut().rev() {
        match item {
            Item::Binding(binding) => stack.push(MutNode::Expr(&mut binding.value)),
            Item::StructDef(def) => {
                for field in def.fields.iter_mut().rev() {
                    if let Some(default) = &mut field.default {
                        stack.push(MutNode::Expr(default));
                    }
                }
            }
        }
    }
    stack
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

fn push_mut_body<'a>(stack: &mut Vec<MutNode<'a>>, body: &'a mut Body) {
    match body {
        Body::Block(stmts) => {
            for stmt in stmts.iter_mut().rev() {
                stack.push(MutNode::Stmt(stmt));
            }
        }
        Body::Single(stmt) => stack.push(MutNode::Stmt(stmt)),
    }
}

fn push_place_children<'a>(stack: &mut Vec<Node<'a>>, place: &'a Place) {
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

fn push_mut_place_children<'a>(stack: &mut Vec<MutNode<'a>>, place: &'a mut Place) {
    let mut places = vec![place];
    while let Some(place) = places.pop() {
        match place {
            Place::Local { .. } => {}
            Place::Field { base, .. } => places.push(base),
            Place::Index { base, index, .. } => {
                stack.push(MutNode::Expr(index));
                places.push(base);
            }
        }
    }
}

fn push_stmt_children<'a>(stack: &mut Vec<Node<'a>>, stmt: &'a Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(Node::Expr(&binding.value)),
        Stmt::Assign { target, value, .. } => {
            stack.push(Node::Expr(value));
            push_place_children(stack, target);
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

fn push_mut_stmt_children<'a>(stack: &mut Vec<MutNode<'a>>, stmt: &'a mut Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(MutNode::Expr(&mut binding.value)),
        Stmt::Assign { target, value, .. } => {
            stack.push(MutNode::Expr(value));
            push_mut_place_children(stack, target);
        }
        Stmt::Expr { expr } => stack.push(MutNode::Expr(expr)),
        Stmt::Return { value } => {
            if let Some(value) = value {
                stack.push(MutNode::Expr(value));
            }
        }
        Stmt::If {
            branches,
            else_body,
        } => {
            if let Some(body) = else_body {
                for stmt in body.iter_mut().rev() {
                    stack.push(MutNode::Stmt(stmt));
                }
            }
            for (cond, body) in branches.iter_mut().rev() {
                for stmt in body.iter_mut().rev() {
                    stack.push(MutNode::Stmt(stmt));
                }
                stack.push(MutNode::Expr(cond));
            }
        }
        Stmt::While { cond, body } => {
            for stmt in body.iter_mut().rev() {
                stack.push(MutNode::Stmt(stmt));
            }
            stack.push(MutNode::Expr(cond));
        }
        Stmt::For { iterable, body, .. } => {
            for stmt in body.iter_mut().rev() {
                stack.push(MutNode::Stmt(stmt));
            }
            stack.push(MutNode::Expr(iterable));
        }
        Stmt::Break | Stmt::Continue => {}
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
        Expr::Call { callee, args, .. } => {
            for arg in args.iter().rev() {
                stack.push(Node::Expr(&arg.value));
            }
            stack.push(Node::Expr(callee));
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
        | Expr::Move { source, .. } => push_place_children(stack, source),
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..)
        | Expr::Typeof { .. } => {}
    }
}

fn push_mut_expr_children<'a>(stack: &mut Vec<MutNode<'a>>, expr: &'a mut Expr) {
    match expr {
        Expr::Str(parts, ..) => {
            for part in parts.iter_mut().rev() {
                if let StrPart::Hole(hole) = part {
                    stack.push(MutNode::Expr(hole));
                }
            }
        }
        Expr::Cast { expr, .. }
        | Expr::Convert { expr, .. }
        | Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Propagate { expr, .. } => stack.push(MutNode::Expr(expr)),
        Expr::Binary { lhs, rhs, .. } => {
            stack.push(MutNode::Expr(rhs));
            stack.push(MutNode::Expr(lhs));
        }
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            stack.push(MutNode::Expr(else_expr));
            stack.push(MutNode::Expr(then_expr));
            stack.push(MutNode::Expr(cond));
        }
        Expr::Call { callee, args, .. } => {
            for arg in args.iter_mut().rev() {
                stack.push(MutNode::Expr(&mut arg.value));
            }
            stack.push(MutNode::Expr(callee));
        }
        Expr::MethodCall { recv, args, .. } => {
            for arg in args.iter_mut().rev() {
                stack.push(MutNode::Expr(&mut arg.value));
            }
            stack.push(MutNode::Expr(recv));
        }
        Expr::Field { recv, .. } => stack.push(MutNode::Expr(recv)),
        Expr::Index { recv, idx, .. } => {
            stack.push(MutNode::Expr(idx));
            stack.push(MutNode::Expr(recv));
        }
        Expr::ArrayLit { elems, .. } => {
            for elem in elems.iter_mut().rev() {
                stack.push(MutNode::Expr(elem));
            }
        }
        Expr::FuncLit { body, .. } => push_mut_body(stack, body),
        Expr::Match { subject, arms, .. } => {
            for arm in arms.iter_mut().rev() {
                match &mut arm.body {
                    ArmBody::Block(stmts) => {
                        for stmt in stmts.iter_mut().rev() {
                            stack.push(MutNode::Stmt(stmt));
                        }
                    }
                    ArmBody::Value(value) | ArmBody::Ret(value) => {
                        stack.push(MutNode::Expr(value));
                    }
                }
            }
            stack.push(MutNode::Expr(subject));
        }
        Expr::ReadPlace { source, .. }
        | Expr::Borrow { source, .. }
        | Expr::Move { source, .. } => push_mut_place_children(stack, source),
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..)
        | Expr::Typeof { .. } => {}
    }
}
