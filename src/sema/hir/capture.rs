use super::{
    ArmBody, BindingId, Body, BorrowKind, CallTarget, Capture, CheckedProgram, Expr, Item, LoanId,
    MatchArm, MethodTarget, Place, PlaceInfo, Stmt, StrPart, ValueCategory,
};
use crate::sema::types::types_match;
use crate::{AliasError, AliasResult, Span};
use std::collections::{HashMap, HashSet};

enum Node<'a> {
    Expr(&'a Expr),
    Stmt(&'a Stmt),
    ExitFunc(usize),
}

enum MutNode<'a> {
    Expr(&'a mut Expr),
    Stmt(&'a mut Stmt),
}

struct CaptureFrame {
    ordinal: usize,
    seen: HashSet<BindingId>,
    ordered: Vec<CaptureSeed>,
}

#[derive(Clone)]
struct CaptureSeed {
    binding_id: BindingId,
    source: Place,
}

pub(super) fn populate_captures(
    program: &mut CheckedProgram,
    next_loan_id: &mut u32,
) -> AliasResult<()> {
    let seeds = collect_capture_seeds(program)?;
    let mut captures = materialize_captures(seeds, next_loan_id)?;

    // 第 3 遍以相同 preorder 写回；最终 HIR gate 在 mutation 完成后验证这些向量。
    apply_captures(program, &mut captures)?;
    Ok(())
}

/// Capture kind is a property of the closure body, while its conflict region belongs to the
/// parent's CFG. Functions are therefore inferred child-before-parent, then written back before
/// the unified ownership-flow pass sees any closure creation site.
pub(super) fn finalize_loan_kinds(program: &mut CheckedProgram) -> AliasResult<()> {
    let kinds = infer_loan_kinds(program)?;
    apply_loan_kinds(program, &kinds)
}

pub(super) fn validate_loan_kinds(program: &CheckedProgram) -> AliasResult<()> {
    validate_capture_payloads(program)?;
    let inferred = infer_loan_kinds(program)?;
    let functions = function_preorder(program);
    let mut seen = HashSet::new();
    for function in functions {
        let Expr::FuncLit { captures, .. } = function else {
            return Err(capture_invariant("loan kind validation 入口不是 FuncLit"));
        };
        for capture in captures {
            if capture.kind != inferred.get(&capture.loan_id).copied() {
                return Err(AliasError {
                    msg: "内部 sema 不变式被破坏: capture loan kind 与 body use 漂移".into(),
                    span: capture.source.span(),
                });
            }
            if !seen.insert(capture.loan_id) {
                return Err(capture_invariant("capture LoanId 在 HIR 中重复"));
            }
        }
    }
    if seen.len() != inferred.len() {
        return Err(capture_invariant("存在未验证的 capture loan kind"));
    }
    Ok(())
}

fn collect_capture_seeds(program: &CheckedProgram) -> AliasResult<Vec<Vec<CaptureSeed>>> {
    let globals: HashSet<BindingId> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Binding(binding) if !binding.is_method() => Some(binding.binding_id),
            _ => None,
        })
        .collect();

    // ordinal 只在两次同序显式遍历间对齐，不是语言 identity。新增 HIR child 若只进入
    // 一遍，capture 会被写给错误 FuncLit，因此 final gate 会重算并逐项比较 payload。
    let locals = collect_function_locals(program)?;
    collect_function_captures(program, &globals, &locals)
}

fn validate_capture_payloads(program: &CheckedProgram) -> AliasResult<()> {
    let expected = collect_capture_seeds(program)?;
    let functions = function_preorder(program);
    if expected.len() != functions.len() {
        return Err(capture_invariant("capture payload validation 函数数量漂移"));
    }
    for (function, seeds) in functions.into_iter().zip(expected) {
        let Expr::FuncLit { captures, .. } = function else {
            return Err(capture_invariant(
                "capture payload validation 入口不是 FuncLit",
            ));
        };
        if captures.len() != seeds.len() {
            return Err(capture_invariant("capture 列表与 body free-use 漂移"));
        }
        for (capture, seed) in captures.iter().zip(seeds) {
            let Place::Local {
                binding_id: source_id,
                info,
            } = &capture.source
            else {
                return Err(capture_invariant("capture source 不是 root Local Place"));
            };
            if capture.binding_id != seed.binding_id
                || *source_id != seed.binding_id
                || !types_match(&info.ty, seed.source.ty())
                || info.span != seed.source.span()
            {
                return Err(capture_invariant(
                    "capture BindingId/root Place 与 body free-use 漂移",
                ));
            }
        }
    }
    Ok(())
}

