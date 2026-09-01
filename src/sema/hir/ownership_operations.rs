//! Final-HIR ownership operations for assignment.
//!
//! Value categories describe what an expression produced; this owner resolves how a destination
//! consumes that value. Ownership flow and codegen must consume the frozen operation rather than
//! independently reconstructing transfer/rebind behavior from expression or target shape.

use super::{
    ArmBody, AssignmentOperation, Binding, BindingId, Body, CheckedProgram, Expr, ExprCategory,
    Item, OwnershipCapability, OwningWrite, Place, Stmt, StorageRelation, StrPart, ValueCategory,
};
use crate::{AliasError, AliasResult, Span};
use std::collections::HashMap;

fn invariant(span: Span, msg: impl Into<String>) -> AliasError {
    AliasError {
        msg: format!("内部 sema 不变式被破坏: {}", msg.into()),
        span,
    }
}

fn provisional_owning_write(value: &Expr) -> AliasResult<Option<OwningWrite>> {
    Ok(match (value.category(), value.ownership_capability()) {
        (
            Some(ExprCategory::Value(ValueCategory::InlineValue)),
            Some(OwnershipCapability::None),
        ) => Some(OwningWrite::InlineCopy),
        (
            Some(ExprCategory::Value(ValueCategory::OwnedTemporary)),
            Some(OwnershipCapability::Available),
        ) => Some(OwningWrite::OwnershipTransfer),
        (Some(ExprCategory::Place | ExprCategory::Value(ValueCategory::General)), None) => None,
        _ => {
            return Err(invariant(
                value.span(),
                "owning write RHS 缺少 inline-copy 或 ownership-transfer 事实",
            ))
        }
    })
}

fn assignment_operation(
    target: &Place,
    value: &Expr,
    relations: &HashMap<BindingId, StorageRelation>,
) -> AliasResult<AssignmentOperation> {
    let rebinds_alias = matches!(
        target,
        Place::Local { binding_id, .. }
            if relations.get(binding_id) == Some(&StorageRelation::Borrowed)
    );
    assignment_operation_for(value, rebinds_alias)
}

pub(super) fn assignment_operation_for(
    value: &Expr,
    rebinds_alias: bool,
) -> AliasResult<AssignmentOperation> {
    provisional_assignment_operation_for(value, rebinds_alias)?.ok_or_else(|| {
        invariant(
            value.span(),
            "Assignment operation 仍依赖 unresolved Place/General",
        )
    })
}

pub(super) fn provisional_assignment_operation_for(
    value: &Expr,
    rebinds_alias: bool,
) -> AliasResult<Option<AssignmentOperation>> {
    if rebinds_alias {
        if value.category() != Some(ExprCategory::Value(ValueCategory::BorrowedValue))
            || value.ownership_capability() != Some(OwnershipCapability::None)
        {
            if matches!(
                (value.category(), value.ownership_capability()),
                (
                    Some(ExprCategory::Place | ExprCategory::Value(ValueCategory::General)),
                    None
                )
            ) {
                return Ok(None);
            }
            return Err(invariant(
                value.span(),
                "borrowed alias rebind RHS 缺少 BorrowedValue 事实",
            ));
        }
        Ok(Some(AssignmentOperation::RebindBorrowedAlias))
    } else {
        Ok(provisional_owning_write(value)?.map(AssignmentOperation::Replace))
    }
}

pub(super) fn finalize(program: &mut CheckedProgram) -> AliasResult<()> {
    let relations = collect_binding_relations(program)?;
    let mut stack = root_mut_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            MutNode::Binding(binding) => {
                stack.push(MutNode::Expr(&mut binding.value));
            }
            MutNode::Stmt(stmt) => {
                if let Stmt::Assign {
                    target,
                    value,
                    operation,
                } = stmt
                {
                    if operation.is_some() {
                        return Err(invariant(
                            target.span(),
                            "Assignment operation 被重复 finalization",
                        ));
                    }
                    *operation = Some(assignment_operation(target, value, &relations)?);
                }
                push_mut_stmt_children(&mut stack, stmt);
            }
            MutNode::Expr(expr) => push_mut_expr_children(&mut stack, expr),
        }
    }
    Ok(())
}

pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    let relations = collect_binding_relations(program)?;
    let mut stack = root_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            Node::Binding(binding) => {
                stack.push(Node::Expr(&binding.value));
            }
            Node::Stmt(stmt) => {
                if let Stmt::Assign {
                    target,
                    value,
                    operation,
                } = stmt
                {
                    let expected = assignment_operation(target, value, &relations)?;
                    if *operation != Some(expected) {
                        return Err(invariant(
                            target.span(),
                            "Assignment operation 与 resolved value operation 漂移",
                        ));
                    }
                }
                push_stmt_children(&mut stack, stmt);
            }
            Node::Expr(expr) => push_expr_children(&mut stack, expr),
        }
    }
    Ok(())
}

fn collect_binding_relations(
    program: &CheckedProgram,
) -> AliasResult<HashMap<BindingId, StorageRelation>> {
    let mut relations = HashMap::new();
    let mut stack = root_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            Node::Binding(binding) => {
                if let Some(relation) = binding.relation {
                    if relations.insert(binding.binding_id, relation).is_some() {
                        return Err(invariant(
                            binding.span,
                            "重复 BindingId 进入 assignment operation",
                        ));
                    }
                }
                stack.push(Node::Expr(&binding.value));
            }
            Node::Stmt(stmt) => push_stmt_children(&mut stack, stmt),
            Node::Expr(expr) => push_expr_children(&mut stack, expr),
        }
    }
    Ok(relations)
}

enum Node<'a> {
    Binding(&'a Binding),
    Stmt(&'a Stmt),
    Expr(&'a Expr),
}

enum MutNode<'a> {
    Binding(&'a mut Binding),
    Stmt(&'a mut Stmt),
    Expr(&'a mut Expr),
}

fn root_nodes(program: &CheckedProgram) -> Vec<Node<'_>> {
    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => stack.push(Node::Binding(binding)),
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
            Item::Binding(binding) => stack.push(MutNode::Binding(binding)),
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
        Stmt::Binding(binding) => stack.push(Node::Binding(binding)),
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
        Stmt::Binding(binding) => stack.push(MutNode::Binding(binding)),
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

fn push_match_children<'a>(
    stack: &mut Vec<Node<'a>>,
    subject: &'a Expr,
    arms: &'a [super::MatchArm],
) {
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

fn push_mut_match_children<'a>(
    stack: &mut Vec<MutNode<'a>>,
    subject: &'a mut Expr,
    arms: &'a mut [super::MatchArm],
) {
    for arm in arms.iter_mut().rev() {
        match &mut arm.body {
            ArmBody::Block(stmts) => {
                for stmt in stmts.iter_mut().rev() {
                    stack.push(MutNode::Stmt(stmt));
                }
            }
            ArmBody::Value(value) | ArmBody::Ret(value) => stack.push(MutNode::Expr(value)),
        }
    }
    stack.push(MutNode::Expr(subject));
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
        Expr::Match { subject, arms, .. } => push_match_children(stack, subject, arms),
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
        Expr::Match { subject, arms, .. } => push_mut_match_children(stack, subject, arms),
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
