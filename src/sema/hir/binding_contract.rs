use super::{
    ArmBody, BindKind, BindingId, Body, CheckedProgram, CtorKind, Expr, Item, Pattern, Place,
    Stmt, StrPart,
};
use crate::sema::types::{types_match, Ty};
use crate::{AliasError, AliasResult, Span};
use std::collections::{HashMap, HashSet};

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

fn register(
    contracts: &mut HashMap<BindingId, Ty>,
    id: BindingId,
    ty: &Ty,
    span: Span,
) -> AliasResult<()> {
    if ty.contains_unknown() {
        return Err(invariant(span, format!("BindingId {id:?} 类型未确定")));
    }
    if contracts.insert(id, ty.clone()).is_some() {
        return Err(invariant(span, format!("BindingId 重复 {id:?}")));
    }
    Ok(())
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
        Expr::Call { callee, args, .. } => {
            for arg in args.iter().rev() {
                stack.push(Node::Expr(&arg.value));
            }
            if matches!(
                expr,
                Expr::Call {
                    target: super::CallTarget::FunctionValue,
                    ..
                }
            ) {
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
        Expr::Move { source, .. } => push_place_expr_children(stack, source),
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

fn pattern_binding_ty(subject: &Ty, pattern: &Pattern) -> AliasResult<Option<Ty>> {
    match pattern {
        Pattern::Binding { .. } => Ok(Some(subject.clone())),
        Pattern::Constructor {
            ctor,
            binding,
            span,
        } => {
            let Ty::Result(ok, err) = subject else {
                return Err(invariant(
                    *span,
                    "result 构造器 Pattern 的主语类型不是 result",
                ));
            };
            if binding.is_none() {
                return Ok(None);
            }
            Ok(Some(match ctor {
                CtorKind::Ok => (**ok).clone(),
                CtorKind::Err => (**err).clone(),
            }))
        }
        Pattern::Wildcard { .. }
        | Pattern::Int { .. }
        | Pattern::Bool { .. }
        | Pattern::Str { .. } => Ok(None),
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

fn collect_contracts(
    program: &CheckedProgram,
) -> AliasResult<(HashMap<BindingId, Ty>, HashSet<BindingId>)> {
    let mut contracts = HashMap::new();
    let mut writable = HashSet::new();
    for item in &program.items {
        if let Item::Binding(binding) = item {
            register(
                &mut contracts,
                binding.binding_id,
                &binding.ty,
                binding.span,
            )?;
            if binding.kind == BindKind::Var {
                writable.insert(binding.binding_id);
            }
        }
    }

    let mut stack = root_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            Node::Expr(expr) => {
                match expr {
                    Expr::FuncLit {
                        params,
                        implicit_bindings,
                        ..
                    } => {
                        let Ty::Func {
                            params: signature_params,
                            ..
                        } = expr.ty()
                        else {
                            return Err(invariant(expr.span(), "FuncLit 缺少完整函数类型"));
                        };
                        if signature_params.len() != implicit_bindings.len() + params.len() {
                            return Err(invariant(
                                expr.span(),
                                "FuncLit 参数数量与完整函数类型不一致",
                            ));
                        }
                        for (id, ty) in implicit_bindings
                            .iter()
                            .zip(&signature_params[..implicit_bindings.len()])
                        {
                            register(&mut contracts, *id, ty, expr.span())?;
                        }
                        for param in params {
                            register(&mut contracts, param.binding_id, &param.ty, expr.span())?;
                        }
                    }
                    Expr::Match { subject, arms, .. } => {
                        for arm in arms {
                            let expected = pattern_binding_ty(subject.ty(), &arm.pattern)?;
                            match (arm.binding_id, expected) {
                                (Some(id), Some(ty)) => {
                                    register(&mut contracts, id, &ty, arm.pattern.span())?;
                                }
                                (Some(_), None) => {
                                    return Err(invariant(
                                        arm.pattern.span(),
                                        "不创建绑定的 Pattern 却携带 BindingId",
                                    ));
                                }
                                (None, Some(_)) => {
                                    return Err(invariant(
                                        arm.pattern.span(),
                                        "创建绑定的 Pattern 缺少 BindingId",
                                    ));
                                }
                                (None, None) => {}
                            }
                        }
                    }
                    _ => {}
                }
                push_expr_children(&mut stack, expr);
            }
            Node::Stmt(stmt) => {
                match stmt {
                    Stmt::Binding(binding) => {
                        register(
                            &mut contracts,
                            binding.binding_id,
                            &binding.ty,
                            binding.span,
                        )?;
                        if binding.kind == BindKind::Var {
                            writable.insert(binding.binding_id);
                        }
                    }
                    Stmt::For {
                        binding_id,
                        ty,
                        span,
                        ..
                    } => register(&mut contracts, *binding_id, ty, *span)?,
                    _ => {}
                }
                push_stmt_children(&mut stack, stmt);
            }
        }
    }
    Ok((contracts, writable))
}

/// Place projection 里的每个 Local root 都必须保持与自身声明一致的静态类型。这里不拥有
/// field/index projection 方程，只验证 BindingId 合同；同一 Place 的终端可写性由外层操作决定。
fn validate_place_bindings(
    place: &Place,
    contracts: &HashMap<BindingId, Ty>,
) -> AliasResult<()> {
    let mut stack = vec![place];
    while let Some(place) = stack.pop() {
        match place {
            Place::Local { binding_id, .. } => {
                if let Some(declared) = contracts.get(binding_id) {
                    if !types_match(declared, place.ty()) {
                        return Err(invariant(
                            place.span(),
                            "Place Local 类型与 BindingId 声明类型不一致",
                        ));
                    }
                }
            }
            Place::Field { base, .. } | Place::Index { base, .. } => stack.push(base),
        }
    }
    Ok(())
}

fn validate_uses(
    program: &CheckedProgram,
    contracts: &HashMap<BindingId, Ty>,
    writable: &HashSet<BindingId>,
) -> AliasResult<()> {
    let mut stack = root_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            Node::Expr(expr) => {
                if let Expr::Call {
                    args,
                    target:
                        super::CallTarget::Builtin(
                            super::BuiltinCall::Increase | super::BuiltinCall::Decrease,
                        ),
                    ..
                } = expr
                {
                    if let [arg] = args.as_slice() {
                        if let Expr::Ident(_, Some(id), ..) = &arg.value {
                            if contracts.contains_key(id) && !writable.contains(id) {
                                return Err(invariant(
                                    arg.value.span(),
                                    "increase/decrease 目标不是可写 var 绑定",
                                ));
                            }
                        }
                    }
                }
                if let Expr::Ident(_, Some(id), ..) = expr {
                    if let Some(declared) = contracts.get(id) {
                        if !types_match(declared, expr.ty()) {
                            return Err(invariant(
                                expr.span(),
                                "Ident 静态类型与 BindingId 声明类型不一致",
                            ));
                        }
                    }
                }
                if let Expr::Move { source, .. } = expr {
                    validate_place_bindings(source, contracts)?;
                }
                push_expr_children(&mut stack, expr);
            }
            Node::Stmt(stmt) => {
                match stmt {
                    Stmt::Assign { target, .. } => {
                        validate_place_bindings(target, contracts)?;
                        if let Place::Local { binding_id, .. } = target {
                            if contracts.contains_key(binding_id) && !writable.contains(binding_id) {
                                return Err(invariant(
                                    target.span(),
                                    "Assign 目标不是可写 var 绑定",
                                ));
                            }
                        }
                    }
                    Stmt::For { ty, iterable, .. } => {
                        let elem = match iterable.ty() {
                            Ty::Array(elem) | Ty::Iterator(elem) => elem.as_ref(),
                            _ => {
                                return Err(invariant(
                                    iterable.span(),
                                    "for iterable 的 HIR 类型不是 array/iterator",
                                ));
                            }
                        };
                        if !types_match(elem, ty) {
                            return Err(invariant(
                                iterable.span(),
                                "for iterable 元素类型与循环变量类型不一致",
                            ));
                        }
                    }
                    _ => {}
                }
                push_stmt_children(&mut stack, stmt);
            }
        }
    }
    Ok(())
}

/// Stable BindingId graph 的 final-HIR contract：每个声明 ID 全局唯一并绑定唯一静态类型；
/// Place 中所有 Local root 必须与 BindingId 声明类型一致。只有终端 Local assignment 要求
/// `var`；Field/Index base 的 local mutability 不决定其可投影性。
pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    let (contracts, writable) = collect_contracts(program)?;
    validate_uses(program, &contracts, &writable)
}
