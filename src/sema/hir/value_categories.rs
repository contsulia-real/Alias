use super::{
    ArmBody, Body, BuiltinCall, CallTarget, CheckedProgram, DeepClonePlan, Expr, ExprCategory, Item,
    MatchArm, MethodTarget, Place, ResolvedConversion, Stmt, StrPart, ValueCategory,
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

/// 当前已能证明不携带独立动态 ownership 的普通值类型。
///
/// string/func/struct/result/array/iterator 都不是 inline；unit 不是值类型，也不能借此被
/// 当成 InlineValue。ptr<T> 等后续类型必须由其正式 ownership 规则决定，而不是默认归类。
fn is_inline_value(ty: &Ty) -> bool {
    matches!(ty, Ty::Int(_) | Ty::UInt(_) | Ty::Float(_) | Ty::Bool)
}

/// 当前已经具有独立动态 ownership root 的语言值类型。
///
/// 这不是 DeepCloneable/ShallowCloneable 判定；它只回答一个已产生的 Value 是否可能携带
/// 独立 owner。ptr<T> 等后续类型引入后必须在其静态 ownership 规则落地时扩展此 owner。
fn carries_dynamic_owner(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Str
            | Ty::Func { .. }
            | Ty::Struct(_)
            | Ty::Result(..)
            | Ty::Array(_)
            | Ty::Iterator(_)
    )
}

fn deep_clone_creates_owner(plan: &DeepClonePlan) -> bool {
    match plan {
        DeepClonePlan::Inline => false,
        DeepClonePlan::String
        | DeepClonePlan::Struct { .. }
        | DeepClonePlan::Array(_)
        | DeepClonePlan::Result { .. } => true,
    }
}

/// 当前阶段能够不依赖 function effects、loan 或 capability dataflow 就证明为新 owner 的
/// resolved HIR 来源。这里必须保持保守：动态函数/用户方法返回、ternary/match merge、
/// propagate 等仍交给后续 ownership/effect 阶段，不得仅按结果类型猜成 OwnedTemporary。
fn produces_owned_temporary(expr: &Expr) -> bool {
    match expr {
        Expr::Str(..)
        | Expr::This(..)
        | Expr::Typeof { .. }
        | Expr::ArrayLit { .. }
        | Expr::FuncLit { .. } => true,
        Expr::Cast { .. }
        | Expr::Convert {
            mode: ResolvedConversion::Convert,
            ..
        } => carries_dynamic_owner(expr.ty()),
        Expr::Call { target, .. } => match target {
            CallTarget::StructConstructor { .. } | CallTarget::ResultConstructor(_) => true,
            CallTarget::Builtin(BuiltinCall::DeepClone(plan)) => deep_clone_creates_owner(plan),
            CallTarget::FunctionValue | CallTarget::Builtin(_) => false,
        },
        Expr::MethodCall { target, .. } => match target {
            MethodTarget::StringUpper
            | MethodTarget::StringLower
            | MethodTarget::StringTrim
            | MethodTarget::ArrayIterator => true,
            MethodTarget::ArrayPop => carries_dynamic_owner(expr.ty()),
            MethodTarget::Numeric(_)
            | MethodTarget::BoolNot
            | MethodTarget::StringLen
            | MethodTarget::ArrayLen
            | MethodTarget::ArrayPush
            | MethodTarget::User { .. } => false,
        },
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::Binary { .. }
        | Expr::Neg { .. }
        | Expr::Not { .. }
        | Expr::BitNot { .. }
        | Expr::Ternary { .. }
        | Expr::Index { .. }
        | Expr::Field { .. }
        | Expr::Match { .. }
        | Expr::Propagate { .. } => false,
        Expr::Convert {
            mode: ResolvedConversion::Identity,
            ..
        } => false,
    }
}

fn inherited_identity_category(inner: &Expr, span: Span) -> AliasResult<ExprCategory> {
    let category = inner
        .category()
        .ok_or_else(|| invariant(span, "Identity Convert 的 inner category 尚未 finalization"))?;
    Ok(match category {
        ExprCategory::Place => ExprCategory::Place,
        ExprCategory::Value(_) => ExprCategory::Value(inner.value_category().ok_or_else(|| {
            invariant(span, "Identity Convert 的 inner Value 缺少 value category")
        })?),
    })
}

/// resolved HIR 节点到 Place/Value + 当前 value subcategory 的唯一映射 owner。
///
/// Identity conversion 不产生新值，必须完整保留 inner category；其它节点先决定 Place/Value，
/// 再优先固化无歧义的 OwnedTemporary 与 InlineValue。`General` 只保留仍需后续
/// ownership/effect 语义继续细分的 Value，codegen 不得据此推断 ownership。
fn expected_category(expr: &Expr) -> AliasResult<ExprCategory> {
    Ok(match expr {
        Expr::Ident(_, Some(_), ..) | Expr::Field { .. } | Expr::Index { .. } => {
            ExprCategory::Place
        }
        Expr::Convert {
            expr: inner,
            mode: ResolvedConversion::Identity,
            span,
            ..
        } => inherited_identity_category(inner, *span)?,
        _ if produces_owned_temporary(expr) => ExprCategory::Value(ValueCategory::OwnedTemporary),
        _ if is_inline_value(expr.ty()) => ExprCategory::Value(ValueCategory::InlineValue),
        _ => ExprCategory::Value(ValueCategory::General),
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

/// final-HIR gate 独立重算每个节点的 resolved category；任何 None 或与最终 HIR 形状/
/// 当前 value-category 规则不一致的值都不能进入 codegen。显式栈避免引入额外宿主递归。
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
                        "Expr category 与 resolved HIR ownership 形状不一致",
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