fn infer_loan_kinds(program: &CheckedProgram) -> AliasResult<HashMap<LoanId, BorrowKind>> {
    let functions = function_preorder(program);
    validate_effect_boundaries(&functions)?;
    let mut kinds = HashMap::new();
    for function in functions.into_iter().rev() {
        let function_kinds =
            super::ownership_flow::infer_capture_kinds_for_function(function, &kinds)?;
        for (loan_id, kind) in function_kinds {
            if kinds.insert(loan_id, kind).is_some() {
                return Err(capture_invariant("capture LoanId 跨函数重复"));
            }
        }
    }
    Ok(kinds)
}

fn expr_uses_capture(expr: &Expr, captures: &HashSet<BindingId>) -> bool {
    let mut stack = vec![Node::Expr(expr)];
    while let Some(node) = stack.pop() {
        match node {
            Node::ExitFunc(_) => {}
            Node::Stmt(stmt) => push_stmt_children(&mut stack, stmt),
            Node::Expr(expr) => {
                match expr {
                    Expr::Ident(_, Some(id), ..) if captures.contains(id) => return true,
                    Expr::ReadPlace { source, .. }
                    | Expr::Borrow { source, .. }
                    | Expr::Move { source, .. }
                        if captures.contains(&source.root_binding_id()) =>
                    {
                        return true;
                    }
                    // A nested closure owns a separate capture contract. Counting its body here
                    // would turn a deferred use into an immediate call/return effect.
                    Expr::FuncLit { .. } => continue,
                    _ => {}
                }
                push_expr_children(&mut stack, expr);
            }
        }
    }
    false
}

fn validate_effect_boundaries(functions: &[&Expr]) -> AliasResult<()> {
    for function in functions {
        let Expr::FuncLit { captures, body, .. } = function else {
            return Err(capture_invariant("effect boundary 入口不是 FuncLit"));
        };
        let capture_ids: HashSet<BindingId> =
            captures.iter().map(|capture| capture.binding_id).collect();
        if capture_ids.is_empty() {
            continue;
        }
        let mut stack = Vec::new();
        push_body(&mut stack, body);
        while let Some(node) = stack.pop() {
            match node {
                Node::ExitFunc(_) => {}
                Node::Stmt(Stmt::Return { value: Some(value) })
                    if super::value_categories::type_carries_dynamic_owner(value.ty())
                        && value.value_category() != Some(ValueCategory::OwnedTemporary)
                        && expr_uses_capture(value, &capture_ids) =>
                {
                    return Err(AliasError {
                        msg: "captured dynamic value 的 return effect 尚未固化".into(),
                        span: value.span(),
                    });
                }
                Node::Stmt(stmt) => push_stmt_children(&mut stack, stmt),
                Node::Expr(expr) => {
                    match expr {
                        Expr::Call {
                            args,
                            target: CallTarget::FunctionValue,
                            ..
                        } if args
                            .iter()
                            .any(|arg| expr_uses_capture(&arg.value, &capture_ids)) =>
                        {
                            return Err(AliasError {
                                msg: "captured argument 的 function parameter effect 尚未固化"
                                    .into(),
                                span: expr.span(),
                            });
                        }
                        Expr::MethodCall {
                            recv,
                            args,
                            target: MethodTarget::User { .. },
                            ..
                        } if expr_uses_capture(recv, &capture_ids)
                            || args
                                .iter()
                                .any(|arg| expr_uses_capture(&arg.value, &capture_ids)) =>
                        {
                            return Err(AliasError {
                                msg: "captured receiver/argument 的 user method effect 尚未固化"
                                    .into(),
                                span: expr.span(),
                            });
                        }
                        Expr::FuncLit { .. } => continue,
                        _ => {}
                    }
                    push_expr_children(&mut stack, expr);
                }
            }
        }
    }
    Ok(())
}

fn function_preorder(program: &CheckedProgram) -> Vec<&Expr> {
    let mut functions = Vec::new();
    let mut stack = root_nodes(program);
    while let Some(node) = stack.pop() {
        match node {
            Node::ExitFunc(_) => {}
            Node::Stmt(stmt) => push_stmt_children(&mut stack, stmt),
            Node::Expr(expr) => {
                if let Expr::FuncLit { body, .. } = expr {
                    functions.push(expr);
                    push_body(&mut stack, body);
                } else {
                    push_expr_children(&mut stack, expr);
                }
            }
        }
    }
    functions
}

