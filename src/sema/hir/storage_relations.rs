use super::{
    Body, CheckedProgram, Expr, ExprCategory, Item, Stmt, StorageRelation, ValueCategory,
};
use crate::sema::types::Ty;
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

fn is_inline_type(ty: &Ty) -> bool {
    matches!(ty, Ty::Int(_) | Ty::UInt(_) | Ty::Float(_) | Ty::Bool)
}

/// 当前阶段 initializer → binding slot relation 的唯一 policy owner。
///
/// OwnedTemporary / InlineValue 已能直接建立 owning slot；标量 Place 普通读取等价值复制，
/// 因而也可确定为 Owning。动态 Place 必须等待显式 DeepClone HIR，General Value 必须等待
/// function effect 等后续事实，二者都不能提前猜成 owning。
pub(super) fn initial_relation(value: &Expr) -> AliasResult<Option<StorageRelation>> {
    let category = value
        .category()
        .ok_or_else(|| invariant(value.span(), "storage relation 缺少 value category 前置事实"))?;
    Ok(match category {
        ExprCategory::Value(ValueCategory::InlineValue | ValueCategory::OwnedTemporary) => {
            Some(StorageRelation::Owning)
        }
        ExprCategory::Place if is_inline_type(value.ty()) => Some(StorageRelation::Owning),
        ExprCategory::Place | ExprCategory::Value(ValueCategory::General) => None,
    })
}

fn validate_binding(binding: &super::Binding) -> AliasResult<()> {
    let want = initial_relation(&binding.value)?;
    if binding.relation != want {
        return Err(invariant(
            binding.span,
            "Binding storage relation 与 initializer semantic category 不一致",
        ));
    }
    Ok(())
}

/// 只遍历 binding declarations；relation policy 不拥有 BindingId/type/use validation。
pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    for item in &program.items {
        if let Item::Binding(binding) = item {
            validate_binding(binding)?;
        }
    }

    let mut stack = root_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            Node::Expr(expr) => push_expr_children(&mut stack, expr),
            Node::Stmt(stmt) => {
                if let Stmt::Binding(binding) = stmt {
                    validate_binding(binding)?;
                }
                push_stmt_children(&mut stack, stmt);
            }
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
            if let super::Place::Field { recv, .. } = target {
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

fn push_match_children<'a>(
    stack: &mut Vec<Node<'a>>,
    subject: &'a Expr,
    arms: &'a [super::MatchArm],
) {
    for arm in arms.iter().rev() {
        match &arm.body {
            super::ArmBody::Block(stmts) => {
                for stmt in stmts.iter().rev() {
                    stack.push(Node::Stmt(stmt));
                }
            }
            super::ArmBody::Value(value) | super::ArmBody::Ret(value) => {
                stack.push(Node::Expr(value));
            }
        }
    }
    stack.push(Node::Expr(subject));
}

fn push_expr_children<'a>(stack: &mut Vec<Node<'a>>, expr: &'a Expr) {
    match expr {
        Expr::Str(parts, ..) => {
            for part in parts.iter().rev() {
                if let super::StrPart::Hole(hole) = part {
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
        Expr::Call { callee, args, target, .. } => {
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
        Expr::Match { subject, arms, .. } => push_match_children(stack, subject, arms),
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..)
        | Expr::Typeof { .. } => {}
    }
}
