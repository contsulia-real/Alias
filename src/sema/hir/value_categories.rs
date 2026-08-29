use super::{
    ArmBody, Body, CheckedProgram, Expr, ExprCategory, Item, MatchArm, Place, ResolvedConversion,
    Stmt, StrPart,
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

/// resolved HIR 节点到粗粒度 Place/Value category 的唯一映射 owner。
///
/// Identity conversion 不产生新值，必须保留 inner 的 category；其它 conversion/运算/
/// 调用节点都产生 Value。OwnedTemporary/BorrowedValue/Null 会在后续阶段继续细分 Value，
/// 不改变这一层 Place/Value 合同。
fn expected_category(expr: &Expr) -> AliasResult<ExprCategory> {
    Ok(match expr {
        Expr::Ident(_, Some(_), ..) | Expr::Field { .. } | Expr::Index { .. } => {
            ExprCategory::Place
        }
        Expr::Convert {
            expr: inner,
            mode: ResolvedConversion::Identity,
            ..
        } => inner.category().ok_or_else(|| {
            invariant(
                expr.span(),
                "Identity Convert 的 inner category 尚未 finalization",
            )
        })?,
        _ => ExprCategory::Value,
    })
}

/// lowering 已经递归完成当前节点的全部 child，并把源码语义形状解析成最终 HIR 后，
/// 立即固化该节点 category。这里不读取 AST variant，也不保存跨 phase 地址 fact。
pub(super) fn finalize(expr: &mut Expr) -> AliasResult<()> {
    if expr.category().is_some() {
        return Err(invariant(expr.span(), "Expr category 被重复 finalization"));
    }
    let category = expected_category(expr)?;
    expr.info_mut().category = Some(category);
    Ok(())
}

/// final-HIR gate 独立重算每个节点的 resolved category；任何 None 或与最终 HIR 形状
/// 不一致的值都不能进入 codegen。显式栈避免为这项新分析引入额外宿主递归。
pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    let mut stack = root_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            Node::Expr(expr) => {
                let got = expr
                    .category()
                    .ok_or_else(|| invariant(expr.span(), "Expr category 未 finalization"))?;
                let want = expected_category(expr)?;
                if got != want {
                    return Err(invariant(
                        expr.span(),
                        "Expr category 与 resolved HIR 形状不一致",
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