fn apply_loan_kinds(
    program: &mut CheckedProgram,
    kinds: &HashMap<LoanId, BorrowKind>,
) -> AliasResult<()> {
    let mut seen = HashSet::new();
    let mut stack = root_nodes_mut(program);
    while let Some(node) = stack.pop() {
        match node {
            MutNode::Stmt(stmt) => push_stmt_children_mut(&mut stack, stmt),
            MutNode::Expr(expr) => {
                if let Expr::FuncLit { captures, body, .. } = expr {
                    for capture in captures {
                        capture.kind =
                            Some(kinds.get(&capture.loan_id).copied().ok_or_else(|| {
                                capture_invariant("capture 缺少 inferred loan kind")
                            })?);
                        if !seen.insert(capture.loan_id) {
                            return Err(capture_invariant("capture LoanId 在 HIR 中重复"));
                        }
                    }
                    push_body_mut(&mut stack, body);
                } else {
                    push_expr_children_mut(&mut stack, expr);
                }
            }
        }
    }
    if seen.len() != kinds.len() {
        return Err(capture_invariant("存在未写回的 capture loan kind"));
    }
    Ok(())
}

fn capture_invariant(msg: impl Into<String>) -> AliasError {
    AliasError {
        msg: format!("内部 sema capture 不变式被破坏: {}", msg.into()),
        span: Span::default(),
    }
}

fn collect_function_locals(program: &CheckedProgram) -> AliasResult<Vec<HashSet<BindingId>>> {
    let mut locals: Vec<HashSet<BindingId>> = Vec::new();
    let mut function_stack: Vec<usize> = Vec::new();
    let mut stack = root_nodes(program);

    while let Some(node) = stack.pop() {
        match node {
            Node::ExitFunc(ordinal) => {
                let actual = function_stack
                    .pop()
                    .ok_or_else(|| capture_invariant("FuncLit locals 栈为空"))?;
                if actual != ordinal {
                    return Err(capture_invariant("FuncLit locals 栈错位"));
                }
            }
            Node::Stmt(stmt) => {
                if let Some(&ordinal) = function_stack.last() {
                    match stmt {
                        Stmt::Binding(binding) => {
                            locals
                                .get_mut(ordinal)
                                .ok_or_else(|| capture_invariant("局部函数 ordinal 越界"))?
                                .insert(binding.binding_id);
                        }
                        Stmt::For { binding_id, .. } => {
                            locals
                                .get_mut(ordinal)
                                .ok_or_else(|| capture_invariant("for 函数 ordinal 越界"))?
                                .insert(*binding_id);
                        }
                        _ => {}
                    }
                }
                push_stmt_children(&mut stack, stmt);
            }
            Node::Expr(expr) => match expr {
                Expr::FuncLit {
                    params,
                    implicit_bindings,
                    body,
                    ..
                } => {
                    let ordinal = locals.len();
                    let mut set: HashSet<BindingId> =
                        params.iter().map(|param| param.binding_id).collect();
                    set.extend(implicit_bindings.iter().copied());
                    locals.push(set);
                    function_stack.push(ordinal);
                    stack.push(Node::ExitFunc(ordinal));
                    push_body(&mut stack, body);
                }
                Expr::Match { subject, arms, .. } => {
                    if let Some(&ordinal) = function_stack.last() {
                        let local = locals
                            .get_mut(ordinal)
                            .ok_or_else(|| capture_invariant("match 函数 ordinal 越界"))?;
                        for arm in arms {
                            if let Some(id) = arm.binding_id {
                                local.insert(id);
                            }
                        }
                    }
                    push_match_children(&mut stack, subject, arms);
                }
                _ => push_expr_children(&mut stack, expr),
            },
        }
    }

    if !function_stack.is_empty() {
        return Err(capture_invariant("FuncLit locals 栈未清空"));
    }
    Ok(locals)
}

