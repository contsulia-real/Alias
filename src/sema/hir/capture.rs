use super::{ArmBody, BindingId, Body, CheckedProgram, Expr, Item, MatchArm, Stmt, StrPart};
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

pub(super) fn populate_captures(program: &mut CheckedProgram) {
    let globals: HashSet<BindingId> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Binding(binding) if !binding.is_method() => Some(binding.binding_id),
            _ => None,
        })
        .collect();

    // Pass 1: determine each function's complete local BindingId set.
    // FuncLit ordinals are traversal-local bookkeeping, not semantic identity.
    let locals = collect_function_locals(program);

    // Pass 2: source-order use scan with explicit enter/exit events. Child captures
    // are propagated to the suspended parent at child exit, so transitive capture
    // semantics are preserved without recursive host calls.
    let mut captures = collect_function_captures(program, &globals, &locals);

    // Pass 3: deterministic preorder writes each capture vector back to its FuncLit.
    // The final HIR validator runs after this mutation and certifies these vectors.
    apply_captures(program, &mut captures);
}

fn collect_function_locals(program: &CheckedProgram) -> Vec<HashSet<BindingId>> {
    let mut locals: Vec<HashSet<BindingId>> = Vec::new();
    let mut function_stack: Vec<usize> = Vec::new();
    let mut stack = root_nodes(program);

    while let Some(node) = stack.pop() {
        match node {
            Node::ExitFunc(ordinal) => {
                let actual = function_stack
                    .pop()
                    .unwrap_or_else(|| panic!("内部 sema 不变式被破坏: FuncLit locals 栈为空"));
                assert_eq!(
                    actual, ordinal,
                    "内部 sema 不变式被破坏: FuncLit locals 栈错位"
                );
            }
            Node::Stmt(stmt) => {
                if let Some(&ordinal) = function_stack.last() {
                    match stmt {
                        Stmt::Binding(binding) => {
                            locals[ordinal].insert(binding.binding_id);
                        }
                        Stmt::For { binding_id, .. } => {
                            locals[ordinal].insert(*binding_id);
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
                        for arm in arms {
                            if let Some(id) = arm.binding_id {
                                locals[ordinal].insert(id);
                            }
                        }
                    }
                    push_match_children(&mut stack, subject, arms);
                }
                _ => push_expr_children(&mut stack, expr),
            },
        }
    }

    assert!(
        function_stack.is_empty(),
        "内部 sema 不变式被破坏: FuncLit locals 栈未清空"
    );
    locals
}

fn collect_function_captures(
    program: &CheckedProgram,
    globals: &HashSet<BindingId>,
    locals: &[HashSet<BindingId>],
) -> Vec<Vec<BindingId>> {
    let mut captures = vec![Vec::new(); locals.len()];
    let mut frames: Vec<CaptureFrame> = Vec::new();
    let mut next_ordinal = 0usize;
    let mut stack = root_nodes(program);

    while let Some(node) = stack.pop() {
        match node {
            Node::ExitFunc(ordinal) => {
                let frame = frames
                    .pop()
                    .unwrap_or_else(|| panic!("内部 sema 不变式被破坏: FuncLit capture 栈为空"));
                assert_eq!(
                    frame.ordinal, ordinal,
                    "内部 sema 不变式被破坏: FuncLit capture 栈错位"
                );
                captures[ordinal] = frame.ordered;
                if let Some(parent) = frames.last_mut() {
                    for id in captures[ordinal].iter().copied() {
                        record_use(parent, locals, globals, id);
                    }
                }
            }
            Node::Stmt(stmt) => {
                if let Stmt::Assign { target_id, .. } = stmt {
                    if let Some(frame) = frames.last_mut() {
                        record_use(frame, locals, globals, *target_id);
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
                        record_use(frame, locals, globals, *id);
                    }
                }
                Expr::Match { subject, arms, .. } => {
                    push_match_children(&mut stack, subject, arms);
                }
                _ => push_expr_children(&mut stack, expr),
            },
        }
    }

    assert_eq!(
        next_ordinal,
        locals.len(),
        "内部 sema 不变式被破坏: FuncLit 遍历数量变化"
    );
    assert!(
        frames.is_empty(),
        "内部 sema 不变式被破坏: FuncLit capture 栈未清空"
    );
    captures
}

fn record_use(
    frame: &mut CaptureFrame,
    locals: &[HashSet<BindingId>],
    globals: &HashSet<BindingId>,
    id: BindingId,
) {
    if !locals[frame.ordinal].contains(&id) && !globals.contains(&id) && frame.seen.insert(id) {
        frame.ordered.push(id);
    }
}

fn apply_captures(program: &mut CheckedProgram, captures: &mut [Vec<BindingId>]) {
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
                    *target = std::mem::take(&mut captures[ordinal]);
                    push_body_mut(&mut stack, body);
                }
                Expr::Match { subject, arms, .. } => {
                    push_match_children_mut(&mut stack, subject, arms);
                }
                _ => push_expr_children_mut(&mut stack, expr),
            },
        }
    }
    assert_eq!(
        next_ordinal,
        captures.len(),
        "内部 sema 不变式被破坏: FuncLit 写回数量变化"
    );
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

fn push_stmt_children<'a>(stack: &mut Vec<Node<'a>>, stmt: &'a Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(Node::Expr(&binding.value)),
        Stmt::Assign { value, .. } => stack.push(Node::Expr(value)),
        Stmt::FieldAssign { recv, value, .. } => {
            stack.push(Node::Expr(value));
            stack.push(Node::Expr(recv));
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

fn push_stmt_children_mut<'a>(stack: &mut Vec<MutNode<'a>>, stmt: &'a mut Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(MutNode::Expr(&mut binding.value)),
        Stmt::Assign { value, .. } => stack.push(MutNode::Expr(value)),
        Stmt::FieldAssign { recv, value, .. } => {
            stack.push(MutNode::Expr(value));
            stack.push(MutNode::Expr(recv));
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
        Expr::FuncLit { .. }
        | Expr::Typeof { .. }
        | Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..) => {}
    }
}
