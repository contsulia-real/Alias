//! Program-point ownership-capability dataflow for explicit consumption operations.
//!
//! The graph builder and worklist are iterative. Ownership analysis processes user-controlled HIR
//! nesting without mapping it onto the compiler's call stack, and joins branches fail-closed by
//! unioning every capability that may already have been moved.

use super::{
    place_relation, ArmBody, BindingId, Body, CheckedProgram, Expr, Item, Place, PlaceRelation,
    Stmt, StorageRelation, StrPart, ValueCategory,
};
use crate::sema::types::Ty;
use crate::{AliasError, AliasResult, Span};
use std::collections::{HashSet, VecDeque};

#[derive(Clone, Copy)]
enum Action<'a> {
    Nop,
    Read(BindingId, Span),
    Move(&'a Place, Span),
    Declare(BindingId),
    Reinitialize(BindingId),
}

struct Node<'a> {
    action: Action<'a>,
    successors: Vec<usize>,
}

#[derive(Clone, Default)]
struct OwnershipState {
    moved: HashSet<BindingId>,
    exposed: HashSet<BindingId>,
}

impl OwnershipState {
    fn join(&mut self, other: &Self) -> bool {
        let moved_before = self.moved.len();
        let exposed_before = self.exposed.len();
        self.moved.extend(other.moved.iter().copied());
        self.exposed.extend(other.exposed.iter().copied());
        self.moved.len() != moved_before || self.exposed.len() != exposed_before
    }

    fn initialize(&mut self, id: BindingId) {
        self.moved.remove(&id);
        self.exposed.remove(&id);
    }
}

#[derive(Clone, Copy, Default)]
struct LoopTargets {
    break_target: Option<usize>,
    continue_target: Option<usize>,
}

enum Task<'a> {
    Expr {
        expr: &'a Expr,
        entry: usize,
        exit: usize,
        replacement: Option<&'a Place>,
        loops: LoopTargets,
    },
    Stmt {
        stmt: &'a Stmt,
        entry: usize,
        exit: usize,
        loops: LoopTargets,
    },
    Stmts {
        stmts: &'a [Stmt],
        entry: usize,
        exit: usize,
        loops: LoopTargets,
    },
    Body {
        body: &'a Body,
        entry: usize,
        exit: usize,
        loops: LoopTargets,
    },
    MatchArm {
        arm: &'a super::MatchArm,
        entry: usize,
        exit: usize,
        loops: LoopTargets,
    },
    PlaceEval {
        place: &'a Place,
        entry: usize,
        exit: usize,
        loops: LoopTargets,
    },
}

struct GraphBuilder<'a> {
    nodes: Vec<Node<'a>>,
    tasks: Vec<Task<'a>>,
    eligible: HashSet<BindingId>,
    nested_functions: Vec<&'a Expr>,
    return_sink: usize,
}

fn error(span: Span, msg: impl Into<String>) -> AliasError {
    AliasError {
        msg: msg.into(),
        span,
    }
}

fn dynamic_owner(ty: &Ty) -> bool {
    super::value_categories::type_carries_dynamic_owner(ty)
}

impl<'a> GraphBuilder<'a> {
    fn new() -> Self {
        let mut graph = Self {
            nodes: Vec::new(),
            tasks: Vec::new(),
            eligible: HashSet::new(),
            nested_functions: Vec::new(),
            return_sink: 0,
        };
        graph.return_sink = graph.node(Action::Nop);
        graph
    }