fn collect_function_captures(
    program: &CheckedProgram,
    globals: &HashSet<BindingId>,
    locals: &[HashSet<BindingId>],
) -> AliasResult<Vec<Vec<CaptureSeed>>> {
    let mut captures = vec![Vec::new(); locals.len()];
    let mut frames: Vec<CaptureFrame> = Vec::new();
    let mut next_ordinal = 0usize;
    let mut stack = root_nodes(program);

    while let Some(node) = stack.pop() {
        match node {
            Node::ExitFunc(ordinal) => {
                let frame = frames
                    .pop()
                    .ok_or_else(|| capture_invariant("FuncLit capture 栈为空"))?;
                if frame.ordinal != ordinal {
                    return Err(capture_invariant("FuncLit capture 栈错位"));
                }
                let slot = captures
                    .get_mut(ordinal)
                    .ok_or_else(|| capture_invariant("FuncLit capture ordinal 越界"))?;
                *slot = frame.ordered;
                if let Some(parent) = frames.last_mut() {
                    for seed in slot.iter() {
                        record_use(parent, locals, globals, seed.clone())?;
                    }
                }
            }
            Node::Stmt(stmt) => {
                if let Stmt::Assign { target, .. } = stmt {
                    if let Some(frame) = frames.last_mut() {
                        record_place_uses(frame, locals, globals, target)?;
                    }
                }
                push_stmt_children(&mut stack, stmt);
            }
            Node::Expr(expr) => match expr {
                Expr::FuncLit { body, .. } => {
                    let ordinal = next_ordinal;
                    next_ordinal += 1;
                    frames.push(CaptureFrame {
                        ordinal,
                        seen: HashSet::new(),
                        ordered: Vec::new(),
                    });
                    stack.push(Node::ExitFunc(ordinal));
                    push_body(&mut stack, body);
                }
                Expr::Ident(_, Some(id), ..) => {
                    if let Some(frame) = frames.last_mut() {
                        record_use(
                            frame,
                            locals,
                            globals,
                            CaptureSeed {
                                binding_id: *id,
                                source: Place::Local {
                                    binding_id: *id,
                                    info: PlaceInfo {
                                        ty: expr.ty().clone(),
                                        span: expr.span(),
                                    },
                                },
                            },
                        )?;
                    }
                }
                Expr::ReadPlace { source, .. }
                | Expr::Borrow { source, .. }
                | Expr::Move { source, .. } => {
                    if let Some(frame) = frames.last_mut() {
                        record_place_uses(frame, locals, globals, source)?;
                    }
                    push_place_expr_children(&mut stack, source);
                }
                Expr::Match { subject, arms, .. } => {
                    push_match_children(&mut stack, subject, arms);
                }
                _ => push_expr_children(&mut stack, expr),
            },
        }
    }

    if next_ordinal != locals.len() {
        return Err(capture_invariant("FuncLit 遍历数量变化"));
    }
    if !frames.is_empty() {
        return Err(capture_invariant("FuncLit capture 栈未清空"));
    }
    Ok(captures)
}

fn record_use(
    frame: &mut CaptureFrame,
    locals: &[HashSet<BindingId>],
    globals: &HashSet<BindingId>,
    seed: CaptureSeed,
) -> AliasResult<()> {
    let local = locals
        .get(frame.ordinal)
        .ok_or_else(|| capture_invariant("capture use 的函数 ordinal 越界"))?;
    let id = seed.binding_id;
    if !local.contains(&id) && !globals.contains(&id) && frame.seen.insert(id) {
        frame.ordered.push(seed);
    }
    Ok(())
}

fn record_place_uses(
    frame: &mut CaptureFrame,
    locals: &[HashSet<BindingId>],
    globals: &HashSet<BindingId>,
    place: &Place,
) -> AliasResult<()> {
    let mut places = vec![place];
    while let Some(place) = places.pop() {
        match place {
            Place::Local { binding_id, .. } => {
                record_use(
                    frame,
                    locals,
                    globals,
                    CaptureSeed {
                        binding_id: *binding_id,
                        source: place.clone(),
                    },
                )?;
            }
            Place::Field { base, .. } | Place::Index { base, .. } => places.push(base),
        }
    }
    Ok(())
}

fn materialize_captures(
    seeds: Vec<Vec<CaptureSeed>>,
    next_loan_id: &mut u32,
) -> AliasResult<Vec<Vec<Capture>>> {
    seeds
        .into_iter()
        .map(|function| {
            function
                .into_iter()
                .map(|seed| {
                    let loan_id = LoanId(*next_loan_id);
                    *next_loan_id = next_loan_id.checked_add(1).ok_or_else(|| {
                        capture_invariant("LoanId 在 capture materialization 中耗尽")
                    })?;
                    Ok(Capture {
                        binding_id: seed.binding_id,
                        loan_id,
                        source: seed.source,
                        kind: None,
                    })
                })
                .collect()
        })
        .collect()
}

