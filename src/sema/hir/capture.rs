use super::{
    ArmBody, BindingId, Body, CheckedProgram, Expr, Item, MatchArm, Place, Stmt, StrPart,
};
use crate::{AliasError, AliasResult, Span};
use std::collections::HashSet;

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
    ordered: Vec<BindingId>,
}

pub(super) fn populate_captures(program: &mut CheckedProgram) -> AliasResult<()> {
    let globals: HashSet<BindingId> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Binding(binding) if !binding.is_method() => Some(binding.binding_id),
            _ => None,
        })
        .collect();

    // 第 1 遍收集每个函数的完整局部 BindingId。ordinal 只用于三遍遍历之间对齐，
    // 不是语言 identity；改变任一遍的 preorder 都会把 capture 写入错误 FuncLit。
    let locals = collect_function_locals(program)?;

    // 第 2 遍用显式 enter/exit 事件按源码顺序扫描 use。子函数退出时把其 capture
    // 传播给暂停的父函数，既保留传递捕获，又不把不可信嵌套交给宿主递归。
    let mut captures = collect_function_captures(program, &globals, &locals)?;

    // 第 3 遍以相同 preorder 写回；最终 HIR gate 在 mutation 完成后验证这些向量。
    apply_captures(program, &mut captures)?;
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
) -> AliasResult<Vec<Vec<BindingId>>> {
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
                    for id in slot.iter().copied() {
                        record_use(parent, locals, globals, id)?;
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
                        record_use(frame, locals, globals, *id)?;
                    }
                }
                Expr::ReadPlace { source, .. } | Expr::Move { source, .. } => {
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
    id: BindingId,
) -> AliasResult<()> {
    let local = locals
        .get(frame.ordinal)
        .ok_or_else(|| capture_invariant("capture use 的函数 ordinal 越界"))?;
    if !local.contains(&id) && !globals.contains(&id) && frame.seen.insert(id) {
        frame.ordered.push(id);
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
                record_use(frame, locals, globals, *binding_id)?;
            }
            Place::Field { base, .. } | Place::Index { base, .. } => places.push(base),
        }
    }
    Ok(())
}

fn apply_captures(
    program: &mut CheckedProgram,
    captures: &mut [Vec<BindingId>],
) -> AliasResult<()> {
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
        Expr::ReadPlace { source, .. } | Expr::Move { source, .. } => {
            push_place_expr_children(stack, source)
        }
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
        Expr::ReadPlace { source, .. } | Expr::Move { source, .. } => {
            push_place_expr_children_mut(stack, source)
        }
        Expr::FuncLit { .. }
        | Expr::Typeof { .. }
        | Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..) => {}
    }
}