    fn node(&mut self, action: Action<'a>) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node {
            action,
            successors: Vec::new(),
        });
        id
    }

    fn edge(&mut self, from: usize, to: usize) {
        if !self.nodes[from].successors.contains(&to) {
            self.nodes[from].successors.push(to);
        }
    }

    fn action_between(&mut self, entry: usize, exit: usize, action: Action<'a>) {
        let node = self.node(action);
        self.edge(entry, node);
        self.edge(node, exit);
    }

    fn expression_sequence(
        &mut self,
        expressions: Vec<&'a Expr>,
        entry: usize,
        exit: usize,
        replacement: Option<&'a Place>,
        loops: LoopTargets,
    ) {
        if expressions.is_empty() {
            self.edge(entry, exit);
            return;
        }
        let mut boundaries = Vec::with_capacity(expressions.len() + 1);
        boundaries.push(entry);
        for _ in 1..expressions.len() {
            let boundary = self.node(Action::Nop);
            boundaries.push(boundary);
        }
        boundaries.push(exit);
        for (index, expr) in expressions.into_iter().enumerate().rev() {
            self.tasks.push(Task::Expr {
                expr,
                entry: boundaries[index],
                exit: boundaries[index + 1],
                replacement,
                loops,
            });
        }
    }

    fn build(mut self, task: Task<'a>) -> AliasResult<Self> {
        self.tasks.push(task);
        while let Some(task) = self.tasks.pop() {
            match task {
                Task::Expr {
                    expr,
                    entry,
                    exit,
                    replacement,
                    loops,
                } => self.build_expr(expr, entry, exit, replacement, loops)?,
                Task::Stmt {
                    stmt,
                    entry,
                    exit,
                    loops,
                } => self.build_stmt(stmt, entry, exit, loops)?,
                Task::Stmts {
                    stmts,
                    entry,
                    exit,
                    loops,
                } => self.build_stmts(stmts, entry, exit, loops),
                Task::Body {
                    body,
                    entry,
                    exit,
                    loops,
                } => match body {
                    Body::Block(stmts) => self.tasks.push(Task::Stmts {
                        stmts,
                        entry,
                        exit,
                        loops,
                    }),
                    Body::Single(stmt) => self.tasks.push(Task::Stmt {
                        stmt,
                        entry,
                        exit,
                        loops,
                    }),
                },
                Task::MatchArm {
                    arm,
                    entry,
                    exit,
                    loops,
                } => match &arm.body {
                    ArmBody::Block(stmts) => self.tasks.push(Task::Stmts {
                        stmts,
                        entry,
                        exit,
                        loops,
                    }),
                    ArmBody::Value(value) => self.tasks.push(Task::Expr {
                        expr: value,
                        entry,
                        exit,
                        replacement: None,
                        loops,
                    }),
                    ArmBody::Ret(value) => self.tasks.push(Task::Expr {
                        expr: value,
                        entry,
                        exit: self.return_sink,
                        replacement: None,
                        loops,
                    }),
                },
                Task::PlaceEval {
                    place,
                    entry,
                    exit,
                    loops,
                } => self.build_place_eval(place, entry, exit, loops),
            }
        }
        Ok(self)
    }

    fn build_stmts(&mut self, stmts: &'a [Stmt], entry: usize, exit: usize, loops: LoopTargets) {
        if stmts.is_empty() {
            self.edge(entry, exit);
            return;
        }
        let mut boundaries = Vec::with_capacity(stmts.len() + 1);
        boundaries.push(entry);
        for _ in 1..stmts.len() {
            boundaries.push(self.node(Action::Nop));
        }
        boundaries.push(exit);
        for (index, stmt) in stmts.iter().enumerate().rev() {
            self.tasks.push(Task::Stmt {
                stmt,
                entry: boundaries[index],
                exit: boundaries[index + 1],
                loops,
            });
        }
    }

    fn build_stmt(
        &mut self,
        stmt: &'a Stmt,
        entry: usize,
        exit: usize,
        loops: LoopTargets,
    ) -> AliasResult<()> {
        match stmt {
            Stmt::Binding(binding) => {
                let after_value = self.node(Action::Nop);
                self.tasks.push(Task::Expr {
                    expr: &binding.value,
                    entry,
                    exit: after_value,
                    replacement: None,
                    loops,
                });
                if dynamic_owner(&binding.ty) && binding.relation == Some(StorageRelation::Owning) {
                    self.eligible.insert(binding.binding_id);
                    self.action_between(after_value, exit, Action::Declare(binding.binding_id));
                } else {
                    self.edge(after_value, exit);
                }
            }
            Stmt::Assign { target, value } => {
                let after_value = self.node(Action::Nop);
                let after_place = self.node(Action::Nop);
                self.tasks.push(Task::Expr {
                    expr: value,
                    entry,
                    exit: after_value,
                    replacement: Some(target),
                    loops,
                });
                self.tasks.push(Task::PlaceEval {
                    place: target,
                    entry: after_value,
                    exit: after_place,
                    loops,
                });
                let reinitializes =
                    matches!(value.value_category(), Some(ValueCategory::OwnedTemporary));
                if reinitializes {
                    if let Place::Local { binding_id, .. } = target {
                        if self.eligible.contains(binding_id) {
                            self.action_between(
                                after_place,
                                exit,
                                Action::Reinitialize(*binding_id),
                            );
                            return Ok(());
                        }
                    }
                }
                self.edge(after_place, exit);
            }
            Stmt::Expr { expr } => self.tasks.push(Task::Expr {
                expr,
                entry,
                exit,
                replacement: None,
                loops,
            }),
            Stmt::Return { value } => {
                if let Some(value) = value {
                    self.tasks.push(Task::Expr {
                        expr: value,
                        entry,
                        exit: self.return_sink,
                        replacement: None,
                        loops,
                    });
                } else {
                    self.edge(entry, self.return_sink);
                }
            }
            Stmt::If {
                branches,
                else_body,
            } => {
                let mut condition_entry = entry;
                for (cond, body) in branches {
                    let condition_exit = self.node(Action::Nop);
                    self.tasks.push(Task::Expr {
                        expr: cond,
                        entry: condition_entry,
                        exit: condition_exit,
                        replacement: None,
                        loops,
                    });
                    self.tasks.push(Task::Stmts {
                        stmts: body,
                        entry: condition_exit,
                        exit,
                        loops,
                    });
                    condition_entry = condition_exit;
                }
                if let Some(body) = else_body {
                    self.tasks.push(Task::Stmts {
                        stmts: body,
                        entry: condition_entry,
                        exit,
                        loops,
                    });
                } else {
                    self.edge(condition_entry, exit);
                }
            }
            Stmt::While { cond, body } => {
                let header = self.node(Action::Nop);
                let after_condition = self.node(Action::Nop);
                let body_exit = self.node(Action::Nop);
                self.edge(entry, header);
                self.tasks.push(Task::Expr {
                    expr: cond,
                    entry: header,
                    exit: after_condition,
                    replacement: None,
                    loops,
                });
                self.edge(after_condition, exit);
                self.tasks.push(Task::Stmts {
                    stmts: body,
                    entry: after_condition,
                    exit: body_exit,
                    loops: LoopTargets {
                        break_target: Some(exit),
                        continue_target: Some(header),
                    },
                });
                self.edge(body_exit, header);
            }
            Stmt::For { iterable, body, .. } => {
                let header = self.node(Action::Nop);
                let body_exit = self.node(Action::Nop);
                self.tasks.push(Task::Expr {
                    expr: iterable,
                    entry,
                    exit: header,
                    replacement: None,
                    loops,
                });
                self.edge(header, exit);
                self.tasks.push(Task::Stmts {
                    stmts: body,
                    entry: header,
                    exit: body_exit,
                    loops: LoopTargets {
                        break_target: Some(exit),
                        continue_target: Some(header),
                    },
                });
                self.edge(body_exit, header);
            }
            Stmt::Break => {
                let target = loops.break_target.ok_or_else(|| {
                    error(
                        Span::default(),
                        "内部 sema 不变式被破坏: break 缺少 CFG target",
                    )
                })?;
                self.edge(entry, target);
            }
            Stmt::Continue => {
                let target = loops.continue_target.ok_or_else(|| {
                    error(
                        Span::default(),
                        "内部 sema 不变式被破坏: continue 缺少 CFG target",
                    )
                })?;
                self.edge(entry, target);
            }
        }
        Ok(())
    }

    fn build_expr(
        &mut self,
        expr: &'a Expr,
        entry: usize,
        exit: usize,
        replacement: Option<&'a Place>,
        loops: LoopTargets,
    ) -> AliasResult<()> {
        match expr {
            Expr::Ident(_, Some(id), span, _) => {
                self.action_between(entry, exit, Action::Read(*id, *span));
            }
            Expr::Move { source, span, .. } => {
                if let Some(target) = replacement {
                    if place_relation(target, source) != PlaceRelation::Disjoint {
                        return Err(error(
                            *span,
                            "move source 与 replacement target 无法证明互不重叠",
                        ));
                    }
                }
                self.action_between(entry, exit, Action::Move(source, *span));
            }
            Expr::Str(parts, ..) => {
                let holes = parts
                    .iter()
                    .filter_map(|part| match part {
                        StrPart::Hole(hole) => Some(hole.as_ref()),
                        StrPart::Lit(_) => None,
                    })
                    .collect();
                self.expression_sequence(holes, entry, exit, replacement, loops);
            }
            Expr::Cast { expr, .. }
            | Expr::Convert { expr, .. }
            | Expr::Neg { expr, .. }
            | Expr::Not { expr, .. }
            | Expr::BitNot { expr, .. }
            | Expr::Propagate { expr, .. } => self.tasks.push(Task::Expr {
                expr,
                entry,
                exit,
                replacement,
                loops,
            }),
            Expr::Binary {
                op: super::BinOp::And | super::BinOp::Or,
                lhs,
                rhs,
                ..
            } => {
                let gate = self.node(Action::Nop);
                self.tasks.push(Task::Expr {
                    expr: lhs,
                    entry,
                    exit: gate,
                    replacement,
                    loops,
                });
                self.edge(gate, exit);
                self.tasks.push(Task::Expr {
                    expr: rhs,
                    entry: gate,
                    exit,
                    replacement,
                    loops,
                });
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.expression_sequence(vec![lhs, rhs], entry, exit, replacement, loops)
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
                ..
            } => {
                let gate = self.node(Action::Nop);
                self.tasks.push(Task::Expr {
                    expr: cond,
                    entry,
                    exit: gate,
                    replacement: None,
                    loops,
                });
                for branch in [then_expr.as_ref(), else_expr.as_ref()] {
                    self.tasks.push(Task::Expr {
                        expr: branch,
                        entry: gate,
                        exit,
                        replacement,
                        loops,
                    });
                }
            }
            Expr::Call {
                callee,
                args,
                target,
                ..
            } => {
                let mut expressions = Vec::with_capacity(args.len() + 1);
                if matches!(target, super::CallTarget::FunctionValue) {
                    expressions.push(callee.as_ref());
                }
                expressions.extend(args.iter().map(|arg| &arg.value));
                self.expression_sequence(expressions, entry, exit, replacement, loops);
            }
            Expr::MethodCall { recv, args, .. } => {
                let mut expressions = Vec::with_capacity(args.len() + 1);
                expressions.push(recv.as_ref());
                expressions.extend(args.iter().map(|arg| &arg.value));
                self.expression_sequence(expressions, entry, exit, replacement, loops);
            }
            Expr::Field { recv, .. } => self.tasks.push(Task::Expr {
                expr: recv,
                entry,
                exit,
                replacement,
                loops,
            }),
            Expr::Index { recv, idx, .. } => {
                self.expression_sequence(vec![recv, idx], entry, exit, replacement, loops)
            }
            Expr::ArrayLit { elems, .. } => {
                self.expression_sequence(elems.iter().collect(), entry, exit, replacement, loops)
            }
            Expr::FuncLit { captures, .. } => {
                self.nested_functions.push(expr);
                if captures.is_empty() {
                    self.edge(entry, exit);
                } else {
                    let mut current = entry;
                    for (index, id) in captures.iter().enumerate() {
                        let next = if index + 1 == captures.len() {
                            exit
                        } else {
                            self.node(Action::Nop)
                        };
                        self.action_between(current, next, Action::Read(*id, expr.span()));
                        current = next;
                    }
                }
            }
            Expr::Match { subject, arms, .. } => {
                let gate = self.node(Action::Nop);
                self.tasks.push(Task::Expr {
                    expr: subject,
                    entry,
                    exit: gate,
                    replacement: None,
                    loops,
                });
                for arm in arms {
                    self.tasks.push(Task::MatchArm {
                        arm,
                        entry: gate,
                        exit,
                        loops,
                    });
                }
            }
            Expr::Int(..)
            | Expr::Float(..)
            | Expr::Bool(..)
            | Expr::Ident(_, None, ..)
            | Expr::This(..)
            | Expr::Typeof { .. } => self.edge(entry, exit),
        }
        Ok(())
    }

    fn build_place_eval(
        &mut self,
        place: &'a Place,
        entry: usize,
        exit: usize,
        loops: LoopTargets,
    ) {
        let mut root = place;
        let mut indices = Vec::new();
        while let Place::Field { base, .. } | Place::Index { base, .. } = root {
            if let Place::Index { index, .. } = root {
                indices.push(index.as_ref());
            }
            root = base;
        }
        if matches!(place, Place::Local { .. }) {
            self.edge(entry, exit);
            return;
        }
        let Place::Local {
            binding_id, info, ..
        } = root
        else {
            self.edge(entry, exit);
            return;
        };
        let after_root = self.node(Action::Nop);
        self.action_between(entry, after_root, Action::Read(*binding_id, info.span));
        indices.reverse();
        self.expression_sequence(indices, after_root, exit, None, loops);
    }
}