fn apply_captures(program: &mut CheckedProgram, captures: &mut [Vec<Capture>]) -> AliasResult<()> {
    let mut next_ordinal = 0usize;
    let mut stack = root_nodes_mut(program);
    while let Some(node) = stack.pop() {
        match node {
            MutNode::Stmt(stmt) => push_stmt_children_mut(&mut stack, stmt),
            MutNode::Expr(expr) => match expr {
                Expr::FuncLit {
                    captures: target,
                    body,
                    ..
                } => {
                    let ordinal = next_ordinal;
                    next_ordinal += 1;
                    let source = captures
                        .get_mut(ordinal)
                        .ok_or_else(|| capture_invariant("FuncLit capture 写回 ordinal 越界"))?;
                    *target = std::mem::take(source);
                    push_body_mut(&mut stack, body);
                }
                Expr::Match { subject, arms, .. } => {
                    push_match_children_mut(&mut stack, subject, arms);
                }
                _ => push_expr_children_mut(&mut stack, expr),
            },
        }
    }
    if next_ordinal != captures.len() {
        return Err(capture_invariant("FuncLit 写回数量变化"));
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

fn root_nodes_mut(program: &mut CheckedProgram) -> Vec<MutNode<'_>> {
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
            // 三遍 capture traversal 必须保持同一 preorder；Place 内只有 Index expression
            // 仍属于可求值 child，Local/Field identity 本身由 record_place_uses 处理。
            stack.push(Node::Expr(value));
            push_place_expr_children(stack, target);
        }
        Stmt::Expr { expr, .. } => stack.push(Node::Expr(expr)),
        Stmt::Return { value, .. } => {
            if let Some(value) = value {
                stack.push(Node::Expr(value));
            }
        }
        Stmt::If {
            branches,
            else_body,
            ..
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
        Stmt::While { cond, body, .. } => {
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
        Expr::Match { subject, arms, .. } => push_match_children(stack, subject, arms),
        Expr::ReadPlace { source, .. }
        | Expr::Borrow { source, .. }
        | Expr::Move { source, .. } => push_place_expr_children(stack, source),
        Expr::FuncLit { .. }
        | Expr::Typeof { .. }
        | Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..) => {}
    }
}

fn push_body_mut<'a>(stack: &mut Vec<MutNode<'a>>, body: &'a mut Body) {
    match body {
        Body::Block(stmts) => {
            for stmt in stmts.iter_mut().rev() {
                stack.push(MutNode::Stmt(stmt));
            }
        }
        Body::Single(stmt) => stack.push(MutNode::Stmt(stmt)),
    }
}

fn push_place_expr_children_mut<'a>(stack: &mut Vec<MutNode<'a>>, place: &'a mut Place) {
    let mut places = vec![place];
    while let Some(place) = places.pop() {
        match place {
            Place::Local { .. } => {}
            Place::Field { base, .. } => places.push(base.as_mut()),
            Place::Index { base, index, .. } => {
                stack.push(MutNode::Expr(index.as_mut()));
                places.push(base.as_mut());
            }
        }
    }
}

fn push_stmt_children_mut<'a>(stack: &mut Vec<MutNode<'a>>, stmt: &'a mut Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(MutNode::Expr(&mut binding.value)),
        Stmt::Assign { target, value } => {
            stack.push(MutNode::Expr(value));
            push_place_expr_children_mut(stack, target);
        }
        Stmt::Expr { expr, .. } => stack.push(MutNode::Expr(expr)),
        Stmt::Return { value, .. } => {
            if let Some(value) = value {
                stack.push(MutNode::Expr(value));
            }
        }
        Stmt::If {
            branches,
            else_body,
            ..
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
        Stmt::While { cond, body, .. } => {
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

fn push_match_children_mut<'a>(
    stack: &mut Vec<MutNode<'a>>,
    subject: &'a mut Expr,
    arms: &'a mut [MatchArm],
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

fn push_expr_children_mut<'a>(stack: &mut Vec<MutNode<'a>>, expr: &'a mut Expr) {
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
        Expr::Match { subject, arms, .. } => push_match_children_mut(stack, subject, arms),
        Expr::ReadPlace { source, .. }
        | Expr::Borrow { source, .. }
        | Expr::Move { source, .. } => push_place_expr_children_mut(stack, source),
        Expr::FuncLit { .. }
        | Expr::Typeof { .. }
        | Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..) => {}
    }
}
