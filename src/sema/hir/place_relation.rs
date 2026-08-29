use super::{
    ArmBody, BindingId, Body, CheckedProgram, Expr, Item, Place, ResolvedConversion, Stmt, StrPart,
};
use crate::sema::types::{IntW, Ty};
use crate::{AliasError, AliasResult, Span};

/// 两个 resolved Place 的静态关系。`Unknown` 与 `Overlap` 一样必须被 ownership / borrow
/// conflict 当作冲突；只有 `Disjoint` 才能授权 move replacement 或并存的独占 loan。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaceRelation {
    Disjoint,
    Overlap,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
enum Projection<'a> {
    Field(usize),
    Index(&'a Expr),
}

enum Node<'a> {
    Expr(&'a Expr),
    Stmt(&'a Stmt),
}

fn invariant(span: Span, msg: impl Into<String>) -> AliasError {
    AliasError {
        msg: format!("内部 sema Place relation 不变式被破坏: {}", msg.into()),
        span,
    }
}

fn constant_i32(expr: &Expr) -> Option<i32> {
    if expr.ty() != &Ty::Int(IntW::W32) {
        return None;
    }
    match expr {
        Expr::Int(value, ..) => i32::try_from(*value).ok(),
        Expr::Neg { expr, .. } => match expr.as_ref() {
            Expr::Int(value, ..) if *value == i32::MAX as u64 + 1 => Some(i32::MIN),
            Expr::Int(value, ..) => i32::try_from(*value).ok().map(|value| -value),
            _ => None,
        },
        Expr::Convert {
            expr,
            mode: ResolvedConversion::Identity,
            ..
        } => constant_i32(expr),
        _ => None,
    }
}

fn decompose(place: &Place) -> (BindingId, Vec<Projection<'_>>) {
    let mut current = place;
    let mut path = Vec::new();
    let root = loop {
        match current {
            Place::Local { binding_id, .. } => break *binding_id,
            Place::Field {
                base, field_index, ..
            } => {
                path.push(Projection::Field(*field_index));
                current = base;
            }
            Place::Index { base, index, .. } => {
                path.push(Projection::Index(index));
                current = base;
            }
        }
    };
    path.reverse();
    (root, path)
}

fn index_relation(left: &Expr, right: &Expr) -> PlaceRelation {
    if std::ptr::eq(left, right) {
        return PlaceRelation::Overlap;
    }
    match (constant_i32(left), constant_i32(right)) {
        (Some(left), Some(right)) if left != right => PlaceRelation::Disjoint,
        (Some(_), Some(_)) => PlaceRelation::Overlap,
        _ => PlaceRelation::Unknown,
    }
}

/// Canonical Place overlap owner。
///
/// 规则完全基于 resolved semantic identity：不同 Local root 可证明 disjoint；不同字段或
/// 不同常量下标可证明 disjoint；ancestor/equal path overlap；两个独立动态 index fact 在
/// 没有其它 projection 能证明 disjoint 时保持 Unknown。这里不读取机器地址，也不做
/// runtime alias guessing。
pub(crate) fn relation(left: &Place, right: &Place) -> PlaceRelation {
    let (left_root, left_path) = decompose(left);
    let (right_root, right_path) = decompose(right);
    if left_root != right_root {
        return PlaceRelation::Disjoint;
    }

    let mut uncertain = false;
    for (left, right) in left_path.iter().zip(&right_path) {
        match (*left, *right) {
            (Projection::Field(a), Projection::Field(b)) => {
                if a != b {
                    return PlaceRelation::Disjoint;
                }
            }
            (Projection::Index(left), Projection::Index(right)) => match index_relation(left, right) {
                PlaceRelation::Disjoint => return PlaceRelation::Disjoint,
                PlaceRelation::Overlap => {}
                PlaceRelation::Unknown => uncertain = true,
            },
            // 对已通过 typed-HIR gate 的 Place，同一 base 不会同时既是 struct 又是 array。
            // relation owner 仍 fail-closed，避免被损坏 HIR 诱导出假的 Disjoint。
            (Projection::Field(_), Projection::Index(_))
            | (Projection::Index(_), Projection::Field(_)) => return PlaceRelation::Unknown,
        }
    }

    if uncertain {
        PlaceRelation::Unknown
    } else {
        // 完全相同，或一方是另一方 ancestor，均覆盖同一 storage region。
        PlaceRelation::Overlap
    }
}

fn validate_place_chain(place: &Place) -> AliasResult<()> {
    if relation(place, place) != PlaceRelation::Overlap {
        return Err(invariant(place.span(), "Place 与自身必须为 Overlap"));
    }
    let mut child = place;
    loop {
        let base = match child {
            Place::Local { .. } => break,
            Place::Field { base, .. } | Place::Index { base, .. } => base.as_ref(),
        };
        if relation(child, base) != PlaceRelation::Overlap {
            return Err(invariant(
                child.span(),
                "Place projection 与其直接 ancestor 必须为 Overlap",
            ));
        }
        child = base;
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

fn push_match_children<'a>(stack: &mut Vec<Node<'a>>, subject: &'a Expr, arms: &'a [super::MatchArm]) {
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

fn push_stmt_children<'a>(stack: &mut Vec<Node<'a>>, stmt: &'a Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(Node::Expr(&binding.value)),
        Stmt::Assign { target, value } => {
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
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..)
        | Expr::Typeof { .. } => {}
    }
}

/// Final-HIR relation gate。这里只验证 relation owner 对已经 resolved 的 Place graph 保持
/// 自反/ancestor 不变量；真正的 move/loan conflict consumer 再比较两个独立 Place。
pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    let mut stack = root_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            Node::Expr(expr) => push_expr_children(&mut stack, expr),
            Node::Stmt(stmt) => {
                if let Stmt::Assign { target, .. } = stmt {
                    validate_place_chain(target)?;
                }
                push_stmt_children(&mut stack, stmt);
            }
        }
    }
    Ok(())
}
