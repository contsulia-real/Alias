//! Function parameter-effect inference and caller-side argument planning.
//!
//! `Ty::Func` is the canonical semantic signature owner. The checker can only construct its
//! type-only shape, so this pass solves body/call dependencies after complete HIR exists, then
//! writes the same frozen effects into function types, HIR parameters, user-method targets, and
//! call argument plans. Ownership flow consumes those plans; codegen never re-infers an effect.

use super::{
    ArgumentPass, ArmBody, Binding, BindingId, BindingOwner, Body, BorrowKind, CallArg, CallTarget,
    CheckedProgram, Expr, FunctionId, Item, MethodId, MethodTarget, OwnershipCapability, Place,
    ResolvedConversion, Stmt, StorageRelation, StrPart, ValueCategory,
};
use crate::sema::types::{ParamEffect, Ty};
use crate::{AliasError, AliasResult, Span};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
struct FunctionMeta {
    parameter_ids: Vec<BindingId>,
    parameter_types: Vec<Ty>,
    span: Span,
}

struct ProgramFacts<'a> {
    functions: HashMap<FunctionId, &'a Expr>,
    function_order: Vec<FunctionId>,
    function_meta: HashMap<FunctionId, FunctionMeta>,
    binding_values: HashMap<BindingId, &'a Expr>,
    function_bindings: HashMap<BindingId, FunctionId>,
    method_functions: HashMap<MethodId, FunctionId>,
    borrowed_bindings: HashSet<BindingId>,
}

