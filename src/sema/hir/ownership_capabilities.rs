use super::{
    ArmBody, Body, CheckedProgram, Expr, ExprCategory, Item, MatchArm, OwnershipCapability, Place,
    Stmt, StrPart, ValueCategory,
};
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

/// 当前 phase 的 value-category → initial capability 唯一映射 owner。
///
/// InlineValue 明确没有独立 ownership；OwnedTemporary 明确携带唯一可转移 capability。
/// Place capability 与 General value 的后续状态必须由真正的 relation/effect/dataflow 决定，
/// 不能在这里按类型或源码形状猜测。
fn expected_capability(expr: &Expr) -> AliasResult<Option<OwnershipCapability>> {
    let category = expr
        .category()
        .ok_or_else(|| invariant(expr.span(), "ownership capability 缺少 value category 前置事实"))?;
    Ok(match category {
        ExprCategory::Place => None,
        ExprCategory::Value(ValueCategory::InlineValue) => Some(OwnershipCapability::None),
        ExprCategory::Value(ValueCategory::OwnedTemporary) => Some(OwnershipCapability::Available),
        ExprCategory::Value(ValueCategory::General) => None,
    })
}

/// value category 已由 lowering 固化后，立即写入当前可证明的 initial capability fact。
/// `Option::None` 不是 `OwnershipCapability::None`：它表示这个 phase 还没有合法依据给该
/// Place/General Value 分配 capability 状态，后续分析必须显式决定。
pub(super) fn finalize(expr: &mut Expr) -> AliasResult<()> {
    let capability = expected_capability(expr)?;
    expr.info_mut().ownership_capability = capability;
    Ok(())
}

/// final-HIR gate 独立验证 initial capability 与 value category 的关系；这不是 relation 或
/// move/free dataflow 的替代。后续新增消费状态时应扩展 capability owner，而不是让 codegen
/// 根据 value 类型、指针 bit 或 AST 形状重建 capability。
pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    let mut stack = root_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            Node::Expr(expr) => {
                let want = expected_capability(expr)?;
                if expr.ownership_capability() != want {
                    return Err(invariant(
                        expr.span(),
                        "Expr ownership capability 与 resolved HIR value category 不一致",
                    ));
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

fn push_stmt_children<'a>(stack: &mut Vec<Node<'a>>, stmt: &'a Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(Node::Expr(&binding.value)),
        Stmt::Assign { target, value } => {
            stack.push(Node::Expr(value));
            if let Place::Field { recv, .. } = target {
                stack.push(Node::Expr(recv));
            }
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

fn push_match_children<'a>(stack: &mut Vec<Node<'a>>, subject: &'a Expr, arms: &'a [MatchArm]) {
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
        Expr::Typeof { .. }
        | Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..) => {}
    }
}
