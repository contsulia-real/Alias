//! Program-point ownership-capability dataflow for explicit consumption operations.
//!
//! The graph builder and worklist are iterative. Ownership analysis processes user-controlled HIR
//! nesting without mapping it onto the compiler's call stack, and joins branches fail-closed by
//! unioning every capability that may already have been moved.

use super::{
    place_relation, ArmBody, BindingId, Body, BorrowKind, CheckedProgram, Expr, Item, LoanId,
    Place, PlaceRelation, ResolvedConversion, Stmt, StorageRelation, StrPart, ValueCategory,
};
use crate::sema::types::Ty;
use crate::{AliasError, AliasResult, Span};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccessKind {
    Read,
    Write,
}

#[derive(Clone, Copy)]
struct PlaceEvalMode {
    read_root: bool,
    exposes_alias: bool,
    access: AccessKind,
}

#[derive(Clone, Copy)]
struct BorrowSpec<'a> {
    loan_id: LoanId,
    source: &'a Place,
    declared_kind: Option<BorrowKind>,
    span: Span,
}

#[derive(Clone, Copy)]
enum Action<'a> {
    Nop,
    Read(BindingId, AccessKind, Span),
    CloneRead(&'a Place, Span),
    Borrow {
        loan_id: LoanId,
        source: &'a Place,
        declared_kind: Option<BorrowKind>,
        span: Span,
    },
    BindLoan(BindingId, LoanId),
    Write(&'a Place, Span),
    Move(&'a Place, Span),
    Declare(BindingId),
    Reinitialize(BindingId, Span),
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
        mode: PlaceEvalMode,
    },
}

struct GraphBuilder<'a> {
    nodes: Vec<Node<'a>>,
    tasks: Vec<Task<'a>>,
    eligible: HashSet<BindingId>,
    owning: HashSet<BindingId>,
    borrowed: HashSet<BindingId>,
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

fn resolved_borrow(expr: &Expr) -> Option<(LoanId, &Place)> {
    let mut current = expr;
    loop {
        match current {
            Expr::Borrow {
                loan_id, source, ..
            } => return Some((*loan_id, source)),
            Expr::Convert {
                expr,
                mode: ResolvedConversion::Identity,
                ..
            } => current = expr,
            _ => return None,
        }
    }
}

fn root_binding(place: &Place) -> BindingId {
    let mut current = place;
    loop {
        match current {
            Place::Local { binding_id, .. } => return *binding_id,
            Place::Field { base, .. } | Place::Index { base, .. } => current = base,
        }
    }
}

impl<'a> GraphBuilder<'a> {
    fn new() -> Self {
        let mut graph = Self {
            nodes: Vec::new(),
            tasks: Vec::new(),
            eligible: HashSet::new(),
            owning: HashSet::new(),
            borrowed: HashSet::new(),
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
                    mode,
                } => self.build_place_eval(place, entry, exit, loops, mode),
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
                if binding.relation == Some(StorageRelation::Borrowed) {
                    self.borrowed.insert(binding.binding_id);
                    let Some((loan_id, _)) = resolved_borrow(&binding.value) else {
                        return Err(error(
                            binding.span,
                            "内部 sema 不变式被破坏: borrowed binding initializer 不是 BorrowedValue",
                        ));
                    };
                    self.action_between(
                        after_value,
                        exit,
                        Action::BindLoan(binding.binding_id, loan_id),
                    );
                } else if binding.relation == Some(StorageRelation::Owning) {
                    self.owning.insert(binding.binding_id);
                    if dynamic_owner(&binding.ty) {
                        self.eligible.insert(binding.binding_id);
                        self.action_between(after_value, exit, Action::Declare(binding.binding_id));
                    } else {
                        self.edge(after_value, exit);
                    }
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
                    mode: PlaceEvalMode {
                        read_root: false,
                        exposes_alias: true,
                        access: AccessKind::Write,
                    },
                });
                if let Place::Local { binding_id, .. } = target {
                    if self.borrowed.contains(binding_id) {
                        let Some((loan_id, _)) = resolved_borrow(value) else {
                            return Err(error(
                                value.span(),
                                "borrowed alias 重新绑定只能接收 BorrowedValue",
                            ));
                        };
                        self.action_between(
                            after_place,
                            exit,
                            Action::BindLoan(*binding_id, loan_id),
                        );
                        return Ok(());
                    }
                    if matches!(value.value_category(), Some(ValueCategory::OwnedTemporary))
                        && self.eligible.contains(binding_id)
                    {
                        self.action_between(
                            after_place,
                            exit,
                            Action::Reinitialize(*binding_id, target.span()),
                        );
                        return Ok(());
                    }
                }
                let after_write = self.node(Action::Nop);
                if self.borrowed.contains(&root_binding(target)) {
                    self.edge(after_place, after_write);
                } else {
                    self.action_between(
                        after_place,
                        after_write,
                        Action::Write(target, target.span()),
                    );
                }
                self.edge(after_write, exit);
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
                self.action_between(entry, exit, Action::Read(*id, AccessKind::Read, *span));
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
            Expr::ReadPlace { source, .. } => self.tasks.push(Task::PlaceEval {
                place: source,
                entry,
                exit,
                loops,
                mode: PlaceEvalMode {
                    read_root: true,
                    exposes_alias: false,
                    access: AccessKind::Read,
                },
            }),
            Expr::Borrow {
                loan_id,
                source,
                kind,
                span,
                ..
            } => self.build_borrow(
                BorrowSpec {
                    loan_id: *loan_id,
                    source,
                    declared_kind: *kind,
                    span: *span,
                },
                entry,
                exit,
                loops,
            ),
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
                if matches!(
                    target,
                    super::CallTarget::Builtin(
                        super::BuiltinCall::Increase | super::BuiltinCall::Decrease
                    )
                ) {
                    let [arg] = args.as_slice() else {
                        return Err(error(
                            expr.span(),
                            "内部 sema 不变式被破坏: increase/decrease 元数漂移",
                        ));
                    };
                    let Expr::Ident(_, Some(id), span, _) = &arg.value else {
                        return Err(error(
                            arg.value.span(),
                            "内部 sema 不变式被破坏: increase/decrease target 未解析为 binding",
                        ));
                    };
                    self.action_between(entry, exit, Action::Read(*id, AccessKind::Write, *span));
                    return Ok(());
                }
                let mut expressions = Vec::with_capacity(args.len() + 1);
                if matches!(target, super::CallTarget::FunctionValue) {
                    expressions.push(callee.as_ref());
                }
                expressions.extend(args.iter().map(|arg| &arg.value));
                self.expression_sequence(expressions, entry, exit, replacement, loops);
            }
            Expr::MethodCall {
                recv, args, target, ..
            } => {
                if matches!(
                    target,
                    super::MethodTarget::ArrayPush | super::MethodTarget::ArrayPop
                ) {
                    if let Expr::Ident(_, Some(id), span, _) = recv.as_ref() {
                        let after_receiver = self.node(Action::Nop);
                        self.action_between(
                            entry,
                            after_receiver,
                            Action::Read(*id, AccessKind::Write, *span),
                        );
                        self.expression_sequence(
                            args.iter().map(|arg| &arg.value).collect(),
                            after_receiver,
                            exit,
                            replacement,
                            loops,
                        );
                        return Ok(());
                    }
                }
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
                        self.action_between(
                            current,
                            next,
                            Action::Read(*id, AccessKind::Read, expr.span()),
                        );
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
        mode: PlaceEvalMode,
    ) {
        let PlaceEvalMode {
            read_root,
            exposes_alias,
            access,
        } = mode;
        let mut root = place;
        let mut indices = Vec::new();
        while let Place::Field { base, .. } | Place::Index { base, .. } = root {
            if let Place::Index { index, .. } = root {
                indices.push(index.as_ref());
            }
            root = base;
        }
        if let Place::Local {
            binding_id, info, ..
        } = place
        {
            if read_root {
                let action = if exposes_alias || self.borrowed.contains(binding_id) {
                    Action::Read(*binding_id, access, info.span)
                } else {
                    Action::CloneRead(place, info.span)
                };
                self.action_between(entry, exit, action);
            } else {
                self.edge(entry, exit);
            }
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
        // Projecting an owning target address is not a semantic read of the entire root. The exact
        // Write action below owns its conflict check; marking this as a root write would reject a
        // write to a field proven disjoint from a live loan. A borrowed root is different: using
        // its alias to reach the target is the write-through use that determines WriteLoan.
        if read_root || self.borrowed.contains(binding_id) {
            let action = if exposes_alias || self.borrowed.contains(binding_id) {
                Action::Read(*binding_id, access, info.span)
            } else {
                Action::CloneRead(place, info.span)
            };
            self.action_between(entry, after_root, action);
        } else {
            self.edge(entry, after_root);
        }
        indices.reverse();
        self.expression_sequence(indices, after_root, exit, None, loops);
    }

    fn build_borrow(
        &mut self,
        spec: BorrowSpec<'a>,
        entry: usize,
        exit: usize,
        loops: LoopTargets,
    ) {
        let BorrowSpec {
            loan_id,
            source,
            declared_kind,
            span,
        } = spec;
        let mut root = source;
        let mut indices = Vec::new();
        while let Place::Field { base, .. } | Place::Index { base, .. } = root {
            if let Place::Index { index, .. } = root {
                indices.push(index.as_ref());
            }
            root = base;
        }
        indices.reverse();
        if indices.is_empty() {
            self.action_between(
                entry,
                exit,
                Action::Borrow {
                    loan_id,
                    source,
                    declared_kind,
                    span,
                },
            );
            return;
        }
        let after_indices = self.node(Action::Nop);
        self.expression_sequence(indices, entry, after_indices, None, loops);
        self.action_between(
            after_indices,
            exit,
            Action::Borrow {
                loan_id,
                source,
                declared_kind,
                span,
            },
        );
    }
}

type ReachingState = HashMap<BindingId, HashSet<LoanId>>;

fn join_reaching(target: &mut ReachingState, source: &ReachingState) -> bool {
    let mut changed = false;
    for (binding, loans) in source {
        let target_loans = target.entry(*binding).or_default();
        let before = target_loans.len();
        target_loans.extend(loans.iter().copied());
        changed |= target_loans.len() != before;
    }
    changed
}

fn compute_reaching(graph: &GraphBuilder<'_>, entry: usize) -> Vec<Option<ReachingState>> {
    let mut inputs = vec![None; graph.nodes.len()];
    inputs[entry] = Some(ReachingState::new());
    let mut queue = VecDeque::from([entry]);
    while let Some(node_id) = queue.pop_front() {
        let mut state = inputs[node_id].clone().unwrap_or_default();
        if let Action::BindLoan(binding, loan_id) = graph.nodes[node_id].action {
            state.insert(binding, HashSet::from([loan_id]));
        }
        for successor in &graph.nodes[node_id].successors {
            let changed = match &mut inputs[*successor] {
                Some(existing) => join_reaching(existing, &state),
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
    inputs
}

fn compute_liveness(
    graph: &GraphBuilder<'_>,
) -> (Vec<HashSet<BindingId>>, Vec<HashSet<BindingId>>) {
    let mut predecessors = vec![Vec::new(); graph.nodes.len()];
    for (node_id, node) in graph.nodes.iter().enumerate() {
        for successor in &node.successors {
            predecessors[*successor].push(node_id);
        }
    }
    let mut live_in = vec![HashSet::new(); graph.nodes.len()];
    let mut live_out = vec![HashSet::new(); graph.nodes.len()];
    let mut queue: VecDeque<usize> = (0..graph.nodes.len()).collect();
    let mut queued = vec![true; graph.nodes.len()];
    while let Some(node_id) = queue.pop_front() {
        queued[node_id] = false;
        let mut next_out = HashSet::new();
        for successor in &graph.nodes[node_id].successors {
            next_out.extend(live_in[*successor].iter().copied());
        }
        let mut next_in = next_out.clone();
        if let Action::BindLoan(binding, _) = graph.nodes[node_id].action {
            next_in.remove(&binding);
        }
        if let Action::Read(binding, _, _) = graph.nodes[node_id].action {
            if graph.borrowed.contains(&binding) {
                next_in.insert(binding);
            }
        }
        if next_in != live_in[node_id] || next_out != live_out[node_id] {
            live_in[node_id] = next_in;
            live_out[node_id] = next_out;
            for predecessor in &predecessors[node_id] {
                if !queued[*predecessor] {
                    queued[*predecessor] = true;
                    queue.push_back(*predecessor);
                }
            }
        }
    }
    (live_in, live_out)
}

struct LoanFacts<'a> {
    sources: HashMap<LoanId, &'a Place>,
    kinds: HashMap<LoanId, BorrowKind>,
    live: HashSet<LoanId>,
    reaching: Vec<Option<ReachingState>>,
    live_in: Vec<HashSet<BindingId>>,
}

fn derive_loan_facts<'a>(graph: &GraphBuilder<'a>, entry: usize) -> AliasResult<LoanFacts<'a>> {
    let reaching = compute_reaching(graph, entry);
    let (live_in, live_out) = compute_liveness(graph);
    let mut sources = HashMap::new();
    for node in &graph.nodes {
        if let Action::Borrow {
            loan_id,
            source,
            declared_kind: _,
            span,
        } = node.action
        {
            if sources.insert(loan_id, source).is_some() {
                return Err(error(span, "内部 sema 不变式被破坏: LoanId 重复"));
            }
        }
    }
    let mut kinds: HashMap<LoanId, BorrowKind> = sources
        .keys()
        .copied()
        .map(|loan_id| (loan_id, BorrowKind::Read))
        .collect();
    let mut live = HashSet::new();
    for (node_id, node) in graph.nodes.iter().enumerate() {
        match node.action {
            Action::Read(binding, AccessKind::Write, span) if graph.borrowed.contains(&binding) => {
                let loans = reaching[node_id]
                    .as_ref()
                    .and_then(|state| state.get(&binding))
                    .ok_or_else(|| {
                        error(span, "borrowed binding 使用点缺少 reaching loan definition")
                    })?;
                for loan_id in loans {
                    let kind = kinds.get_mut(loan_id).ok_or_else(|| {
                        error(span, "内部 sema 不变式被破坏: reaching LoanId 无 source")
                    })?;
                    *kind = BorrowKind::Write;
                }
            }
            Action::BindLoan(binding, loan_id) if live_out[node_id].contains(&binding) => {
                live.insert(loan_id);
            }
            _ => {}
        }
    }
    Ok(LoanFacts {
        sources,
        kinds,
        live,
        reaching,
        live_in,
    })
}

fn active_loans(facts: &LoanFacts<'_>, node_id: usize) -> HashSet<LoanId> {
    let mut active = HashSet::new();
    let Some(reaching) = &facts.reaching[node_id] else {
        return active;
    };
    for binding in &facts.live_in[node_id] {
        if let Some(loans) = reaching.get(binding) {
            active.extend(loans.iter().copied());
        }
    }
    active
}

fn loan_overlaps(facts: &LoanFacts<'_>, loan_id: LoanId, place: &Place) -> bool {
    facts
        .sources
        .get(&loan_id)
        .is_some_and(|source| place_relation(source, place) != PlaceRelation::Disjoint)
}

fn run_dataflow(graph: &GraphBuilder<'_>, entry: usize, facts: &LoanFacts<'_>) -> AliasResult<()> {
    let mut inputs: Vec<Option<OwnershipState>> = vec![None; graph.nodes.len()];
    inputs[entry] = Some(OwnershipState::default());
    let mut queue = VecDeque::from([entry]);
    while let Some(node_id) = queue.pop_front() {
        let mut state = inputs[node_id].clone().unwrap_or_default();
        let active = active_loans(facts, node_id);
        match graph.nodes[node_id].action {
            Action::Nop => {}
            Action::Read(id, access, span) => {
                if graph.borrowed.contains(&id) {
                    if facts.reaching[node_id]
                        .as_ref()
                        .and_then(|reaching| reaching.get(&id))
                        .is_none()
                    {
                        return Err(error(span, "borrowed binding 使用点没有 live loan"));
                    }
                    // The loan itself authorizes this access. Conflicting loans were rejected when
                    // either loan was created, using their complete NLL regions.
                    continue_state(&mut inputs, &mut queue, graph, node_id, &state);
                    continue;
                }
                if state.moved.contains(&id) {
                    return Err(error(span, "值已被 move，重新初始化前不能读取"));
                }
                if active.iter().any(|loan_id| {
                    root_binding(facts.sources[loan_id]) == id
                        && (access == AccessKind::Write
                            || facts.kinds[loan_id] == BorrowKind::Write)
                }) {
                    return Err(error(span, "owner access 与 live loan 冲突"));
                }
                // Reads not resolved as owning-slot ReadPlace still share the current runtime
                // object. Until their call/return/capture effect is resolved, a later Move cannot
                // claim that the source remained exclusive.
                if graph.eligible.contains(&id) {
                    state.exposed.insert(id);
                }
            }
            Action::CloneRead(source, span) => {
                let root = root_binding(source);
                if state.moved.contains(&root) {
                    return Err(error(span, "值已被 move，重新初始化前不能读取"));
                }
                if active.iter().any(|loan_id| {
                    loan_overlaps(facts, *loan_id, source)
                        && facts.kinds[loan_id] == BorrowKind::Write
                }) {
                    return Err(error(span, "owner read 与 live WriteLoan 冲突"));
                }
            }
            Action::Borrow {
                loan_id,
                source,
                declared_kind: _,
                span,
            } => {
                let root = root_binding(source);
                if !graph.owning.contains(&root) {
                    return Err(error(
                        span,
                        "borrow source 尚未证明为当前函数内的 owning Place",
                    ));
                }
                if state.moved.contains(&root) {
                    return Err(error(span, "值已被 move，不能再建立 borrow"));
                }
                let kind = facts.kinds[&loan_id];
                if kind == BorrowKind::Write && state.exposed.contains(&root) {
                    return Err(error(
                        span,
                        "WriteLoan source 已被先前共享读取或 closure 捕获",
                    ));
                }
                if facts.live.contains(&loan_id)
                    && active.iter().any(|active_id| {
                        loan_overlaps(facts, *active_id, source)
                            && (kind == BorrowKind::Write
                                || facts.kinds[active_id] == BorrowKind::Write)
                    })
                {
                    return Err(error(span, "新 loan 与现有 live loan 冲突"));
                }
            }
            Action::BindLoan(_, _) => {}
            Action::Write(target, span) => {
                let root = root_binding(target);
                if state.moved.contains(&root) {
                    return Err(error(span, "值已被 move，重新初始化前不能写入"));
                }
                if active
                    .iter()
                    .any(|loan_id| loan_overlaps(facts, *loan_id, target))
                {
                    return Err(error(span, "owner write 与 live loan 冲突"));
                }
            }
            Action::Move(source, span) => {
                let root = root_binding(source);
                if !graph.owning.contains(&root) {
                    let message = if graph.borrowed.contains(&root) {
                        "borrowed Place 不携带 ownership capability，不能 move"
                    } else {
                        "move source 尚未证明为当前函数内的 owning local"
                    };
                    return Err(error(span, message));
                }
                if active
                    .iter()
                    .any(|loan_id| loan_overlaps(facts, *loan_id, source))
                {
                    return Err(error(span, "move source 与 live loan 冲突"));
                }
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
            Action::Declare(id) => {
                state.initialize(id);
            }
            Action::Reinitialize(id, span) => {
                if active
                    .iter()
                    .any(|loan_id| root_binding(facts.sources[loan_id]) == id)
                {
                    return Err(error(span, "owner reinitialization 与 live loan 冲突"));
                }
                state.initialize(id);
            }
        }
        continue_state(&mut inputs, &mut queue, graph, node_id, &state);
    }
    Ok(())
}

fn continue_state(
    inputs: &mut [Option<OwnershipState>],
    queue: &mut VecDeque<usize>,
    graph: &GraphBuilder<'_>,
    node_id: usize,
    state: &OwnershipState,
) {
    for successor in &graph.nodes[node_id].successors {
        let changed = match &mut inputs[*successor] {
            Some(existing) => existing.join(state),
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

fn merge_graph_kinds(
    graph: &GraphBuilder<'_>,
    facts: &LoanFacts<'_>,
    target: &mut HashMap<LoanId, BorrowKind>,
    verify_declared: bool,
) -> AliasResult<()> {
    for node in &graph.nodes {
        let Action::Borrow {
            loan_id,
            declared_kind,
            span,
            ..
        } = node.action
        else {
            continue;
        };
        let inferred = facts.kinds[&loan_id];
        if verify_declared && declared_kind != Some(inferred) {
            return Err(error(
                span,
                "内部 sema 不变式被破坏: Borrow loan kind 与 NLL 分析结果漂移",
            ));
        }
        if target.insert(loan_id, inferred).is_some() {
            return Err(error(span, "内部 sema 不变式被破坏: LoanId 跨 CFG 重复"));
        }
    }
    Ok(())
}

fn analyze_function<'a>(
    function: &'a Expr,
    queue: &mut Vec<&'a Expr>,
    kinds: &mut HashMap<LoanId, BorrowKind>,
    verify_declared: bool,
) -> AliasResult<()> {
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
    let facts = derive_loan_facts(&builder, entry)?;
    run_dataflow(&builder, entry, &facts)?;
    merge_graph_kinds(&builder, &facts, kinds, verify_declared)?;
    queue.extend(builder.nested_functions);
    Ok(())
}

fn analyze_root_expr<'a>(
    expr: &'a Expr,
    queue: &mut Vec<&'a Expr>,
    kinds: &mut HashMap<LoanId, BorrowKind>,
    verify_declared: bool,
) -> AliasResult<()> {
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
    let facts = derive_loan_facts(&builder, entry)?;
    run_dataflow(&builder, entry, &facts)?;
    merge_graph_kinds(&builder, &facts, kinds, verify_declared)?;
    queue.extend(builder.nested_functions);
    Ok(())
}

fn analyze_program(
    program: &CheckedProgram,
    verify_declared: bool,
) -> AliasResult<HashMap<LoanId, BorrowKind>> {
    let mut functions = Vec::new();
    let mut kinds = HashMap::new();
    for item in &program.items {
        match item {
            Item::Binding(binding) => {
                analyze_root_expr(&binding.value, &mut functions, &mut kinds, verify_declared)?
            }
            Item::StructDef(def) => {
                for field in &def.fields {
                    if let Some(default) = &field.default {
                        analyze_root_expr(default, &mut functions, &mut kinds, verify_declared)?;
                    }
                }
            }
        }
    }
    while let Some(function) = functions.pop() {
        analyze_function(function, &mut functions, &mut kinds, verify_declared)?;
    }
    Ok(kinds)
}

enum MutNode<'a> {
    Expr(&'a mut Expr),
    Stmt(&'a mut Stmt),
}

fn push_place_expr_children_mut<'a>(stack: &mut Vec<MutNode<'a>>, place: &'a mut Place) {
    let mut places = vec![place];
    while let Some(place) = places.pop() {
        match place {
            Place::Local { .. } => {}
            Place::Field { base, .. } => places.push(base),
            Place::Index { base, index, .. } => {
                stack.push(MutNode::Expr(index));
                places.push(base);
            }
        }
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

fn push_stmt_mut<'a>(stack: &mut Vec<MutNode<'a>>, stmt: &'a mut Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(MutNode::Expr(&mut binding.value)),
        Stmt::Assign { target, value } => {
            stack.push(MutNode::Expr(value));
            push_place_expr_children_mut(stack, target);
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

fn push_expr_mut<'a>(stack: &mut Vec<MutNode<'a>>, expr: &'a mut Expr) {
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
        Expr::FuncLit { body, .. } => push_body_mut(stack, body),
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
        | Expr::Move { source, .. } => push_place_expr_children_mut(stack, source),
        Expr::Typeof { .. }
        | Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..) => {}
    }
}

fn apply_kinds(
    program: &mut CheckedProgram,
    kinds: &HashMap<LoanId, BorrowKind>,
) -> AliasResult<()> {
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
    let mut seen = HashSet::new();
    while let Some(node) = stack.pop() {
        match node {
            MutNode::Stmt(stmt) => push_stmt_mut(&mut stack, stmt),
            MutNode::Expr(expr) => {
                if let Expr::Borrow {
                    loan_id,
                    kind,
                    span,
                    ..
                } = expr
                {
                    let inferred = kinds.get(loan_id).copied().ok_or_else(|| {
                        error(*span, "内部 sema 不变式被破坏: Borrow 缺少 NLL kind fact")
                    })?;
                    *kind = Some(inferred);
                    if !seen.insert(*loan_id) {
                        return Err(error(*span, "内部 sema 不变式被破坏: LoanId 在 HIR 中重复"));
                    }
                }
                push_expr_mut(&mut stack, expr);
            }
        }
    }
    if seen.len() != kinds.len() {
        return Err(error(
            Span::default(),
            "内部 sema 不变式被破坏: 存在未写回的 LoanId fact",
        ));
    }
    Ok(())
}

pub(super) fn finalize(program: &mut CheckedProgram) -> AliasResult<()> {
    let kinds = analyze_program(program, false)?;
    apply_kinds(program, &kinds)
}

pub(super) fn validate(program: &CheckedProgram) -> AliasResult<()> {
    analyze_program(program, true).map(|_| ())
}