enum Node<'a> {
    Binding(&'a Binding),
    Expr(&'a Expr),
    Stmt(&'a Stmt),
}

fn invariant(span: Span, msg: impl Into<String>) -> AliasError {
    AliasError {
        msg: format!("内部 sema 不变式被破坏: {}", msg.into()),
        span,
    }
}

fn dynamic_owner(ty: &Ty) -> bool {
    super::value_categories::type_carries_dynamic_owner(ty)
}

fn initial_effect(ty: &Ty) -> ParamEffect {
    if dynamic_owner(ty) {
        ParamEffect::ReadBorrow
    } else {
        // Inline values cross the ABI by value. `Owned` is the canonical signature spelling, but
        // it does not manufacture a dynamic ownership capability for scalar arguments.
        ParamEffect::Owned
    }
}

fn initial_effects(facts: &ProgramFacts<'_>) -> HashMap<FunctionId, Vec<ParamEffect>> {
    facts
        .function_meta
        .iter()
        .map(|(id, meta)| {
            (
                *id,
                meta.parameter_types.iter().map(initial_effect).collect(),
            )
        })
        .collect()
}

fn collect_facts(program: &CheckedProgram) -> AliasResult<ProgramFacts<'_>> {
    let mut facts = ProgramFacts {
        functions: HashMap::new(),
        function_order: Vec::new(),
        function_meta: HashMap::new(),
        binding_values: HashMap::new(),
        function_bindings: HashMap::new(),
        method_functions: HashMap::new(),
        borrowed_bindings: HashSet::new(),
    };
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

    while let Some(node) = stack.pop() {
        match node {
            Node::Binding(binding) => {
                if facts
                    .binding_values
                    .insert(binding.binding_id, &binding.value)
                    .is_some()
                {
                    return Err(invariant(
                        binding.span,
                        "parameter effect 遇到重复 BindingId",
                    ));
                }
                if binding.relation == Some(StorageRelation::Borrowed) {
                    facts.borrowed_bindings.insert(binding.binding_id);
                }
                if let Expr::FuncLit { function_id, .. } = &binding.value {
                    facts
                        .function_bindings
                        .insert(binding.binding_id, *function_id);
                    if let BindingOwner::Method { method_id, .. } = binding.owner {
                        facts.method_functions.insert(method_id, *function_id);
                    }
                }
                stack.push(Node::Expr(&binding.value));
            }
            Node::Stmt(stmt) => push_stmt_children(&mut stack, stmt),
            Node::Expr(expr) => {
                if let Expr::FuncLit {
                    function_id,
                    params,
                    implicit_bindings,
                    body,
                    ..
                } = expr
                {
                    let Ty::Func {
                        params: parameter_types,
                        ..
                    } = expr.ty()
                    else {
                        return Err(invariant(expr.span(), "FuncLit 缺少完整函数类型"));
                    };
                    let mut parameter_ids = implicit_bindings.clone();
                    parameter_ids.extend(params.iter().map(|param| param.binding_id));
                    if parameter_ids.len() != parameter_types.len() {
                        return Err(invariant(
                            expr.span(),
                            "FuncLit 参数 BindingId 与函数类型数量不一致",
                        ));
                    }
                    if facts.functions.insert(*function_id, expr).is_some() {
                        return Err(invariant(expr.span(), "FunctionId 重复"));
                    }
                    facts.function_order.push(*function_id);
                    facts.function_meta.insert(
                        *function_id,
                        FunctionMeta {
                            parameter_ids,
                            parameter_types: parameter_types.clone(),
                            span: expr.span(),
                        },
                    );
                    push_body(&mut stack, body);
                } else {
                    push_expr_children(&mut stack, expr);
                }
            }
        }
    }
    Ok(facts)
}

fn join_effect(current: ParamEffect, inferred: ParamEffect) -> ParamEffect {
    use ParamEffect::{Owned, ReadBorrow, WriteBorrow};
    match (current, inferred) {
        (Owned, _) | (_, Owned) => Owned,
        (WriteBorrow, _) | (_, WriteBorrow) => WriteBorrow,
        (ReadBorrow, ReadBorrow) => ReadBorrow,
    }
}

fn merge_candidate(
    merged: &mut Option<Vec<ParamEffect>>,
    candidate: &[ParamEffect],
    span: Span,
    strict: bool,
) -> AliasResult<()> {
    if let Some(existing) = merged {
        if existing != candidate {
            if strict {
                return Err(AliasError {
                    msg: "函数值分支的 parameter effects 不一致".into(),
                    span,
                });
            }
            if existing.len() != candidate.len() {
                return Err(invariant(span, "函数值分支的 parameter effect arity 漂移"));
            }
            for (slot, candidate) in existing.iter_mut().zip(candidate) {
                *slot = join_effect(*slot, *candidate);
            }
        }
    } else {
        *merged = Some(candidate.to_vec());
    }
    Ok(())
}

fn resolve_expr_effects(
    expr: &Expr,
    current_function: Option<FunctionId>,
    facts: &ProgramFacts<'_>,
    binding_effects: &HashMap<BindingId, Vec<ParamEffect>>,
    effects: &HashMap<FunctionId, Vec<ParamEffect>>,
    strict: bool,
) -> AliasResult<Option<Vec<ParamEffect>>> {
    let mut stack = vec![expr];
    let mut visited_bindings = HashSet::new();
    let mut merged = None;
    while let Some(expr) = stack.pop() {
        match expr {
            Expr::FuncLit { function_id, .. } => {
                merge_candidate(
                    &mut merged,
                    effects.get(function_id).ok_or_else(|| {
                        invariant(expr.span(), "FuncLit FunctionId 缺少 effect fact")
                    })?,
                    expr.span(),
                    strict,
                )?;
            }
            Expr::Ident(_, Some(binding_id), span, _) => {
                if let Some(candidate) = binding_effects.get(binding_id) {
                    merge_candidate(&mut merged, candidate, *span, strict)?;
                } else if visited_bindings.insert(*binding_id) {
                    let Some(value) = facts.binding_values.get(binding_id) else {
                        return Ok(None);
                    };
                    stack.push(value);
                } else {
                    return Ok(None);
                }
            }
            Expr::This(span, _) => {
                let Some(function_id) = current_function else {
                    return Err(invariant(*span, "this 缺少 enclosing FunctionId"));
                };
                merge_candidate(
                    &mut merged,
                    effects
                        .get(&function_id)
                        .ok_or_else(|| invariant(*span, "this FunctionId 缺少 effect fact"))?,
                    *span,
                    strict,
                )?;
            }
            Expr::Convert {
                expr: inner,
                mode: ResolvedConversion::Identity,
                ..
            } => stack.push(inner),
            Expr::Ternary {
                then_expr,
                else_expr,
                ..
            } => {
                stack.push(else_expr);
                stack.push(then_expr);
            }
            Expr::Match { arms, .. } => {
                for arm in arms.iter().rev() {
                    match &arm.body {
                        ArmBody::Value(value) => stack.push(value),
                        ArmBody::Block(_) | ArmBody::Ret(_) => return Ok(None),
                    }
                }
            }
            other => {
                if let Ty::Func {
                    param_effects: Some(candidate),
                    ..
                } = other.ty()
                {
                    merge_candidate(&mut merged, candidate, other.span(), strict)?;
                } else {
                    return Ok(None);
                }
            }
        }
    }
    Ok(merged)
}

fn infer_binding_effects(
    facts: &ProgramFacts<'_>,
    effects: &HashMap<FunctionId, Vec<ParamEffect>>,
    strict: bool,
) -> AliasResult<HashMap<BindingId, Vec<ParamEffect>>> {
    let mut resolved = HashMap::new();
    for (binding_id, function_id) in &facts.function_bindings {
        resolved.insert(*binding_id, effects[function_id].clone());
    }
    let candidates: Vec<BindingId> = facts
        .binding_values
        .iter()
        .filter_map(|(id, value)| matches!(value.ty(), Ty::Func { .. }).then_some(*id))
        .collect();
    for _ in 0..=candidates.len() {
        let mut changed = false;
        for binding_id in &candidates {
            if resolved.contains_key(binding_id) {
                continue;
            }
            let value = facts.binding_values[binding_id];
            if let Some(candidate) =
                resolve_expr_effects(value, None, facts, &resolved, effects, strict)?
            {
                resolved.insert(*binding_id, candidate);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for binding_id in candidates {
        if !resolved.contains_key(&binding_id) {
            return Err(invariant(
                facts.binding_values[&binding_id].span(),
                "函数值 binding 的 parameter effects 无法唯一解析",
            ));
        }
    }
    Ok(resolved)
}

fn argument_pass(
    value: &Expr,
    effect: ParamEffect,
    facts: &ProgramFacts<'_>,
    loan_id: &mut impl FnMut() -> AliasResult<super::LoanId>,
) -> AliasResult<ArgumentPass> {
    if !dynamic_owner(value.ty()) {
        return Ok(ArgumentPass::Inline);
    }
    match effect {
        ParamEffect::Owned => {
            if value.value_category() != Some(ValueCategory::OwnedTemporary)
                || value.ownership_capability() != Some(OwnershipCapability::Available)
            {
                return Err(AliasError {
                    msg: "Owned parameter 的动态实参必须是显式 ownership-producing value".into(),
                    span: value.span(),
                });
            }
            Ok(ArgumentPass::Owned)
        }
        ParamEffect::ReadBorrow | ParamEffect::WriteBorrow => {
            if value.value_category() == Some(ValueCategory::BorrowedValue) {
                return Err(AliasError {
                    msg: "borrowed argument 的 referent-loan forwarding 尚未固化".into(),
                    span: value.span(),
                });
            }
            if let Some(source) = super::expr_places::from_expr(value) {
                if facts.borrowed_bindings.contains(&source.root_binding_id()) {
                    return Err(AliasError {
                        msg: "borrowed alias argument 的 referent-loan forwarding 尚未固化".into(),
                        span: value.span(),
                    });
                }
                let loan_id = loan_id()?;
                Ok(match effect {
                    ParamEffect::ReadBorrow => ArgumentPass::ReadBorrow { loan_id, source },
                    ParamEffect::WriteBorrow => ArgumentPass::WriteBorrow { loan_id, source },
                    ParamEffect::Owned => unreachable!(),
                })
            } else if value.value_category() == Some(ValueCategory::OwnedTemporary) {
                Ok(ArgumentPass::BorrowTemporary {
                    kind: if effect == ParamEffect::ReadBorrow {
                        BorrowKind::Read
                    } else {
                        BorrowKind::Write
                    },
                })
            } else {
                Err(AliasError {
                    msg: "borrow parameter 的动态实参必须是 stable Place 或 OwnedTemporary".into(),
                    span: value.span(),
                })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum PassSite {
    Argument(usize),
    Receiver(usize),
}

#[derive(Default)]
struct PassMaps {
    // These address keys only bridge two traversals of the same HIR allocation. Between collect
    // and apply we mutate pass/effect fields in place and never replace/reallocate Expr/CallArg
    // containers; a future HIR rewrite must introduce stable node IDs before changing that order.
    arguments: HashMap<usize, ArgumentPass>,
    receivers: HashMap<usize, ArgumentPass>,
    methods: HashMap<usize, Vec<ParamEffect>>,
}

enum ScopedNode<'a> {
    Binding(&'a Binding, Option<FunctionId>),
    Expr(&'a Expr, Option<FunctionId>),
    Stmt(&'a Stmt, Option<FunctionId>),
}

fn loan_for_site(
    site: PassSite,
    loans: &mut HashMap<PassSite, super::LoanId>,
    next_loan_id: &mut u32,
    span: Span,
) -> AliasResult<super::LoanId> {
    if let Some(id) = loans.get(&site) {
        return Ok(*id);
    }
    let id = super::LoanId(*next_loan_id);
    *next_loan_id = next_loan_id.checked_add(1).ok_or_else(|| AliasError {
        msg: "call loan generation 数量超过编译器上限".into(),
        span,
    })?;
    loans.insert(site, id);
    Ok(id)
}

fn resolved_call_signature(callee: &Expr) -> AliasResult<(&[Ty], &Ty)> {
    let Ty::Func { params, ret, .. } = callee.ty() else {
        return Err(invariant(
            callee.span(),
            "FunctionValue callee 缺少完整函数类型",
        ));
    };
    Ok((params, ret))
}

fn collect_pass_maps(
    program: &CheckedProgram,
    facts: &ProgramFacts<'_>,
    effects: &HashMap<FunctionId, Vec<ParamEffect>>,
    binding_effects: &HashMap<BindingId, Vec<ParamEffect>>,
    site_loans: &mut HashMap<PassSite, super::LoanId>,
    next_loan_id: &mut u32,
    strict: bool,
) -> AliasResult<PassMaps> {
    let mut maps = PassMaps::default();
    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => stack.push(ScopedNode::Binding(binding, None)),
            Item::StructDef(def) => {
                for field in def.fields.iter().rev() {
                    if let Some(default) = &field.default {
                        stack.push(ScopedNode::Expr(default, None));
                    }
                }
            }
        }
    }
    while let Some(node) = stack.pop() {
        match node {
            ScopedNode::Binding(binding, current) => {
                stack.push(ScopedNode::Expr(&binding.value, current));
            }
            ScopedNode::Stmt(stmt, current) => push_scoped_stmt(&mut stack, stmt, current),
            ScopedNode::Expr(expr, current) => {
                match expr {
                    Expr::Call {
                        callee,
                        args,
                        target: CallTarget::FunctionValue,
                        ..
                    } => {
                        let parameter_effects = resolve_expr_effects(
                            callee,
                            current,
                            facts,
                            binding_effects,
                            effects,
                            strict,
                        )?
                        .ok_or_else(|| {
                            invariant(callee.span(), "FunctionValue callee effects 无法唯一解析")
                        })?;
                        let (parameter_types, _) = resolved_call_signature(callee)?;
                        if args.len() != parameter_effects.len()
                            || args.len() != parameter_types.len()
                        {
                            return Err(invariant(expr.span(), "call effect arity 漂移"));
                        }
                        for (arg, effect) in args.iter().zip(parameter_effects) {
                            let key = arg as *const CallArg as usize;
                            let site = PassSite::Argument(key);
                            let mut allocate =
                                || loan_for_site(site, site_loans, next_loan_id, arg.value.span());
                            let pass = argument_pass(&arg.value, effect, facts, &mut allocate)?;
                            if maps.arguments.insert(key, pass).is_some() {
                                return Err(invariant(expr.span(), "CallArg identity 重复"));
                            }
                        }
                    }
                    Expr::MethodCall {
                        recv,
                        args,
                        target: MethodTarget::User { id, .. },
                        ..
                    } => {
                        let function_id = facts.method_functions.get(id).ok_or_else(|| {
                            invariant(expr.span(), "MethodId 缺少 owning FunctionId")
                        })?;
                        let parameter_effects = effects.get(function_id).ok_or_else(|| {
                            invariant(expr.span(), "MethodId 缺少 parameter effects")
                        })?;
                        if parameter_effects.len() != args.len() + 1 {
                            return Err(invariant(expr.span(), "method effect arity 漂移"));
                        }
                        let expr_key = expr as *const Expr as usize;
                        maps.methods.insert(expr_key, parameter_effects.clone());
                        let receiver_site = PassSite::Receiver(expr_key);
                        let mut allocate_receiver =
                            || loan_for_site(receiver_site, site_loans, next_loan_id, recv.span());
                        maps.receivers.insert(
                            expr_key,
                            argument_pass(
                                recv,
                                parameter_effects[0],
                                facts,
                                &mut allocate_receiver,
                            )?,
                        );
                        for (arg, effect) in args.iter().zip(&parameter_effects[1..]) {
                            let key = arg as *const CallArg as usize;
                            let site = PassSite::Argument(key);
                            let mut allocate =
                                || loan_for_site(site, site_loans, next_loan_id, arg.value.span());
                            let pass = argument_pass(&arg.value, *effect, facts, &mut allocate)?;
                            if maps.arguments.insert(key, pass).is_some() {
                                return Err(invariant(expr.span(), "method CallArg identity 重复"));
                            }
                        }
                    }
                    _ => {}
                }
                push_scoped_expr(&mut stack, expr, current);
            }
        }
    }
    Ok(maps)
}

enum MutNode<'a> {
    Binding(&'a mut Binding),
    Expr(&'a mut Expr),
    Stmt(&'a mut Stmt),
}

fn apply_pass_maps(program: &mut CheckedProgram, maps: &PassMaps) -> AliasResult<()> {
    let mut stack = root_mut_nodes(program);
    let mut seen_args = HashSet::new();
    let mut seen_methods = HashSet::new();
    while let Some(node) = stack.pop() {
        match node {
            MutNode::Binding(binding) => stack.push(MutNode::Expr(&mut binding.value)),
            MutNode::Stmt(stmt) => push_mut_stmt(&mut stack, stmt),
            MutNode::Expr(expr) => {
                let expr_key = expr as *const Expr as usize;
                let expr_span = expr.span();
                match expr {
                    Expr::Call {
                        args,
                        target: CallTarget::FunctionValue,
                        ..
                    } => {
                        for arg in args.iter_mut() {
                            let key = arg as *const CallArg as usize;
                            arg.pass =
                                Some(maps.arguments.get(&key).cloned().ok_or_else(|| {
                                    invariant(
                                        arg.value.span(),
                                        "FunctionValue CallArg 缺少 pass fact",
                                    )
                                })?);
                            seen_args.insert(key);
                        }
                    }
                    Expr::MethodCall {
                        receiver_pass,
                        args,
                        target: MethodTarget::User { param_effects, .. },
                        ..
                    } => {
                        *receiver_pass =
                            Some(maps.receivers.get(&expr_key).cloned().ok_or_else(|| {
                                invariant(expr_span, "user method receiver 缺少 pass fact")
                            })?);
                        *param_effects =
                            Some(maps.methods.get(&expr_key).cloned().ok_or_else(|| {
                                invariant(expr_span, "user method target 缺少 effect fact")
                            })?);
                        seen_methods.insert(expr_key);
                        for arg in args.iter_mut() {
                            let key = arg as *const CallArg as usize;
                            arg.pass =
                                Some(maps.arguments.get(&key).cloned().ok_or_else(|| {
                                    invariant(
                                        arg.value.span(),
                                        "user method CallArg 缺少 pass fact",
                                    )
                                })?);
                            seen_args.insert(key);
                        }
                    }
                    _ => {}
                }
                push_mut_expr(&mut stack, expr);
            }
        }
    }
    if seen_args.len() != maps.arguments.len() || seen_methods.len() != maps.methods.len() {
        return Err(invariant(
            Span::default(),
            "存在未写回的 call parameter-effect fact",
        ));
    }
    Ok(())
}

fn expression_effect_map(
    program: &CheckedProgram,
    facts: &ProgramFacts<'_>,
    effects: &HashMap<FunctionId, Vec<ParamEffect>>,
    binding_effects: &HashMap<BindingId, Vec<ParamEffect>>,
) -> AliasResult<HashMap<usize, Vec<ParamEffect>>> {
    // Like PassMaps, this phase-local address map is consumed before any structural HIR edit.
    // Rebuilding expressions here would detach final signatures from the nodes they describe.
    let mut result = HashMap::new();
    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => stack.push(ScopedNode::Binding(binding, None)),
            Item::StructDef(def) => {
                for field in def.fields.iter().rev() {
                    if let Some(default) = &field.default {
                        stack.push(ScopedNode::Expr(default, None));
                    }
                }
            }
        }
    }
    while let Some(node) = stack.pop() {
        match node {
            ScopedNode::Binding(binding, current) => {
                stack.push(ScopedNode::Expr(&binding.value, current));
            }
            ScopedNode::Stmt(stmt, current) => push_scoped_stmt(&mut stack, stmt, current),
            ScopedNode::Expr(expr, current) => {
                if matches!(expr.ty(), Ty::Func { .. }) {
                    let resolved =
                        resolve_expr_effects(expr, current, facts, binding_effects, effects, true)?;
                    if let Some(resolved) = resolved {
                        result.insert(expr as *const Expr as usize, resolved);
                    } else if !matches!(expr, Expr::Ident(_, None, ..)) {
                        return Err(invariant(expr.span(), "函数值表达式 effects 无法唯一解析"));
                    }
                }
                push_scoped_expr(&mut stack, expr, current);
            }
        }
    }
    Ok(result)
}

fn set_function_effects(ty: &mut Ty, effects: &[ParamEffect], span: Span) -> AliasResult<()> {
    let Ty::Func {
        params,
        param_effects,
        ..
    } = ty
    else {
        return Err(invariant(span, "effect fact 写入非函数类型"));
    };
    if params.len() != effects.len() {
        return Err(invariant(span, "函数类型 effect 数量漂移"));
    }
    *param_effects = Some(effects.to_vec());
    Ok(())
}

fn apply_signatures(
    program: &mut CheckedProgram,
    effects: &HashMap<FunctionId, Vec<ParamEffect>>,
    binding_effects: &HashMap<BindingId, Vec<ParamEffect>>,
    expr_effects: &HashMap<usize, Vec<ParamEffect>>,
) -> AliasResult<()> {
    let mut stack = root_mut_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            MutNode::Binding(binding) => {
                if let Some(resolved) = binding_effects.get(&binding.binding_id) {
                    set_function_effects(&mut binding.ty, resolved, binding.span)?;
                }
                stack.push(MutNode::Expr(&mut binding.value));
            }
            MutNode::Stmt(stmt) => push_mut_stmt(&mut stack, stmt),
            MutNode::Expr(expr) => {
                let key = expr as *const Expr as usize;
                let expr_span = expr.span();
                if let Some(resolved) = expr_effects.get(&key) {
                    set_function_effects(&mut expr.info_mut().ty, resolved, expr_span)?;
                }
                if let Expr::FuncLit {
                    function_id,
                    params,
                    implicit_bindings,
                    ..
                } = expr
                {
                    let resolved = effects.get(function_id).ok_or_else(|| {
                        invariant(expr_span, "FuncLit 缺少 final parameter effects")
                    })?;
                    if resolved.len() != implicit_bindings.len() + params.len() {
                        return Err(invariant(expr_span, "FuncLit Param effect 数量漂移"));
                    }
                    for (param, effect) in
                        params.iter_mut().zip(&resolved[implicit_bindings.len()..])
                    {
                        param.effect = Some(*effect);
                    }
                }
                push_mut_expr(&mut stack, expr);
            }
        }
    }
    Ok(())
}

fn expr_uses_any_binding(expr: &Expr, bindings: &HashSet<BindingId>) -> bool {
    let mut stack = vec![Node::Expr(expr)];
    while let Some(node) = stack.pop() {
        match node {
            Node::Binding(binding) => stack.push(Node::Expr(&binding.value)),
            Node::Stmt(stmt) => push_stmt_children(&mut stack, stmt),
            Node::Expr(expr) => match expr {
                Expr::Ident(_, Some(binding_id), ..) if bindings.contains(binding_id) => {
                    return true;
                }
                Expr::ReadPlace { source, .. }
                | Expr::Borrow { source, .. }
                | Expr::Move { source, .. }
                    if bindings.contains(&source.root_binding_id()) =>
                {
                    return true;
                }
                _ => push_expr_children(&mut stack, expr),
            },
        }
    }
    false
}

fn validate_parameter_return_value(
    value: &Expr,
    borrowed_parameters: &HashSet<BindingId>,
) -> AliasResult<()> {
    if !dynamic_owner(value.ty())
        || (value.value_category() == Some(ValueCategory::OwnedTemporary)
            && value.ownership_capability() == Some(OwnershipCapability::Available))
        || !expr_uses_any_binding(value, borrowed_parameters)
    {
        return Ok(());
    }
    Err(AliasError {
        msg: "borrow parameter 的 return source 尚未固化，不能逃逸当前调用 loan".into(),
        span: value.span(),
    })
}

fn validate_parameter_return_boundaries(
    facts: &ProgramFacts<'_>,
    effects: &HashMap<FunctionId, Vec<ParamEffect>>,
) -> AliasResult<()> {
    for function_id in &facts.function_order {
        let function = facts.functions[function_id];
        let Expr::FuncLit { body, .. } = function else {
            return Err(invariant(function.span(), "FunctionId 指向非 FuncLit"));
        };
        let meta = &facts.function_meta[function_id];
        let resolved = effects
            .get(function_id)
            .ok_or_else(|| invariant(function.span(), "return boundary 缺少 parameter effects"))?;
        if meta.parameter_ids.len() != resolved.len() {
            return Err(invariant(
                function.span(),
                "return boundary parameter effect 数量漂移",
            ));
        }
        let borrowed_parameters: HashSet<_> = meta
            .parameter_ids
            .iter()
            .zip(resolved)
            .filter_map(|(id, effect)| (*effect != ParamEffect::Owned).then_some(*id))
            .collect();
        if borrowed_parameters.is_empty() {
            continue;
        }

        let mut stack = Vec::new();
        push_body(&mut stack, body);
        while let Some(node) = stack.pop() {
            match node {
                Node::Binding(binding) => stack.push(Node::Expr(&binding.value)),
                Node::Stmt(stmt) => {
                    if let Stmt::Return { value: Some(value) } = stmt {
                        validate_parameter_return_value(value, &borrowed_parameters)?;
                    }
                    push_stmt_children(&mut stack, stmt);
                }
                Node::Expr(Expr::FuncLit { .. }) => {
                    // Nested returns belong to the nested FunctionId and are checked separately.
                }
                Node::Expr(Expr::Match { subject, arms, .. }) => {
                    stack.push(Node::Expr(subject));
                    for arm in arms.iter().rev() {
                        match &arm.body {
                            ArmBody::Block(stmts) => {
                                for stmt in stmts.iter().rev() {
                                    stack.push(Node::Stmt(stmt));
                                }
                            }
                            ArmBody::Value(value) => stack.push(Node::Expr(value)),
                            ArmBody::Ret(value) => {
                                validate_parameter_return_value(value, &borrowed_parameters)?;
                                stack.push(Node::Expr(value));
                            }
                        }
                    }
                }
                Node::Expr(expr) => push_expr_children(&mut stack, expr),
            }
        }
    }
    Ok(())
}

fn validate_argument_pass(
    value: &Expr,
    effect: ParamEffect,
    pass: &ArgumentPass,
    facts: &ProgramFacts<'_>,
) -> AliasResult<()> {
    if !dynamic_owner(value.ty()) {
        return if matches!(pass, ArgumentPass::Inline) {
            Ok(())
        } else {
            Err(invariant(
                value.span(),
                "inline argument 携带 dynamic parameter pass",
            ))
        };
    }
    match effect {
        ParamEffect::Owned => {
            if !matches!(pass, ArgumentPass::Owned)
                || value.value_category() != Some(ValueCategory::OwnedTemporary)
                || value.ownership_capability() != Some(OwnershipCapability::Available)
            {
                return Err(invariant(
                    value.span(),
                    "Owned parameter argument pass 与 ownership capability 漂移",
                ));
            }
        }
        ParamEffect::ReadBorrow | ParamEffect::WriteBorrow => {
            if value.value_category() == Some(ValueCategory::BorrowedValue) {
                return Err(invariant(
                    value.span(),
                    "borrowed argument 绕过 referent-loan forwarding gate",
                ));
            }
            if let Some(expected_source) = super::expr_places::from_expr(value) {
                if facts
                    .borrowed_bindings
                    .contains(&expected_source.root_binding_id())
                {
                    return Err(invariant(
                        value.span(),
                        "borrowed alias argument 绕过 referent-loan forwarding gate",
                    ));
                }
                let source = match (effect, pass) {
                    (ParamEffect::ReadBorrow, ArgumentPass::ReadBorrow { source, .. })
                    | (ParamEffect::WriteBorrow, ArgumentPass::WriteBorrow { source, .. }) => {
                        source
                    }
                    _ => {
                        return Err(invariant(
                            value.span(),
                            "stable Place argument pass 与 parameter effect 漂移",
                        ));
                    }
                };
                if !super::expr_places::same_source(source, &expected_source) {
                    return Err(invariant(
                        value.span(),
                        "argument pass source 与实参 Place 漂移",
                    ));
                }
            } else if value.value_category() == Some(ValueCategory::OwnedTemporary) {
                let expected_kind = if effect == ParamEffect::ReadBorrow {
                    BorrowKind::Read
                } else {
                    BorrowKind::Write
                };
                if !matches!(
                    pass,
                    ArgumentPass::BorrowTemporary { kind } if *kind == expected_kind
                ) {
                    return Err(invariant(
                        value.span(),
                        "temporary argument pass 与 parameter effect 漂移",
                    ));
                }
            } else {
                return Err(invariant(
                    value.span(),
                    "borrow parameter argument 缺少 stable Place 或 OwnedTemporary",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn finalize(program: &mut CheckedProgram, next_loan_id: &mut u32) -> AliasResult<()> {
    let mut effects = {
        let facts = collect_facts(program)?;
        initial_effects(&facts)
    };
    // Fixed-point iterations need stable per-call generations so capture/ownership analysis can
    // compare graphs, but those provisional IDs must not consume the final HIR namespace: an
    // effect can rise from Borrow to Owned and remove the loan before convergence.
    let first_call_loan_id = *next_loan_id;
    let mut provisional_next_loan_id = first_call_loan_id;
    let mut provisional_site_loans = HashMap::new();
    let total_parameters = effects.values().map(Vec::len).sum::<usize>();
    let max_iterations = total_parameters.saturating_mul(2).saturating_add(2);
    let mut stable = false;
    for _ in 0..max_iterations {
        let maps = {
            let facts = collect_facts(program)?;
            let binding_effects = infer_binding_effects(&facts, &effects, false)?;
            collect_pass_maps(
                program,
                &facts,
                &effects,
                &binding_effects,
                &mut provisional_site_loans,
                &mut provisional_next_loan_id,
                false,
            )?
        };
        apply_pass_maps(program, &maps)?;
        let capture_kinds = super::capture::infer_loan_kinds(program)?;
        let facts = collect_facts(program)?;
        let mut changed = false;
        for function_id in &facts.function_order {
            let inferred = super::ownership_flow::infer_parameter_effects_for_function(
                facts.functions[function_id],
                &capture_kinds,
            )?;
            let current = effects.get_mut(function_id).ok_or_else(|| {
                invariant(
                    facts.function_meta[function_id].span,
                    "effect map 缺少 FunctionId",
                )
            })?;
            if current.len() != inferred.len() {
                return Err(invariant(
                    facts.function_meta[function_id].span,
                    "inferred parameter effect 数量漂移",
                ));
            }
            for (slot, inferred) in current.iter_mut().zip(inferred) {
                let joined = join_effect(*slot, inferred);
                changed |= joined != *slot;
                *slot = joined;
            }
        }
        if !changed {
            stable = true;
            break;
        }
    }
    if !stable {
        return Err(invariant(
            Span::default(),
            "parameter effect fixed-point 未在有限格高度内收敛",
        ));
    }

    let (binding_effects, expr_effects, maps, final_next_loan_id) = {
        let facts = collect_facts(program)?;
        let binding_effects = infer_binding_effects(&facts, &effects, true)?;
        let expr_effects = expression_effect_map(program, &facts, &effects, &binding_effects)?;
        let mut final_site_loans = HashMap::new();
        let mut final_next_loan_id = first_call_loan_id;
        let maps = collect_pass_maps(
            program,
            &facts,
            &effects,
            &binding_effects,
            &mut final_site_loans,
            &mut final_next_loan_id,
            true,
        )?;
        (binding_effects, expr_effects, maps, final_next_loan_id)
    };
    apply_pass_maps(program, &maps)?;
    apply_signatures(program, &effects, &binding_effects, &expr_effects)?;
    *next_loan_id = final_next_loan_id;
    {
        let facts = collect_facts(program)?;
        validate_parameter_return_boundaries(&facts, &effects)?;
    }
    super::capture::validate_effect_boundaries(program)?;
    Ok(())
}

pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    let facts = collect_facts(program)?;
    let mut frozen_effects = HashMap::new();
    for function_id in &facts.function_order {
        let function = facts.functions[function_id];
        let Expr::FuncLit {
            params,
            implicit_bindings,
            ..
        } = function
        else {
            return Err(invariant(function.span(), "FunctionId 指向非 FuncLit"));
        };
        let Ty::Func {
            params: parameter_types,
            param_effects: Some(parameter_effects),
            ..
        } = function.ty()
        else {
            return Err(invariant(
                function.span(),
                "FuncLit 缺少 final parameter effects",
            ));
        };
        if parameter_types.len() != parameter_effects.len()
            || parameter_effects.len() != implicit_bindings.len() + params.len()
        {
            return Err(invariant(
                function.span(),
                "FuncLit effect signature arity 漂移",
            ));
        }
        for (param, effect) in params
            .iter()
            .zip(&parameter_effects[implicit_bindings.len()..])
        {
            if param.effect != Some(*effect) {
                return Err(invariant(function.span(), "HIR Param effect 与签名漂移"));
            }
        }
        frozen_effects.insert(*function_id, parameter_effects.clone());
    }

    let binding_effects = infer_binding_effects(&facts, &frozen_effects, true)?;
    let expression_effects =
        expression_effect_map(program, &facts, &frozen_effects, &binding_effects)?;
    let capture_kinds = super::capture::infer_loan_kinds(program)?;
    for function_id in &facts.function_order {
        let function = facts.functions[function_id];
        let inferred =
            super::ownership_flow::infer_parameter_effects_for_function(function, &capture_kinds)?;
        if frozen_effects.get(function_id) != Some(&inferred) {
            return Err(invariant(
                function.span(),
                "final parameter effects 与函数体/调用 fixed-point 漂移",
            ));
        }
    }
    validate_parameter_return_boundaries(&facts, &frozen_effects)?;

    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => stack.push(ScopedNode::Binding(binding, None)),
            Item::StructDef(def) => {
                for field in def.fields.iter().rev() {
                    if let Some(default) = &field.default {
                        stack.push(ScopedNode::Expr(default, None));
                    }
                }
            }
        }
    }
    while let Some(node) = stack.pop() {
        match node {
            ScopedNode::Binding(binding, current) => {
                if let Ty::Func { param_effects, .. } = &binding.ty {
                    let expected = binding_effects.get(&binding.binding_id).ok_or_else(|| {
                        invariant(binding.span, "function binding 缺少 resolved effects")
                    })?;
                    if param_effects.as_deref() != Some(expected.as_slice()) {
                        return Err(invariant(
                            binding.span,
                            "function binding 类型与 resolved effects 漂移",
                        ));
                    }
                }
                stack.push(ScopedNode::Expr(&binding.value, current));
            }
            ScopedNode::Stmt(stmt, current) => push_scoped_stmt(&mut stack, stmt, current),
            ScopedNode::Expr(expr, current) => {
                let expr_key = expr as *const Expr as usize;
                if let Ty::Func { param_effects, .. } = expr.ty() {
                    if let Some(expected) = expression_effects.get(&expr_key) {
                        if param_effects.as_deref() != Some(expected.as_slice()) {
                            return Err(invariant(
                                expr.span(),
                                "function expression 类型与 resolved effects 漂移",
                            ));
                        }
                    } else if !matches!(expr, Expr::Ident(_, None, ..)) {
                        return Err(invariant(
                            expr.span(),
                            "function expression 缺少 resolved effects",
                        ));
                    }
                }

                match expr {
                    Expr::Call {
                        callee,
                        args,
                        target: CallTarget::FunctionValue,
                        ..
                    } => {
                        let Ty::Func {
                            param_effects: Some(parameter_effects),
                            ..
                        } = callee.ty()
                        else {
                            return Err(invariant(
                                callee.span(),
                                "FunctionValue callee 缺少 final parameter effects",
                            ));
                        };
                        if parameter_effects.len() != args.len() {
                            return Err(invariant(expr.span(), "FunctionValue pass arity 漂移"));
                        }
                        for (arg, effect) in args.iter().zip(parameter_effects) {
                            let pass = arg.pass.as_ref().ok_or_else(|| {
                                invariant(arg.value.span(), "FunctionValue CallArg 缺少 pass fact")
                            })?;
                            validate_argument_pass(&arg.value, *effect, pass, &facts)?;
                        }
                    }
                    Expr::MethodCall {
                        recv,
                        receiver_pass,
                        args,
                        target:
                            MethodTarget::User {
                                id, param_effects, ..
                            },
                        ..
                    } => {
                        let function_id = facts.method_functions.get(id).ok_or_else(|| {
                            invariant(expr.span(), "MethodId 缺少 owning FunctionId")
                        })?;
                        let expected = frozen_effects.get(function_id).ok_or_else(|| {
                            invariant(expr.span(), "MethodId 缺少 final parameter effects")
                        })?;
                        if param_effects.as_deref() != Some(expected.as_slice())
                            || expected.len() != args.len() + 1
                        {
                            return Err(invariant(
                                expr.span(),
                                "user method target parameter effects 漂移",
                            ));
                        }
                        let receiver_pass = receiver_pass.as_ref().ok_or_else(|| {
                            invariant(recv.span(), "user method receiver 缺少 pass fact")
                        })?;
                        validate_argument_pass(recv, expected[0], receiver_pass, &facts)?;
                        for (arg, effect) in args.iter().zip(&expected[1..]) {
                            let pass = arg.pass.as_ref().ok_or_else(|| {
                                invariant(arg.value.span(), "user method CallArg 缺少 pass fact")
                            })?;
                            validate_argument_pass(&arg.value, *effect, pass, &facts)?;
                        }
                    }
                    Expr::Call { args, .. } => {
                        if args.iter().any(|arg| arg.pass.is_some()) {
                            return Err(invariant(
                                expr.span(),
                                "non-FunctionValue call 携带 parameter pass",
                            ));
                        }
                    }
                    Expr::MethodCall {
                        receiver_pass,
                        args,
                        ..
                    } if receiver_pass.is_some() || args.iter().any(|arg| arg.pass.is_some()) => {
                        return Err(invariant(
                            expr.span(),
                            "builtin method call 携带 user parameter pass",
                        ));
                    }
                    _ => {}
                }
                push_scoped_expr(&mut stack, expr, current);
            }
        }
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

fn push_stmt_children<'a>(stack: &mut Vec<Node<'a>>, stmt: &'a Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(Node::Binding(binding)),
        Stmt::Assign { target, value } => {
            push_place_indices(stack, target);
            stack.push(Node::Expr(value));
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

fn push_place_indices<'a>(stack: &mut Vec<Node<'a>>, place: &'a Place) {
    let mut current = place;
    loop {
        match current {
            Place::Local { .. } => return,
            Place::Field { base, .. } => current = base,
            Place::Index { base, index, .. } => {
                stack.push(Node::Expr(index));
                current = base;
            }
        }
    }
}

fn push_expr_children<'a>(stack: &mut Vec<Node<'a>>, expr: &'a Expr) {
    match expr {
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..)
        | Expr::Typeof { .. } => {}
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
        | Expr::Move { source, .. } => push_place_indices(stack, source),
    }
}

fn push_scoped_body<'a>(
    stack: &mut Vec<ScopedNode<'a>>,
    body: &'a Body,
    current: Option<FunctionId>,
) {
    match body {
        Body::Block(stmts) => {
            for stmt in stmts.iter().rev() {
                stack.push(ScopedNode::Stmt(stmt, current));
            }
        }
        Body::Single(stmt) => stack.push(ScopedNode::Stmt(stmt, current)),
    }
}

fn push_scoped_stmt<'a>(
    stack: &mut Vec<ScopedNode<'a>>,
    stmt: &'a Stmt,
    current: Option<FunctionId>,
) {
    match stmt {
        Stmt::Binding(binding) => stack.push(ScopedNode::Binding(binding, current)),
        Stmt::Assign { target, value } => {
            let mut place = target;
            loop {
                match place {
                    Place::Local { .. } => break,
                    Place::Field { base, .. } => place = base,
                    Place::Index { base, index, .. } => {
                        stack.push(ScopedNode::Expr(index, current));
                        place = base;
                    }
                }
            }
            stack.push(ScopedNode::Expr(value, current));
        }
        Stmt::Expr { expr } => stack.push(ScopedNode::Expr(expr, current)),
        Stmt::Return { value } => {
            if let Some(value) = value {
                stack.push(ScopedNode::Expr(value, current));
            }
        }
        Stmt::If {
            branches,
            else_body,
        } => {
            if let Some(body) = else_body {
                for stmt in body.iter().rev() {
                    stack.push(ScopedNode::Stmt(stmt, current));
                }
            }
            for (cond, body) in branches.iter().rev() {
                for stmt in body.iter().rev() {
                    stack.push(ScopedNode::Stmt(stmt, current));
                }
                stack.push(ScopedNode::Expr(cond, current));
            }
        }
        Stmt::While { cond, body } => {
            for stmt in body.iter().rev() {
                stack.push(ScopedNode::Stmt(stmt, current));
            }
            stack.push(ScopedNode::Expr(cond, current));
        }
        Stmt::For { iterable, body, .. } => {
            for stmt in body.iter().rev() {
                stack.push(ScopedNode::Stmt(stmt, current));
            }
            stack.push(ScopedNode::Expr(iterable, current));
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn push_scoped_expr<'a>(
    stack: &mut Vec<ScopedNode<'a>>,
    expr: &'a Expr,
    current: Option<FunctionId>,
) {
    match expr {
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..)
        | Expr::Typeof { .. } => {}
        Expr::Str(parts, ..) => {
            for part in parts.iter().rev() {
                if let StrPart::Hole(hole) = part {
                    stack.push(ScopedNode::Expr(hole, current));
                }
            }
        }
        Expr::Cast { expr, .. }
        | Expr::Convert { expr, .. }
        | Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Propagate { expr, .. } => stack.push(ScopedNode::Expr(expr, current)),
        Expr::Binary { lhs, rhs, .. } => {
            stack.push(ScopedNode::Expr(rhs, current));
            stack.push(ScopedNode::Expr(lhs, current));
        }
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            stack.push(ScopedNode::Expr(else_expr, current));
            stack.push(ScopedNode::Expr(then_expr, current));
            stack.push(ScopedNode::Expr(cond, current));
        }
        Expr::Call { callee, args, .. } => {
            for arg in args.iter().rev() {
                stack.push(ScopedNode::Expr(&arg.value, current));
            }
            stack.push(ScopedNode::Expr(callee, current));
        }
        Expr::MethodCall { recv, args, .. } => {
            for arg in args.iter().rev() {
                stack.push(ScopedNode::Expr(&arg.value, current));
            }
            stack.push(ScopedNode::Expr(recv, current));
        }
        Expr::Field { recv, .. } => stack.push(ScopedNode::Expr(recv, current)),
        Expr::Index { recv, idx, .. } => {
            stack.push(ScopedNode::Expr(idx, current));
            stack.push(ScopedNode::Expr(recv, current));
        }
        Expr::ArrayLit { elems, .. } => {
            for elem in elems.iter().rev() {
                stack.push(ScopedNode::Expr(elem, current));
            }
        }
        Expr::FuncLit {
            function_id, body, ..
        } => push_scoped_body(stack, body, Some(*function_id)),
        Expr::Match { subject, arms, .. } => {
            for arm in arms.iter().rev() {
                match &arm.body {
                    ArmBody::Block(stmts) => {
                        for stmt in stmts.iter().rev() {
                            stack.push(ScopedNode::Stmt(stmt, current));
                        }
                    }
                    ArmBody::Value(value) | ArmBody::Ret(value) => {
                        stack.push(ScopedNode::Expr(value, current));
                    }
                }
            }
            stack.push(ScopedNode::Expr(subject, current));
        }
        Expr::ReadPlace { source, .. }
        | Expr::Borrow { source, .. }
        | Expr::Move { source, .. } => {
            let mut place: &Place = source;
            loop {
                match place {
                    Place::Local { .. } => break,
                    Place::Field { base, .. } => place = base.as_ref(),
                    Place::Index { base, index, .. } => {
                        stack.push(ScopedNode::Expr(index, current));
                        place = base.as_ref();
                    }
                }
            }
        }
    }
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

fn push_mut_stmt<'a>(stack: &mut Vec<MutNode<'a>>, stmt: &'a mut Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(MutNode::Binding(binding)),
        Stmt::Assign { target, value } => {
            let mut place = target;
            loop {
                match place {
                    Place::Local { .. } => break,
                    Place::Field { base, .. } => place = base,
                    Place::Index { base, index, .. } => {
                        stack.push(MutNode::Expr(index));
                        place = base;
                    }
                }
            }
            stack.push(MutNode::Expr(value));
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

fn push_mut_expr<'a>(stack: &mut Vec<MutNode<'a>>, expr: &'a mut Expr) {
    match expr {
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..)
        | Expr::Typeof { .. } => {}
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
        | Expr::Move { source, .. } => {
            let mut place: &mut Place = source;
            loop {
                match place {
                    Place::Local { .. } => break,
                    Place::Field { base, .. } => place = base.as_mut(),
                    Place::Index { base, index, .. } => {
                        stack.push(MutNode::Expr(index));
                        place = base.as_mut();
                    }
                }
            }
        }
    }
}
