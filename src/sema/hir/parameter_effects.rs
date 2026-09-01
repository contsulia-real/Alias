//! Function parameter/return-effect inference and caller-side call planning.
//!
//! `Ty::Func` is the canonical semantic signature owner. The checker can only construct its
//! type-only shape, so this pass solves body/call dependencies after complete HIR exists, then
//! writes the same frozen effects into function types, HIR parameters, user-method targets,
//! argument/result plans, and return passes. Ownership flow consumes those plans; codegen never
//! re-infers an effect.

use super::{
    ArgumentPass, ArmBody, Binding, BindingId, BindingOwner, Body, BorrowKind, CallArg, CallResult,
    CallTarget, CheckedProgram, Expr, ExprCategory, FunctionId, Item, LoanId, MethodId,
    MethodTarget, OwnershipCapability, Place, PlaceInfo, ResolvedConversion, ReturnPass, Stmt,
    StorageRelation, StrPart, ValueCategory,
};
use crate::sema::types::{ParamEffect, ReturnBorrowSource, ReturnEffect, Ty};
use crate::{AliasError, AliasResult, Span};
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
struct FunctionMeta {
    parameter_ids: Vec<BindingId>,
    implicit_count: usize,
    parameter_types: Vec<Ty>,
    self_id: Option<BindingId>,
    capture_ids: HashSet<BindingId>,
    span: Span,
}