fn run_dataflow(graph: &GraphBuilder<'_>, entry: usize) -> AliasResult<()> {
    let mut inputs: Vec<Option<OwnershipState>> = vec![None; graph.nodes.len()];
    inputs[entry] = Some(OwnershipState::default());
    let mut queue = VecDeque::from([entry]);
    while let Some(node_id) = queue.pop_front() {
        let mut state = inputs[node_id].clone().unwrap_or_default();
        match graph.nodes[node_id].action {
            Action::Nop => {}
            Action::Read(id, span) => {
                if state.moved.contains(&id) {
                    return Err(error(span, "值已被 move，重新初始化前不能读取"));
                }
                // Ordinary dynamic reads still share the current runtime object. Until their
                // DeepClone/effect contract is resolved, any such read may have exposed an alias,
                // so later claiming exclusive ownership through Move would be unsound.
                if graph.eligible.contains(&id) {
                    state.exposed.insert(id);
                }
            }
            Action::Move(source, span) => {
                if !dynamic_owner(source.ty()) {
                    // Scalar move is ordinary value passing and carries no consumable capability.
                } else {
                    let Place::Local { binding_id, .. } = source else {
                        return Err(error(span, "move source 必须是完整 local owning Place"));
                    };
                    if !graph.eligible.contains(binding_id) {
                        return Err(error(
                            span,
                            "move source 尚未证明为当前函数内的 owning local",
                        ));
                    }
                    if state.exposed.contains(binding_id) {
                        return Err(error(
                            span,
                            "move source 已被先前普通读取或 closure 捕获，无法证明 ownership 唯一",
                        ));
                    }
                    if !state.moved.insert(*binding_id) {
                        return Err(error(span, "ownership capability 已被 move"));
                    }
                }
            }
            Action::Declare(id) | Action::Reinitialize(id) => {
                state.initialize(id);
            }
        }
        for successor in &graph.nodes[node_id].successors {
            let changed = match &mut inputs[*successor] {
                Some(existing) => {
                    existing.join(&state)
                }
                slot @ None => {
                    *slot = Some(state.clone());
                    true
                }
            };
            if changed {
                queue.push_back(*successor);
            }
        }
    }
    Ok(())
}

fn validate_function<'a>(function: &'a Expr, queue: &mut Vec<&'a Expr>) -> AliasResult<()> {
    let Expr::FuncLit { body, .. } = function else {
        return Err(error(
            function.span(),
            "内部 sema 不变式被破坏: ownership flow 入口不是 FuncLit",
        ));
    };
    let mut builder = GraphBuilder::new();
    let entry = builder.node(Action::Nop);
    let exit = builder.node(Action::Nop);
    builder = builder.build(Task::Body {
        body,
        entry,
        exit,
        loops: LoopTargets::default(),
    })?;
    run_dataflow(&builder, entry)?;
    queue.extend(builder.nested_functions);
    Ok(())
}

fn validate_root_expr<'a>(expr: &'a Expr, queue: &mut Vec<&'a Expr>) -> AliasResult<()> {
    if matches!(expr, Expr::FuncLit { .. }) {
        queue.push(expr);
        return Ok(());
    }
    let mut builder = GraphBuilder::new();
    let entry = builder.node(Action::Nop);
    let exit = builder.node(Action::Nop);
    builder = builder.build(Task::Expr {
        expr,
        entry,
        exit,
        replacement: None,
        loops: LoopTargets::default(),
    })?;
    run_dataflow(&builder, entry)?;
    queue.extend(builder.nested_functions);
    Ok(())
}

pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    let mut functions = Vec::new();
    for item in &program.items {
        match item {
            Item::Binding(binding) => validate_root_expr(&binding.value, &mut functions)?,
            Item::StructDef(def) => {
                for field in &def.fields {
                    if let Some(default) = &field.default {
                        validate_root_expr(default, &mut functions)?;
                    }
                }
            }
        }
    }
    while let Some(function) = functions.pop() {
        validate_function(function, &mut functions)?;
    }
    Ok(())
}