#[derive(Clone)]
struct BindingMeta {
    ty: Ty,
    mutable: bool,
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
    global_bindings: HashSet<BindingId>,
    binding_meta: HashMap<BindingId, BindingMeta>,
    struct_fields: HashMap<String, Vec<bool>>,
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
        global_bindings: HashSet::new(),
        binding_meta: HashMap::new(),
        struct_fields: HashMap::new(),
    };
    let mut stack = Vec::new();
    for item in program.items.iter().rev() {
        match item {
            Item::Binding(binding) => {
                facts.global_bindings.insert(binding.binding_id);
                stack.push(Node::Binding(binding));
            }
            Item::StructDef(def) => {
                facts.struct_fields.insert(
                    def.name.clone(),
                    def.fields.iter().map(|field| field.mutable).collect(),
                );
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
                facts.binding_meta.insert(
                    binding.binding_id,
                    BindingMeta {
                        ty: binding.ty.clone(),
                        mutable: binding.kind == super::BindKind::Var,
                        span: binding.span,
                    },
                );
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
                    captures,
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
                    for (binding_id, ty) in parameter_ids.iter().zip(parameter_types) {
                        facts
                            .binding_meta
                            .entry(*binding_id)
                            .or_insert(BindingMeta {
                                ty: ty.clone(),
                                mutable: false,
                                span: expr.span(),
                            });
                    }
                    facts.function_order.push(*function_id);
                    facts.function_meta.insert(
                        *function_id,
                        FunctionMeta {
                            parameter_ids,
                            implicit_count: implicit_bindings.len(),
                            parameter_types: parameter_types.clone(),
                            self_id: implicit_bindings.first().copied(),
                            capture_ids: captures
                                .iter()
                                .map(|capture| capture.binding_id)
                                .collect(),
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
                if matches!(expr.ty(), Ty::Func { .. }) && !matches!(expr, Expr::Ident(_, None, ..))
                {
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

fn expression_return_effect_map(
    program: &CheckedProgram,
    facts: &ProgramFacts<'_>,
    effects: &HashMap<FunctionId, ReturnEffect>,
) -> AliasResult<HashMap<usize, ReturnEffect>> {
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
                if matches!(expr.ty(), Ty::Func { .. }) && !matches!(expr, Expr::Ident(_, None, ..))
                {
                    let ids = resolve_expr_function_ids(expr, current, facts)?;
                    let mut resolved = None;
                    for id in ids {
                        let candidate = effects.get(&id).copied().ok_or_else(|| {
                            invariant(expr.span(), "function expression 缺少 return effect")
                        })?;
                        if let Some(existing) = resolved {
                            if existing != candidate {
                                return Err(AliasError {
                                    msg: "函数值分支的 return effect / borrow source 不一致".into(),
                                    span: expr.span(),
                                });
                            }
                        } else {
                            resolved = Some(candidate);
                        }
                    }
                    result.insert(
                        expr as *const Expr as usize,
                        resolved.ok_or_else(|| {
                            invariant(expr.span(), "function expression return effect 为空")
                        })?,
                    );
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

fn set_return_effect(ty: &mut Ty, effect: ReturnEffect, span: Span) -> AliasResult<()> {
    let Ty::Func { return_effect, .. } = ty else {
        return Err(invariant(span, "return effect fact 写入非函数类型"));
    };
    *return_effect = Some(effect);
    Ok(())
}

fn apply_signatures(
    program: &mut CheckedProgram,
    effects: &HashMap<FunctionId, Vec<ParamEffect>>,
    return_effects: &HashMap<FunctionId, ReturnEffect>,
    binding_effects: &HashMap<BindingId, Vec<ParamEffect>>,
    binding_return_effects: &HashMap<BindingId, ReturnEffect>,
    expr_effects: &HashMap<usize, Vec<ParamEffect>>,
    expr_return_effects: &HashMap<usize, ReturnEffect>,
) -> AliasResult<()> {
    let mut stack = root_mut_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            MutNode::Binding(binding) => {
                if let Some(resolved) = binding_effects.get(&binding.binding_id) {
                    set_function_effects(&mut binding.ty, resolved, binding.span)?;
                    if let Some(effect) = binding_return_effects.get(&binding.binding_id).copied() {
                        set_return_effect(&mut binding.ty, effect, binding.span)?;
                    }
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
                if let Some(effect) = expr_return_effects.get(&key).copied() {
                    set_return_effect(&mut expr.info_mut().ty, effect, expr_span)?;
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
                    let return_effect = return_effects
                        .get(function_id)
                        .copied()
                        .ok_or_else(|| invariant(expr_span, "FuncLit 缺少 final return effect"))?;
                    set_return_effect(&mut expr.info_mut().ty, return_effect, expr_span)?;
                }
                push_mut_expr(&mut stack, expr);
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
enum ReturnDraft {
    Inline,
    OwnedValue,
    OwnedTransfer(Place),
    BorrowPlace(Place, ReturnBorrowSource),
    BorrowValue(ReturnBorrowSource),
}

impl ReturnDraft {
    fn effect(&self) -> ReturnEffect {
        match self {
            Self::Inline => ReturnEffect::Inline,
            Self::OwnedValue | Self::OwnedTransfer(_) => ReturnEffect::Owned,
            Self::BorrowPlace(_, source) | Self::BorrowValue(source) => {
                ReturnEffect::Borrowed(*source)
            }
        }
    }

    fn into_pass(self) -> ReturnPass {
        match self {
            Self::Inline => ReturnPass::Inline,
            Self::OwnedValue => ReturnPass::OwnedValue,
            Self::OwnedTransfer(source) => ReturnPass::OwnedTransfer { source },
            Self::BorrowPlace(source, origin) => ReturnPass::BorrowPlace { source, origin },
            Self::BorrowValue(origin) => ReturnPass::BorrowValue { origin },
        }
    }
}

fn place_origin(
    place: &Place,
    function_id: FunctionId,
    facts: &ProgramFacts<'_>,
) -> AliasResult<Option<ReturnBorrowSource>> {
    let root = place.root_binding_id();
    let meta = &facts.function_meta[&function_id];
    if meta.self_id == Some(root) {
        return Ok(Some(ReturnBorrowSource::SelfValue));
    }
    if let Some(index) = meta.parameter_ids[meta.implicit_count..]
        .iter()
        .position(|id| *id == root)
    {
        return Ok(Some(ReturnBorrowSource::Parameter(index)));
    }
    if facts.global_bindings.contains(&root) {
        return Ok(Some(ReturnBorrowSource::Global(root)));
    }
    Ok(None)
}

fn validate_borrow_return_origin(
    origin: ReturnBorrowSource,
    function_id: FunctionId,
    facts: &ProgramFacts<'_>,
    span: Span,
) -> AliasResult<()> {
    let meta = &facts.function_meta[&function_id];
    let source_ty = match origin {
        ReturnBorrowSource::Parameter(index) => meta
            .parameter_types
            .get(meta.implicit_count + index)
            .ok_or_else(|| invariant(span, "borrowed return Parameter index 越界"))?,
        ReturnBorrowSource::SelfValue => meta
            .parameter_types
            .first()
            .ok_or_else(|| invariant(span, "borrowed return Self 缺少 receiver type"))?,
        ReturnBorrowSource::Global(_) => return Ok(()),
    };
    if !dynamic_owner(source_ty) {
        return Err(AliasError {
            msg: "InlineValue parameter/self 按值传递，不能作为 borrowed return source".into(),
            span,
        });
    }
    Ok(())
}

fn return_source_from_pass(pass: &ArgumentPass, span: Span) -> AliasResult<Option<Place>> {
    match pass {
        ArgumentPass::ReadBorrow { source, .. } | ArgumentPass::WriteBorrow { source, .. } => {
            Ok(Some(source.clone()))
        }
        ArgumentPass::BorrowTemporary { .. } => Err(AliasError {
            msg: "borrowed return 不能依赖只活到调用结束的 temporary argument".into(),
            span,
        }),
        ArgumentPass::Owned => Err(AliasError {
            msg: "borrowed return source 不能来自已经 transfer 给 callee 的 Owned argument".into(),
            span,
        }),
        ArgumentPass::Inline => Err(invariant(
            span,
            "borrowed return source 指向 inline argument",
        )),
    }
}

fn global_place(binding_id: BindingId, facts: &ProgramFacts<'_>) -> AliasResult<Place> {
    let meta = facts.binding_meta.get(&binding_id).ok_or_else(|| {
        invariant(
            Span::default(),
            "borrowed global source 缺少 Binding metadata",
        )
    })?;
    Ok(Place::Local {
        binding_id,
        info: PlaceInfo {
            ty: meta.ty.clone(),
            span: meta.span,
        },
    })
}

fn map_call_borrow_source(
    source: ReturnBorrowSource,
    receiver: Option<(&Expr, &ArgumentPass)>,
    args: &[CallArg],
    function_id: FunctionId,
    facts: &ProgramFacts<'_>,
    span: Span,
) -> AliasResult<(Place, Option<ReturnBorrowSource>)> {
    let place = match source {
        ReturnBorrowSource::SelfValue => {
            let Some((_, pass)) = receiver else {
                return Err(invariant(span, "Self borrowed return 出现在非方法调用"));
            };
            return_source_from_pass(pass, span)?
                .ok_or_else(|| invariant(span, "Self borrowed return 缺少 stable receiver Place"))?
        }
        ReturnBorrowSource::Parameter(index) => {
            let arg = args.get(index).ok_or_else(|| {
                invariant(span, "borrowed return Parameter index 超出 caller arity")
            })?;
            let pass = arg
                .pass
                .as_ref()
                .ok_or_else(|| invariant(span, "borrowed return argument 缺少 pass fact"))?;
            return_source_from_pass(pass, arg.value.span())?.ok_or_else(|| {
                invariant(span, "borrowed return argument 缺少 stable source Place")
            })?
        }
        ReturnBorrowSource::Global(binding_id) => global_place(binding_id, facts)?,
    };
    let origin = place_origin(&place, function_id, facts)?;
    Ok((place, origin))
}

fn resolve_expr_function_ids(
    expr: &Expr,
    current_function: Option<FunctionId>,
    facts: &ProgramFacts<'_>,
) -> AliasResult<Vec<FunctionId>> {
    let mut stack = vec![expr];
    let mut visited_bindings = HashSet::new();
    let mut ids = Vec::new();
    while let Some(expr) = stack.pop() {
        match expr {
            Expr::FuncLit { function_id, .. } => ids.push(*function_id),
            Expr::Ident(_, Some(binding_id), ..) => {
                if let Some(function_id) = facts.function_bindings.get(binding_id) {
                    ids.push(*function_id);
                } else if visited_bindings.insert(*binding_id) {
                    if let Some(value) = facts.binding_values.get(binding_id) {
                        stack.push(value);
                    }
                }
            }
            Expr::This(..) => ids.push(current_function.ok_or_else(|| {
                invariant(expr.span(), "top-level function expression 不能解析 this")
            })?),
            Expr::Convert {
                expr,
                mode: ResolvedConversion::Identity,
                ..
            } => stack.push(expr),
            Expr::Ternary {
                then_expr,
                else_expr,
                ..
            } => {
                stack.push(else_expr);
                stack.push(then_expr);
            }
            Expr::Match { arms, .. } => {
                for arm in arms {
                    match &arm.body {
                        ArmBody::Value(value) => stack.push(value),
                        ArmBody::Block(_) | ArmBody::Ret(_) => {
                            return Err(invariant(
                                expr.span(),
                                "函数值 match branch 无法解析 return effect",
                            ));
                        }
                    }
                }
            }
            _ => {
                return Err(invariant(
                    expr.span(),
                    "FunctionValue callee 无法恢复 FunctionId",
                ));
            }
        }
    }
    ids.sort_unstable_by_key(|id| id.0);
    ids.dedup();
    if ids.is_empty() {
        return Err(invariant(expr.span(), "函数值表达式没有可解析 FunctionId"));
    }
    Ok(ids)
}

fn resolved_callee_return_effect(
    callee: &Expr,
    current_function: Option<FunctionId>,
    facts: &ProgramFacts<'_>,
    effects: &HashMap<FunctionId, ReturnEffect>,
) -> AliasResult<Option<ReturnEffect>> {
    let ids = resolve_expr_function_ids(callee, current_function, facts)?;
    let mut merged = None;
    for id in ids {
        let Some(candidate) = effects.get(&id).copied() else {
            return Ok(None);
        };
        if let Some(existing) = merged {
            if existing != candidate {
                return Err(AliasError {
                    msg: "函数值分支的 return effect / borrow source 不一致".into(),
                    span: callee.span(),
                });
            }
        } else {
            merged = Some(candidate);
        }
    }
    Ok(merged)
}

fn classify_return_expr(
    value: &Expr,
    function_id: FunctionId,
    parameter_effects: &HashMap<FunctionId, Vec<ParamEffect>>,
    return_effects: &HashMap<FunctionId, ReturnEffect>,
    facts: &ProgramFacts<'_>,
) -> AliasResult<Option<ReturnDraft>> {
    match value {
        Expr::Move { .. } if !dynamic_owner(value.ty()) => Ok(Some(ReturnDraft::Inline)),
        Expr::Move { .. } => Ok(Some(ReturnDraft::OwnedValue)),
        Expr::Borrow { source, .. } => {
            let origin = place_origin(source, function_id, facts)?.ok_or_else(|| AliasError {
                msg: "borrowed return 只能依赖当前函数 parameter、self 或 global source".into(),
                span: value.span(),
            })?;
            validate_borrow_return_origin(origin, function_id, facts, value.span())?;
            Ok(Some(ReturnDraft::BorrowValue(origin)))
        }
        Expr::Call {
            callee,
            args,
            target: CallTarget::FunctionValue,
            ..
        } => {
            let Some(effect) =
                resolved_callee_return_effect(callee, Some(function_id), facts, return_effects)?
            else {
                return Ok(None);
            };
            match effect {
                ReturnEffect::Inline => Ok(Some(ReturnDraft::Inline)),
                ReturnEffect::Owned => Ok(Some(ReturnDraft::OwnedValue)),
                ReturnEffect::Borrowed(source) => {
                    let (_, origin) = map_call_borrow_source(
                        source,
                        None,
                        args,
                        function_id,
                        facts,
                        value.span(),
                    )?;
                    let origin = origin.ok_or_else(|| AliasError {
                        msg: "borrowed return 只能依赖当前函数 parameter、self 或 global source"
                            .into(),
                        span: value.span(),
                    })?;
                    Ok(Some(ReturnDraft::BorrowValue(origin)))
                }
            }
        }
        Expr::MethodCall {
            recv,
            receiver_pass,
            args,
            target: MethodTarget::User { id, .. },
            ..
        } => {
            let callee_id = *facts.method_functions.get(id).ok_or_else(|| {
                invariant(value.span(), "user method return effect 缺少 FunctionId")
            })?;
            let Some(effect) = return_effects.get(&callee_id).copied() else {
                return Ok(None);
            };
            match effect {
                ReturnEffect::Inline => Ok(Some(ReturnDraft::Inline)),
                ReturnEffect::Owned => Ok(Some(ReturnDraft::OwnedValue)),
                ReturnEffect::Borrowed(source) => {
                    let receiver_pass = receiver_pass.as_ref().ok_or_else(|| {
                        invariant(value.span(), "user method receiver 缺少 pass fact")
                    })?;
                    let (_, origin) = map_call_borrow_source(
                        source,
                        Some((recv, receiver_pass)),
                        args,
                        function_id,
                        facts,
                        value.span(),
                    )?;
                    let origin = origin.ok_or_else(|| AliasError {
                        msg: "borrowed return 只能依赖当前函数 parameter、self 或 global source"
                            .into(),
                        span: value.span(),
                    })?;
                    Ok(Some(ReturnDraft::BorrowValue(origin)))
                }
            }
        }
        Expr::Convert {
            expr,
            mode: ResolvedConversion::Identity,
            ..
        } => classify_return_expr(expr, function_id, parameter_effects, return_effects, facts),
        _ if !dynamic_owner(value.ty()) => Ok(Some(ReturnDraft::Inline)),
        _ if value.value_category() == Some(ValueCategory::OwnedTemporary)
            && value.ownership_capability() == Some(OwnershipCapability::Available) =>
        {
            Ok(Some(ReturnDraft::OwnedValue))
        }
        _ => {
            let Some(place) = super::expr_places::from_expr(value) else {
                return Err(AliasError {
                    msg: "dynamic return value 缺少可证明的 ownership / borrow source".into(),
                    span: value.span(),
                });
            };
            let root = place.root_binding_id();
            if facts.borrowed_bindings.contains(&root) {
                return Err(AliasError {
                    msg: "borrowed alias return 的 generation forwarding 尚不能唯一固化".into(),
                    span: value.span(),
                });
            }
            if let Some(origin) = place_origin(&place, function_id, facts)? {
                if let Some(index) = facts.function_meta[&function_id]
                    .parameter_ids
                    .iter()
                    .position(|id| *id == root)
                {
                    let effects = &parameter_effects[&function_id];
                    if effects[index] == ParamEffect::Owned {
                        if !matches!(place, Place::Local { .. }) {
                            return Err(AliasError {
                                msg: "owned return 不能隐式 partial-move parameter projection"
                                    .into(),
                                span: value.span(),
                            });
                        }
                        return Ok(Some(ReturnDraft::OwnedTransfer(place)));
                    }
                }
                return Ok(Some(ReturnDraft::BorrowPlace(place, origin)));
            }
            if facts.function_meta[&function_id]
                .capture_ids
                .contains(&root)
            {
                return Err(AliasError {
                    msg: "captured dynamic value 不能形成可返回的 ownership/borrow source；return effect 无法固化".into(),
                    span: value.span(),
                });
            }
            if matches!(place, Place::Local { .. }) {
                Ok(Some(ReturnDraft::OwnedTransfer(place)))
            } else {
                Err(AliasError {
                    msg: "owned return 不能隐式 partial-move local projection".into(),
                    span: value.span(),
                })
            }
        }
    }
}

fn function_return_values(function: &Expr) -> AliasResult<Vec<&Expr>> {
    let Expr::FuncLit { body, .. } = function else {
        return Err(invariant(function.span(), "return effect 入口不是 FuncLit"));
    };
    let mut values = Vec::new();
    let mut stack = Vec::new();
    push_body(&mut stack, body);
    while let Some(node) = stack.pop() {
        match node {
            Node::Binding(binding) => stack.push(Node::Expr(&binding.value)),
            Node::Stmt(Stmt::Return { value: Some(value) }) => {
                values.push(value);
            }
            Node::Stmt(stmt) => push_stmt_children(&mut stack, stmt),
            Node::Expr(Expr::FuncLit { .. }) => {}
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
                        ArmBody::Ret(value) => values.push(value),
                    }
                }
            }
            Node::Expr(expr) => push_expr_children(&mut stack, expr),
        }
    }
    Ok(values)
}

fn infer_return_effects(
    facts: &ProgramFacts<'_>,
    parameter_effects: &HashMap<FunctionId, Vec<ParamEffect>>,
) -> AliasResult<HashMap<FunctionId, ReturnEffect>> {
    let mut effects = HashMap::new();
    for function_id in &facts.function_order {
        let meta = &facts.function_meta[function_id];
        let Ty::Func { ret, .. } = facts.functions[function_id].ty() else {
            return Err(invariant(
                meta.span,
                "return effect function 缺少 Func type",
            ));
        };
        if **ret == Ty::Unit {
            effects.insert(*function_id, ReturnEffect::Inline);
        }
    }
    let max_iterations = facts.function_order.len().saturating_add(1);
    for _ in 0..max_iterations {
        let mut changed = false;
        for function_id in &facts.function_order {
            let function = facts.functions[function_id];
            let values = function_return_values(function)?;
            let mut inferred = None;
            for value in values {
                let Some(draft) =
                    classify_return_expr(value, *function_id, parameter_effects, &effects, facts)?
                else {
                    continue;
                };
                let candidate = draft.effect();
                if let Some(existing) = inferred {
                    if existing != candidate {
                        return Err(AliasError {
                            msg: "同一函数的 return effect 或 borrowed source 不一致".into(),
                            span: value.span(),
                        });
                    }
                } else {
                    inferred = Some(candidate);
                }
            }
            if let Some(effect) = inferred {
                if let Some(existing) = effects.get(function_id) {
                    if *existing != effect {
                        return Err(AliasError {
                            msg: "递归 return effect 收敛到不一致结果".into(),
                            span: function.span(),
                        });
                    }
                } else {
                    effects.insert(*function_id, effect);
                    changed = true;
                }
            }
        }
        if effects.len() == facts.function_order.len() {
            for function_id in &facts.function_order {
                for value in function_return_values(facts.functions[function_id])? {
                    let draft = classify_return_expr(
                        value,
                        *function_id,
                        parameter_effects,
                        &effects,
                        facts,
                    )?
                    .ok_or_else(|| invariant(value.span(), "收敛后 return dependency 仍未解析"))?;
                    if draft.effect() != effects[function_id] {
                        return Err(AliasError {
                            msg: "同一函数的 return effect 或 borrowed source 不一致".into(),
                            span: value.span(),
                        });
                    }
                }
            }
            return Ok(effects);
        }
        if !changed {
            break;
        }
    }
    let unresolved = facts
        .function_order
        .iter()
        .find(|id| !effects.contains_key(id))
        .copied()
        .ok_or_else(|| invariant(Span::default(), "return effect unresolved set 漂移"))?;
    Err(AliasError {
        msg: "递归 return effect 无法得到唯一安全解".into(),
        span: facts.function_meta[&unresolved].span,
    })
}

fn validate_main_return_effect(
    program: &CheckedProgram,
    facts: &ProgramFacts<'_>,
    return_effects: &HashMap<FunctionId, ReturnEffect>,
) -> AliasResult<()> {
    let function_id = facts
        .function_bindings
        .get(&program.main_id)
        .ok_or_else(|| invariant(Span::default(), "main binding 缺少 FunctionId"))?;
    if return_effects.get(function_id) != Some(&ReturnEffect::Inline) {
        return Err(AliasError {
            msg: "main 必须返回 Inline i32，不能返回 ownership/borrow effect".into(),
            span: facts.function_meta[function_id].span,
        });
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

#[derive(Default)]
struct ReturnMaps {
    call_results: HashMap<usize, CallResult>,
    method_effects: HashMap<usize, ReturnEffect>,
    return_passes: HashMap<usize, ReturnPass>,
}

fn place_terminal_writable(place: &Place, facts: &ProgramFacts<'_>) -> AliasResult<bool> {
    match place {
        Place::Local {
            binding_id, info, ..
        } => facts
            .binding_meta
            .get(binding_id)
            .map(|binding| binding.mutable)
            .ok_or_else(|| invariant(info.span, "borrow source root 缺少 binding metadata")),
        Place::Field {
            base,
            field_index,
            info,
        } => {
            let Ty::Struct(name) = base.ty() else {
                return Err(invariant(info.span, "Field borrow source base 不是 struct"));
            };
            facts
                .struct_fields
                .get(name)
                .and_then(|fields| fields.get(*field_index))
                .copied()
                .ok_or_else(|| invariant(info.span, "Field borrow source 缺少可写性事实"))
        }
        // Direct terminal Index assignment remains closed by the HIR contract, so a borrowed
        // result must not manufacture write permission for the same unresolved target.
        Place::Index { .. } => Ok(false),
    }
}

fn call_result_for_effect(
    effect: ReturnEffect,
    receiver: Option<(&Expr, &ArgumentPass)>,
    args: &[CallArg],
    current_function: Option<FunctionId>,
    facts: &ProgramFacts<'_>,
    next_loan_id: &mut u32,
    span: Span,
) -> AliasResult<CallResult> {
    match effect {
        ReturnEffect::Inline => Ok(CallResult::Inline),
        ReturnEffect::Owned => Ok(CallResult::Owned),
        ReturnEffect::Borrowed(source) => {
            let current_function = current_function.ok_or_else(|| AliasError {
                msg: "borrowed function result 不能存入 top-level/global storage".into(),
                span,
            })?;
            let (source, _) =
                map_call_borrow_source(source, receiver, args, current_function, facts, span)?;
            let loan_id = LoanId(*next_loan_id);
            *next_loan_id = next_loan_id.checked_add(1).ok_or_else(|| AliasError {
                msg: "borrowed call return loan 数量超过编译器上限".into(),
                span,
            })?;
            let source_writable = place_terminal_writable(&source, facts)?;
            Ok(CallResult::Borrowed {
                loan_id,
                source,
                source_writable,
                kind: None,
            })
        }
    }
}

fn collect_return_maps(
    program: &CheckedProgram,
    facts: &ProgramFacts<'_>,
    parameter_effects: &HashMap<FunctionId, Vec<ParamEffect>>,
    return_effects: &HashMap<FunctionId, ReturnEffect>,
    next_loan_id: &mut u32,
) -> AliasResult<ReturnMaps> {
    let mut maps = ReturnMaps::default();
    for function_id in &facts.function_order {
        for value in function_return_values(facts.functions[function_id])? {
            let draft = classify_return_expr(
                value,
                *function_id,
                parameter_effects,
                return_effects,
                facts,
            )?
            .ok_or_else(|| {
                invariant(value.span(), "final return draft 仍依赖 unresolved effect")
            })?;
            let expected = return_effects[function_id];
            if draft.effect() != expected {
                return Err(invariant(
                    value.span(),
                    "return pass 与 final return effect 漂移",
                ));
            }
            if maps
                .return_passes
                .insert(value as *const Expr as usize, draft.into_pass())
                .is_some()
            {
                return Err(invariant(value.span(), "return expression identity 重复"));
            }
        }
    }

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
                        let effect =
                            resolved_callee_return_effect(callee, current, facts, return_effects)?
                                .ok_or_else(|| {
                                    invariant(expr.span(), "final call 缺少 resolved return effect")
                                })?;
                        let result = call_result_for_effect(
                            effect,
                            None,
                            args,
                            current,
                            facts,
                            next_loan_id,
                            expr.span(),
                        )?;
                        maps.call_results
                            .insert(expr as *const Expr as usize, result);
                    }
                    Expr::MethodCall {
                        recv,
                        receiver_pass,
                        args,
                        target: MethodTarget::User { id, .. },
                        ..
                    } => {
                        let function_id = facts.method_functions.get(id).ok_or_else(|| {
                            invariant(expr.span(), "method return effect 缺少 FunctionId")
                        })?;
                        let effect = return_effects.get(function_id).copied().ok_or_else(|| {
                            invariant(expr.span(), "method 缺少 resolved return effect")
                        })?;
                        let receiver_pass = receiver_pass.as_ref().ok_or_else(|| {
                            invariant(expr.span(), "method receiver 缺少 parameter pass")
                        })?;
                        let result = call_result_for_effect(
                            effect,
                            Some((recv, receiver_pass)),
                            args,
                            current,
                            facts,
                            next_loan_id,
                            expr.span(),
                        )?;
                        let key = expr as *const Expr as usize;
                        maps.call_results.insert(key, result);
                        maps.method_effects.insert(key, effect);
                    }
                    _ => {}
                }
                push_scoped_expr(&mut stack, expr, current);
            }
        }
    }
    Ok(maps)
}

fn apply_result_category(expr: &mut Expr, result: &CallResult) -> AliasResult<()> {
    let (category, capability) = match result {
        CallResult::Inline => return Ok(()),
        CallResult::Owned => (
            ExprCategory::Value(ValueCategory::OwnedTemporary),
            Some(OwnershipCapability::Available),
        ),
        CallResult::Borrowed { .. } => (
            ExprCategory::Value(ValueCategory::BorrowedValue),
            Some(OwnershipCapability::None),
        ),
    };
    expr.info_mut().category = Some(category);
    expr.info_mut().ownership_capability = capability;
    Ok(())
}

fn apply_return_maps(program: &mut CheckedProgram, maps: &ReturnMaps) -> AliasResult<()> {
    let mut stack = root_mut_nodes(program);
    let mut seen_calls = HashSet::new();
    let mut seen_returns = HashSet::new();
    while let Some(node) = stack.pop() {
        match node {
            MutNode::Binding(binding) => stack.push(MutNode::Expr(&mut binding.value)),
            MutNode::Stmt(stmt) => push_mut_stmt(&mut stack, stmt),
            MutNode::Expr(expr) => {
                let key = expr as *const Expr as usize;
                let expr_span = expr.span();
                if let Some(pass) = maps.return_passes.get(&key).cloned() {
                    expr.info_mut().return_pass = Some(Box::new(pass));
                    seen_returns.insert(key);
                }
                if let Some(result) = maps.call_results.get(&key).cloned() {
                    match expr {
                        Expr::Call {
                            result: slot,
                            target: CallTarget::FunctionValue,
                            ..
                        } => *slot = Some(Box::new(result.clone())),
                        Expr::MethodCall {
                            result: slot,
                            target: MethodTarget::User { return_effect, .. },
                            ..
                        } => {
                            *slot = Some(Box::new(result.clone()));
                            *return_effect =
                                Some(*maps.method_effects.get(&key).ok_or_else(|| {
                                    invariant(expr_span, "method return target fact 缺失")
                                })?);
                        }
                        _ => return Err(invariant(expr_span, "call result map 指向非 user call")),
                    }
                    apply_result_category(expr, &result)?;
                    seen_calls.insert(key);
                }
                push_mut_expr(&mut stack, expr);
            }
        }
    }
    if seen_calls.len() != maps.call_results.len() || seen_returns.len() != maps.return_passes.len()
    {
        return Err(invariant(
            Span::default(),
            "存在未写回的 call result / return pass fact",
        ));
    }
    Ok(())
}

fn refresh_binding_relations(program: &mut CheckedProgram) -> AliasResult<()> {
    let mut stack = root_mut_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            MutNode::Binding(binding) => {
                binding.relation = super::storage_relations::initial_relation(&binding.value)?;
                stack.push(MutNode::Expr(&mut binding.value));
            }
            MutNode::Stmt(stmt) => push_mut_stmt(&mut stack, stmt),
            MutNode::Expr(expr) => push_mut_expr(&mut stack, expr),
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
    let (
        return_effects,
        binding_return_effects,
        expr_return_effects,
        return_maps,
        final_next_loan_id,
    ) = {
        let facts = collect_facts(program)?;
        let return_effects = infer_return_effects(&facts, &effects)?;
        validate_main_return_effect(program, &facts, &return_effects)?;
        let binding_return_effects = facts
            .function_bindings
            .iter()
            .map(|(binding_id, function_id)| {
                Ok((
                    *binding_id,
                    return_effects.get(function_id).copied().ok_or_else(|| {
                        invariant(
                            facts.function_meta[function_id].span,
                            "binding 缺少 return effect",
                        )
                    })?,
                ))
            })
            .collect::<AliasResult<HashMap<_, _>>>()?;
        let expr_return_effects = expression_return_effect_map(program, &facts, &return_effects)?;
        let mut final_next_loan_id = final_next_loan_id;
        let return_maps = collect_return_maps(
            program,
            &facts,
            &effects,
            &return_effects,
            &mut final_next_loan_id,
        )?;
        (
            return_effects,
            binding_return_effects,
            expr_return_effects,
            return_maps,
            final_next_loan_id,
        )
    };
    apply_signatures(
        program,
        &effects,
        &return_effects,
        &binding_effects,
        &binding_return_effects,
        &expr_effects,
        &expr_return_effects,
    )?;
    apply_return_maps(program, &return_maps)?;
    refresh_binding_relations(program)?;
    *next_loan_id = final_next_loan_id;
    Ok(())
}

fn return_pass_matches(expected: &ReturnDraft, actual: &ReturnPass) -> bool {
    match (expected, actual) {
        (ReturnDraft::Inline, ReturnPass::Inline)
        | (ReturnDraft::OwnedValue, ReturnPass::OwnedValue) => true,
        (ReturnDraft::OwnedTransfer(expected), ReturnPass::OwnedTransfer { source: actual }) => {
            super::expr_places::same_source(expected, actual)
        }
        (
            ReturnDraft::BorrowPlace(expected_place, expected_origin),
            ReturnPass::BorrowPlace {
                source: actual_place,
                origin: actual_origin,
            },
        ) => {
            expected_origin == actual_origin
                && super::expr_places::same_source(expected_place, actual_place)
        }
        (
            ReturnDraft::BorrowValue(expected_origin),
            ReturnPass::BorrowValue {
                origin: actual_origin,
            },
        ) => expected_origin == actual_origin,
        _ => false,
    }
}

fn validate_call_result(
    actual: &Option<Box<CallResult>>,
    expected_effect: ReturnEffect,
    expected_source: Option<&Place>,
    expected_source_writable: Option<bool>,
    span: Span,
    seen_loans: &mut HashSet<LoanId>,
) -> AliasResult<()> {
    match (expected_effect, actual.as_deref()) {
        (ReturnEffect::Inline, Some(CallResult::Inline))
        | (ReturnEffect::Owned, Some(CallResult::Owned)) => Ok(()),
        (
            ReturnEffect::Borrowed(_),
            Some(CallResult::Borrowed {
                loan_id,
                source,
                source_writable,
                kind: Some(_),
            }),
        ) if expected_source
            .is_some_and(|expected| super::expr_places::same_source(expected, source))
            && expected_source_writable == Some(*source_writable) =>
        {
            if !seen_loans.insert(*loan_id) {
                return Err(invariant(span, "borrowed call return LoanId 重复"));
            }
            Ok(())
        }
        _ => Err(invariant(
            span,
            "call result plan 与 resolved return effect 漂移",
        )),
    }
}

fn validate_return_effects(
    program: &CheckedProgram,
    facts: &ProgramFacts<'_>,
    parameter_effects: &HashMap<FunctionId, Vec<ParamEffect>>,
) -> AliasResult<HashMap<FunctionId, ReturnEffect>> {
    let mut frozen = HashMap::new();
    for function_id in &facts.function_order {
        let Ty::Func {
            return_effect: Some(effect),
            ..
        } = facts.functions[function_id].ty()
        else {
            return Err(invariant(
                facts.function_meta[function_id].span,
                "FuncLit 缺少 final return effect",
            ));
        };
        frozen.insert(*function_id, *effect);
    }
    let inferred = infer_return_effects(facts, parameter_effects)?;
    validate_main_return_effect(program, facts, &inferred)?;
    if inferred != frozen {
        let function_id = facts
            .function_order
            .iter()
            .find(|id| inferred.get(id) != frozen.get(id))
            .copied()
            .ok_or_else(|| invariant(Span::default(), "return effect drift set 为空"))?;
        return Err(invariant(
            facts.function_meta[&function_id].span,
            "final return effects 与函数体/call fixed-point 漂移",
        ));
    }

    let expr_effects = expression_return_effect_map(program, facts, &frozen)?;
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
    let mut seen_return_loans = HashSet::new();
    while let Some(node) = stack.pop() {
        match node {
            ScopedNode::Binding(binding, current) => {
                if let Some(function_id) = facts.function_bindings.get(&binding.binding_id) {
                    let Ty::Func {
                        return_effect: Some(actual),
                        ..
                    } = &binding.ty
                    else {
                        return Err(invariant(
                            binding.span,
                            "function binding 缺少 return effect",
                        ));
                    };
                    if frozen.get(function_id) != Some(actual) {
                        return Err(invariant(
                            binding.span,
                            "function binding return effect 漂移",
                        ));
                    }
                }
                stack.push(ScopedNode::Expr(&binding.value, current));
            }
            ScopedNode::Stmt(stmt, current) => push_scoped_stmt(&mut stack, stmt, current),
            ScopedNode::Expr(expr, current) => {
                let key = expr as *const Expr as usize;
                if let Some(expected) = expr_effects.get(&key) {
                    let Ty::Func {
                        return_effect: Some(actual),
                        ..
                    } = expr.ty()
                    else {
                        return Err(invariant(
                            expr.span(),
                            "function expression 缺少 return effect",
                        ));
                    };
                    if actual != expected {
                        return Err(invariant(
                            expr.span(),
                            "function expression return effect 漂移",
                        ));
                    }
                }
                match expr {
                    Expr::Call {
                        callee,
                        args,
                        result,
                        target: CallTarget::FunctionValue,
                        ..
                    } => {
                        let effect =
                            resolved_callee_return_effect(callee, current, facts, &frozen)?
                                .ok_or_else(|| {
                                    invariant(expr.span(), "call return effect unresolved")
                                })?;
                        let source = match effect {
                            ReturnEffect::Borrowed(source) => {
                                let current = current.ok_or_else(|| {
                                    invariant(
                                        expr.span(),
                                        "top-level borrowed user call 缺少 FunctionId",
                                    )
                                })?;
                                Some(
                                    map_call_borrow_source(
                                        source,
                                        None,
                                        args,
                                        current,
                                        facts,
                                        expr.span(),
                                    )?
                                    .0,
                                )
                            }
                            ReturnEffect::Inline | ReturnEffect::Owned => None,
                        };
                        let source_writable = source
                            .as_ref()
                            .map(|source| place_terminal_writable(source, facts))
                            .transpose()?;
                        validate_call_result(
                            result,
                            effect,
                            source.as_ref(),
                            source_writable,
                            expr.span(),
                            &mut seen_return_loans,
                        )?;
                    }
                    Expr::MethodCall {
                        recv,
                        receiver_pass,
                        args,
                        result,
                        target:
                            MethodTarget::User {
                                id, return_effect, ..
                            },
                        ..
                    } => {
                        let function_id = facts.method_functions.get(id).ok_or_else(|| {
                            invariant(expr.span(), "method return effect 缺少 FunctionId")
                        })?;
                        let effect = frozen[function_id];
                        if *return_effect != Some(effect) {
                            return Err(invariant(expr.span(), "method target return effect 漂移"));
                        }
                        let source = match effect {
                            ReturnEffect::Borrowed(source) => {
                                let current = current.ok_or_else(|| {
                                    invariant(
                                        expr.span(),
                                        "top-level borrowed user method 缺少 FunctionId",
                                    )
                                })?;
                                Some(
                                    map_call_borrow_source(
                                        source,
                                        Some((
                                            recv,
                                            receiver_pass.as_ref().ok_or_else(|| {
                                                invariant(expr.span(), "method receiver pass 缺失")
                                            })?,
                                        )),
                                        args,
                                        current,
                                        facts,
                                        expr.span(),
                                    )?
                                    .0,
                                )
                            }
                            ReturnEffect::Inline | ReturnEffect::Owned => None,
                        };
                        let source_writable = source
                            .as_ref()
                            .map(|source| place_terminal_writable(source, facts))
                            .transpose()?;
                        validate_call_result(
                            result,
                            effect,
                            source.as_ref(),
                            source_writable,
                            expr.span(),
                            &mut seen_return_loans,
                        )?;
                    }
                    Expr::Call { result, .. } | Expr::MethodCall { result, .. }
                        if result.is_some() =>
                    {
                        return Err(invariant(expr.span(), "non-user call 携带 CallResult"));
                    }
                    _ => {}
                }
                push_scoped_expr(&mut stack, expr, current);
            }
        }
    }

    for function_id in &facts.function_order {
        for value in function_return_values(facts.functions[function_id])? {
            let draft =
                classify_return_expr(value, *function_id, parameter_effects, &frozen, facts)?
                    .ok_or_else(|| invariant(value.span(), "final return pass 无法重算"))?;
            let actual = value
                .info()
                .return_pass
                .as_ref()
                .ok_or_else(|| invariant(value.span(), "return expression 缺少 ReturnPass"))?;
            if !return_pass_matches(&draft, actual) {
                return Err(invariant(value.span(), "ReturnPass 与 return source 漂移"));
            }
        }
    }
    Ok(frozen)
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
    let _frozen_return_effects = validate_return_effects(program, &facts, &frozen_effects)?;

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
        Stmt::Assign { target, value, .. } => {
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
        Stmt::Assign { target, value, .. } => {
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
        Stmt::Assign { target, value, .. } => {
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
