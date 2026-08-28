//! sema 输出的类型化 HIR。parser AST 只描述语法；进入 codegen 的每个表达式
//! 都携带最终静态类型，调用表达式还携带 sema 已解析的调用目标。
//!
//! 名字解析只允许发生在 sema 的原始检查点。HIR lowering 只消费 facts：词法绑定、
//! 赋值目标、for/Pattern 绑定、字段访问、用户方法调用和闭包捕获均以稳定 ID/索引表示；
//! 本模块禁止再次按源码名字重建语义关系。

#![allow(dead_code)]

pub use crate::ast::{BinOp, BindKind, CtorKind, Import, Pattern};
use crate::sema::types::Ty;
use crate::{AliasError, AliasResult, Span};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct BindingId(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct MethodId(pub(crate) u32);

#[derive(Debug, Clone)]
pub(crate) struct CheckedProgram {
    pub(crate) imports: Vec<Import>,
    pub(crate) items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub(crate) enum Item {
    Binding(Binding),
    StructDef(StructDef),
}

#[derive(Debug, Clone)]
pub(crate) struct StructDef {
    pub(crate) name: String,
    pub(crate) fields: Vec<StructField>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone)]
pub(crate) struct StructField {
    pub(crate) name: String,
    pub(crate) mutable: bool,
    pub(crate) ty: Ty,
    pub(crate) default: Option<Expr>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone)]
pub(crate) struct Binding {
    pub(crate) binding_id: BindingId,
    pub(crate) method_id: Option<MethodId>,
    pub(crate) self_id: Option<BindingId>,
    pub(crate) is_pub: bool,
    pub(crate) kind: BindKind,
    pub(crate) ty: Ty,
    pub(crate) name: String,
    pub(crate) receiver: Option<Ty>,
    pub(crate) value: Expr,
    pub(crate) span: Span,
}

#[derive(Debug, Clone)]
pub(crate) enum Body {
    Block(Vec<Stmt>),
    Single(Box<Stmt>),
}

#[derive(Debug, Clone)]
pub(crate) struct Param {
    pub(crate) binding_id: BindingId,
    pub(crate) ty: Ty,
    pub(crate) name: String,
    pub(crate) span: Span,
}

#[derive(Debug, Clone)]
pub(crate) enum Stmt {
    Binding(Binding),
    Assign {
        target: String,
        target_id: BindingId,
        value: Expr,
        span: Span,
    },
    FieldAssign {
        recv: Box<Expr>,
        field: String,
        field_index: usize,
        value: Expr,
        span: Span,
    },
    ExprStmt {
        expr: Expr,
        span: Span,
    },
    Return {
        value: Option<Expr>,
        span: Span,
    },
    If {
        branches: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
        span: Span,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    For {
        binding_id: BindingId,
        ty: Ty,
        name: String,
        iterable: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    Break { span: Span },
    Continue { span: Span },
}

#[derive(Debug, Clone)]
pub(crate) enum StrPart {
    Lit(String),
    Hole(Box<Expr>),
}

#[derive(Debug, Clone)]
pub(crate) enum ArmBody {
    Block(Vec<Stmt>),
    Value(Box<Expr>),
    Ret(Box<Expr>),
}

#[derive(Debug, Clone)]
pub(crate) struct MatchArm {
    pub(crate) pattern: Pattern,
    pub(crate) binding_id: Option<BindingId>,
    pub(crate) body: ArmBody,
    pub(crate) span: Span,
}

#[derive(Debug, Clone)]
pub(crate) struct CallArg {
    pub(crate) label: Option<String>,
    pub(crate) value: Expr,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuiltinCall {
    Print,
    Println,
    Typeof,
    From,
    TryFrom,
    Increase,
    Decrease,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MethodTarget {
    Numeric(BinOp),
    BoolNot,
    StringLen,
    StringUpper,
    StringLower,
    StringTrim,
    ArrayLen,
    ArrayPush,
    ArrayPop,
    ArrayIterator,
    User {
        receiver: Ty,
        name: String,
        id: Option<MethodId>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CallTarget {
    FunctionValue,
    StructConstructor(String),
    ResultConstructor(CtorKind),
    Builtin(BuiltinCall),
    Method(MethodTarget),
}

#[derive(Debug, Clone)]
pub(crate) struct ExprInfo {
    pub(crate) ty: Ty,
    pub(crate) call_target: Option<CallTarget>,
    /// 零参函数裸名的 sema lowering 标记。HIR lowering 必须立即消费并清除。
    pub(crate) implicit_zero_callee: Option<Ty>,
}

pub(super) struct LowerFacts {
    pub(super) exprs: HashMap<usize, ExprInfo>,
    pub(super) bindings: HashMap<usize, Ty>,
    pub(super) binding_ids: HashMap<usize, BindingId>,
    pub(super) receivers: HashMap<usize, Ty>,
    pub(super) method_ids: HashMap<usize, MethodId>,
    pub(super) method_self_ids: HashMap<usize, BindingId>,
    pub(super) fields: HashMap<usize, Ty>,
    pub(super) field_indices: HashMap<usize, usize>,
    pub(super) field_assign_indices: HashMap<usize, usize>,
    pub(super) params: HashMap<usize, Ty>,
    pub(super) param_ids: HashMap<usize, BindingId>,
    pub(super) fors: HashMap<usize, Ty>,
    pub(super) for_ids: HashMap<usize, BindingId>,
    pub(super) assign_target_ids: HashMap<usize, BindingId>,
    pub(super) match_binding_ids: HashMap<usize, BindingId>,
    pub(super) expr_binding_ids: HashMap<usize, BindingId>,
}

#[derive(Debug, Clone)]
pub(crate) enum Expr {
    Int(u64, Span, ExprInfo),
    Float(f64, Span, ExprInfo),
    Bool(bool, Span, ExprInfo),
    Str(Vec<StrPart>, Span, ExprInfo),
    /// None 只允许出现在 codegen 不会求值的 builtin/constructor callee 名字上。
    Ident(String, Option<BindingId>, Span, ExprInfo),
    This(Span, ExprInfo),
    Cast { expr: Box<Expr>, span: Span, info: ExprInfo },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
        info: ExprInfo,
    },
    Neg { expr: Box<Expr>, span: Span, info: ExprInfo },
    Not { expr: Box<Expr>, span: Span, info: ExprInfo },
    BitNot { expr: Box<Expr>, span: Span, info: ExprInfo },
    Ternary {
        cond: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
        span: Span,
        info: ExprInfo,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<CallArg>,
        span: Span,
        info: ExprInfo,
    },
    MethodCall {
        recv: Box<Expr>,
        args: Vec<CallArg>,
        span: Span,
        info: ExprInfo,
    },
    Field {
        recv: Box<Expr>,
        name: String,
        field_index: usize,
        span: Span,
        info: ExprInfo,
    },
    Index {
        recv: Box<Expr>,
        idx: Box<Expr>,
        span: Span,
        info: ExprInfo,
    },
    ArrayLit { elems: Vec<Expr>, span: Span, info: ExprInfo },
    FuncLit {
        params: Vec<Param>,
        /// 当前只用于方法隐式 self；它属于函数局部，不应进入 capture 列表。
        implicit_bindings: Vec<BindingId>,
        /// sema/HIR 基于稳定 BindingId 计算出的自由变量列表。
        captures: Vec<BindingId>,
        body: Box<Body>,
        span: Span,
        info: ExprInfo,
    },
    Match {
        subject: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
        info: ExprInfo,
    },
    Propagate { expr: Box<Expr>, span: Span, info: ExprInfo },
}

impl Expr {
    pub(crate) fn info(&self) -> &ExprInfo {
        match self {
            Self::Int(_, _, info)
            | Self::Float(_, _, info)
            | Self::Bool(_, _, info)
            | Self::Str(_, _, info)
            | Self::Ident(_, _, _, info)
            | Self::This(_, info) => info,
            Self::Cast { info, .. }
            | Self::Binary { info, .. }
            | Self::Neg { info, .. }
            | Self::Not { info, .. }
            | Self::BitNot { info, .. }
            | Self::Ternary { info, .. }
            | Self::Call { info, .. }
            | Self::MethodCall { info, .. }
            | Self::Field { info, .. }
            | Self::Index { info, .. }
            | Self::ArrayLit { info, .. }
            | Self::FuncLit { info, .. }
            | Self::Match { info, .. }
            | Self::Propagate { info, .. } => info,
        }
    }

    pub(crate) fn ty(&self) -> &Ty {
        &self.info().ty
    }

    pub(crate) fn call_target(&self) -> Option<&CallTarget> {
        self.info().call_target.as_ref()
    }

    pub(crate) fn span(&self) -> Span {
        match self {
            Self::Int(_, span, _)
            | Self::Float(_, span, _)
            | Self::Bool(_, span, _)
            | Self::Str(_, span, _)
            | Self::Ident(_, _, span, _)
            | Self::This(span, _) => *span,
            Self::Cast { span, .. }
            | Self::Binary { span, .. }
            | Self::Neg { span, .. }
            | Self::Not { span, .. }
            | Self::BitNot { span, .. }
            | Self::Ternary { span, .. }
            | Self::Call { span, .. }
            | Self::MethodCall { span, .. }
            | Self::Field { span, .. }
            | Self::Index { span, .. }
            | Self::ArrayLit { span, .. }
            | Self::FuncLit { span, .. }
            | Self::Match { span, .. }
            | Self::Propagate { span, .. } => *span,
        }
    }
}

pub(super) fn lower(
    program: crate::ast::Program,
    mut facts: LowerFacts,
) -> AliasResult<CheckedProgram> {
    let mut checked = CheckedProgram {
        imports: program.imports.clone(),
        items: program
            .items
            .iter()
            .map(|item| lower_item(item, &mut facts))
            .collect::<AliasResult<Vec<_>>>()?,
    };
    ensure_facts_consumed(&facts)?;
    validate_resolved_hir(&checked)?;
    populate_captures(&mut checked);
    Ok(checked)
}

fn ensure_facts_consumed(facts: &LowerFacts) -> AliasResult<()> {
    let leftovers = [
        ("表达式", facts.exprs.len()),
        ("绑定类型", facts.bindings.len()),
        ("BindingId", facts.binding_ids.len()),
        ("方法接收者", facts.receivers.len()),
        ("MethodId", facts.method_ids.len()),
        ("方法 self", facts.method_self_ids.len()),
        ("结构体字段", facts.fields.len()),
        ("字段索引", facts.field_indices.len()),
        ("字段赋值索引", facts.field_assign_indices.len()),
        ("函数参数类型", facts.params.len()),
        ("函数参数 BindingId", facts.param_ids.len()),
        ("for 循环变量类型", facts.fors.len()),
        ("for 循环变量 BindingId", facts.for_ids.len()),
        ("赋值目标 BindingId", facts.assign_target_ids.len()),
        ("Pattern BindingId", facts.match_binding_ids.len()),
        ("标识符 BindingId", facts.expr_binding_ids.len()),
    ];
    if let Some((what, count)) = leftovers.into_iter().find(|(_, count)| *count != 0) {
        return Err(AliasError {
            msg: format!("内部 sema 不变式被破坏: 存在 {count} 条未消费的{what} fact"),
            span: Span::default(),
        });
    }
    Ok(())
}

fn take_required<T: Copy>(
    facts: &mut HashMap<usize, T>,
    key: usize,
    span: Span,
    what: &str,
) -> AliasResult<T> {
    facts.remove(&key).ok_or_else(|| AliasError {
        msg: format!("内部 sema 不变式被破坏: {what}缺失"),
        span,
    })
}

fn lower_item(item: &crate::ast::Item, facts: &mut LowerFacts) -> AliasResult<Item> {
    Ok(match item {
        crate::ast::Item::Binding(binding) => Item::Binding(lower_binding(binding, facts)?),
        crate::ast::Item::StructDef(def) => Item::StructDef(StructDef {
            name: def.name.clone(),
            fields: def
                .fields
                .iter()
                .map(|field| {
                    let key = field as *const crate::ast::StructField as usize;
                    Ok(StructField {
                        name: field.name.clone(),
                        mutable: field.mutable,
                        ty: facts.fields.remove(&key).ok_or_else(|| AliasError {
                            msg: "内部 sema 不变式被破坏: 结构体字段缺少静态类型".into(),
                            span: field.span,
                        })?,
                        default: field
                            .default
                            .as_ref()
                            .map(|expr| lower_expr(expr, facts))
                            .transpose()?,
                        span: field.span,
                    })
                })
                .collect::<AliasResult<Vec<_>>>()?,
            span: def.span,
        }),
    })
}

fn lower_binding(binding: &crate::ast::Binding, facts: &mut LowerFacts) -> AliasResult<Binding> {
    let key = binding as *const crate::ast::Binding as usize;
    let binding_id = take_required(&mut facts.binding_ids, key, binding.span, "绑定 BindingId")?;
    let ty = facts.bindings.remove(&key).ok_or_else(|| AliasError {
        msg: "内部 sema 不变式被破坏: 绑定缺少静态类型".into(),
        span: binding.span,
    })?;
    let receiver = facts.receivers.remove(&key);
    let method_id = facts.method_ids.remove(&key);
    let self_id = facts.method_self_ids.remove(&key);
    if receiver.is_some() != method_id.is_some() || receiver.is_some() != self_id.is_some() {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: 方法绑定缺少 MethodId/self BindingId".into(),
            span: binding.span,
        });
    }
    let mut value = lower_expr(&binding.value, facts)?;
    if let Some(id) = self_id {
        let Expr::FuncLit {
            implicit_bindings, ..
        } = &mut value
        else {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: 方法值不是 FuncLit".into(),
                span: binding.span,
            });
        };
        implicit_bindings.push(id);
    }
    Ok(Binding {
        binding_id,
        method_id,
        self_id,
        is_pub: binding.is_pub,
        kind: binding.kind,
        ty,
        name: binding.name.clone(),
        receiver,
        value,
        span: binding.span,
    })
}

fn lower_body(body: &crate::ast::Body, facts: &mut LowerFacts) -> AliasResult<Body> {
    Ok(match body {
        crate::ast::Body::Block(stmts) => Body::Block(
            stmts
                .iter()
                .map(|stmt| lower_stmt(stmt, facts))
                .collect::<AliasResult<Vec<_>>>()?,
        ),
        crate::ast::Body::Single(stmt) => Body::Single(Box::new(lower_stmt(stmt, facts)?)),
    })
}

fn lower_stmt(stmt: &crate::ast::Stmt, facts: &mut LowerFacts) -> AliasResult<Stmt> {
    let key = stmt as *const crate::ast::Stmt as usize;
    Ok(match stmt {
        crate::ast::Stmt::Binding(binding) => Stmt::Binding(lower_binding(binding, facts)?),
        crate::ast::Stmt::Assign { target, value, span } => Stmt::Assign {
            target: target.clone(),
            target_id: take_required(
                &mut facts.assign_target_ids,
                key,
                *span,
                "赋值目标 BindingId",
            )?,
            value: lower_expr(value, facts)?,
            span: *span,
        },
        crate::ast::Stmt::FieldAssign {
            recv,
            field,
            value,
            span,
        } => Stmt::FieldAssign {
            recv: Box::new(lower_expr(recv, facts)?),
            field: field.clone(),
            field_index: take_required(
                &mut facts.field_assign_indices,
                key,
                *span,
                "字段赋值索引",
            )?,
            value: lower_expr(value, facts)?,
            span: *span,
        },
        crate::ast::Stmt::ExprStmt { expr, span } => Stmt::ExprStmt {
            expr: lower_expr(expr, facts)?,
            span: *span,
        },
        crate::ast::Stmt::Return { value, span } => Stmt::Return {
            value: value
                .as_ref()
                .map(|expr| lower_expr(expr, facts))
                .transpose()?,
            span: *span,
        },
        crate::ast::Stmt::If {
            branches,
            else_body,
            span,
        } => Stmt::If {
            branches: branches
                .iter()
                .map(|(cond, body)| {
                    Ok((
                        lower_expr(cond, facts)?,
                        body.iter()
                            .map(|stmt| lower_stmt(stmt, facts))
                            .collect::<AliasResult<Vec<_>>>()?,
                    ))
                })
                .collect::<AliasResult<Vec<_>>>()?,
            else_body: else_body
                .as_ref()
                .map(|body| {
                    body.iter()
                        .map(|stmt| lower_stmt(stmt, facts))
                        .collect::<AliasResult<Vec<_>>>()
                })
                .transpose()?,
            span: *span,
        },
        crate::ast::Stmt::While { cond, body, span } => Stmt::While {
            cond: lower_expr(cond, facts)?,
            body: body
                .iter()
                .map(|stmt| lower_stmt(stmt, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
        },
        crate::ast::Stmt::For {
            name,
            iterable,
            body,
            span,
            ..
        } => Stmt::For {
            binding_id: take_required(&mut facts.for_ids, key, *span, "for BindingId")?,
            ty: facts.fors.remove(&key).ok_or_else(|| AliasError {
                msg: "内部 sema 不变式被破坏: for 循环变量缺少静态类型".into(),
                span: *span,
            })?,
            name: name.clone(),
            iterable: lower_expr(iterable, facts)?,
            body: body
                .iter()
                .map(|stmt| lower_stmt(stmt, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
        },
        crate::ast::Stmt::Break { span } => Stmt::Break { span: *span },
        crate::ast::Stmt::Continue { span } => Stmt::Continue { span: *span },
    })
}

fn lower_expr(expr: &crate::ast::Expr, facts: &mut LowerFacts) -> AliasResult<Expr> {
    let key = expr as *const crate::ast::Expr as usize;
    let Some(mut info) = facts.exprs.remove(&key) else {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: 表达式缺少静态类型".into(),
            span: expr.span(),
        });
    };

    if let Some(callee_ty) = info.implicit_zero_callee.take() {
        if info.call_target != Some(CallTarget::FunctionValue) {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: 零参裸名调用缺少函数调用目标".into(),
                span: expr.span(),
            });
        }
        let callee_info = ExprInfo {
            ty: callee_ty,
            call_target: None,
            implicit_zero_callee: None,
        };
        let callee = match expr {
            crate::ast::Expr::Ident(name, span) => Expr::Ident(
                name.clone(),
                Some(take_required(
                    &mut facts.expr_binding_ids,
                    key,
                    *span,
                    "零参函数 callee BindingId",
                )?),
                *span,
                callee_info,
            ),
            crate::ast::Expr::This(span) => Expr::This(*span, callee_info),
            _ => {
                return Err(AliasError {
                    msg: "内部 sema 不变式被破坏: 零参隐式调用只允许直接函数引用".into(),
                    span: expr.span(),
                })
            }
        };
        return Ok(Expr::Call {
            callee: Box::new(callee),
            args: Vec::new(),
            span: expr.span(),
            info,
        });
    }

    let is_call = matches!(
        expr,
        crate::ast::Expr::Call { .. }
            | crate::ast::Expr::Juxtapose { .. }
            | crate::ast::Expr::MethodCall { .. }
    );
    if is_call && info.call_target.is_none() {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: 调用表达式缺少已解析目标".into(),
            span: expr.span(),
        });
    }
    if matches!(
        info.call_target,
        Some(CallTarget::Method(MethodTarget::User { id: None, .. }))
    ) {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: 用户方法调用缺少 MethodId".into(),
            span: expr.span(),
        });
    }

    Ok(match expr {
        crate::ast::Expr::Int(value, span) => Expr::Int(*value, *span, info),
        crate::ast::Expr::Float(value, span) => Expr::Float(*value, *span, info),
        crate::ast::Expr::Bool(value, span) => Expr::Bool(*value, *span, info),
        crate::ast::Expr::Str(parts, span) => Expr::Str(
            parts
                .iter()
                .map(|part| match part {
                    crate::ast::StrPartAst::Lit(value) => Ok(StrPart::Lit(value.clone())),
                    crate::ast::StrPartAst::Hole(expr) => {
                        Ok(StrPart::Hole(Box::new(lower_expr(expr, facts)?)))
                    }
                })
                .collect::<AliasResult<Vec<_>>>()?,
            *span,
            info,
        ),
        crate::ast::Expr::Ident(name, span) => Expr::Ident(
            name.clone(),
            facts.expr_binding_ids.remove(&key),
            *span,
            info,
        ),
        crate::ast::Expr::This(span) => Expr::This(*span, info),
        crate::ast::Expr::Cast { expr, span, .. } => Expr::Cast {
            expr: Box::new(lower_expr(expr, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
            op: *op,
            lhs: Box::new(lower_expr(lhs, facts)?),
            rhs: Box::new(lower_expr(rhs, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Neg { expr, span } => Expr::Neg {
            expr: Box::new(lower_expr(expr, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Not { expr, span } => Expr::Not {
            expr: Box::new(lower_expr(expr, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::BitNot { expr, span } => Expr::BitNot {
            expr: Box::new(lower_expr(expr, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            span,
        } => Expr::Ternary {
            cond: Box::new(lower_expr(cond, facts)?),
            then_expr: Box::new(lower_expr(then_expr, facts)?),
            else_expr: Box::new(lower_expr(else_expr, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Call { callee, args, span } => Expr::Call {
            callee: Box::new(lower_expr(callee, facts)?),
            args: args
                .iter()
                .map(|arg| lower_call_arg(arg, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
            info,
        },
        crate::ast::Expr::Juxtapose { lhs, rhs, span } => match info.call_target.as_ref() {
            Some(CallTarget::FunctionValue) => Expr::Call {
                callee: Box::new(lower_expr(lhs, facts)?),
                args: vec![CallArg {
                    label: None,
                    value: lower_expr(rhs, facts)?,
                    span: rhs.span(),
                }],
                span: *span,
                info,
            },
            Some(CallTarget::Method(_)) => {
                if !matches!(rhs.as_ref(), crate::ast::Expr::Ident(..)) {
                    return Err(AliasError {
                        msg: "内部 sema 不变式被破坏: 零参方法中缀 RHS 不是方法名".into(),
                        span: rhs.span(),
                    });
                }
                Expr::MethodCall {
                    recv: Box::new(lower_expr(lhs, facts)?),
                    args: Vec::new(),
                    span: *span,
                    info,
                }
            }
            _ => {
                return Err(AliasError {
                    msg: "内部 sema 不变式被破坏: 两项无括号邻接未解析".into(),
                    span: *span,
                })
            }
        },
        crate::ast::Expr::MethodCall {
            recv, args, span, ..
        } => Expr::MethodCall {
            recv: Box::new(lower_expr(recv, facts)?),
            args: args
                .iter()
                .map(|arg| lower_call_arg(arg, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
            info,
        },
        crate::ast::Expr::Field { recv, name, span } => Expr::Field {
            recv: Box::new(lower_expr(recv, facts)?),
            name: name.clone(),
            field_index: take_required(
                &mut facts.field_indices,
                key,
                *span,
                "字段访问索引",
            )?,
            span: *span,
            info,
        },
        crate::ast::Expr::Index { recv, idx, span } => Expr::Index {
            recv: Box::new(lower_expr(recv, facts)?),
            idx: Box::new(lower_expr(idx, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::ArrayLit { elems, span } => Expr::ArrayLit {
            elems: elems
                .iter()
                .map(|expr| lower_expr(expr, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
            info,
        },
        crate::ast::Expr::FuncLit { params, body, span } => Expr::FuncLit {
            params: params
                .iter()
                .map(|param| {
                    let pkey = param as *const crate::ast::Param as usize;
                    Ok(Param {
                        binding_id: take_required(
                            &mut facts.param_ids,
                            pkey,
                            param.span,
                            "函数参数 BindingId",
                        )?,
                        ty: facts.params.remove(&pkey).ok_or_else(|| AliasError {
                            msg: "内部 sema 不变式被破坏: 函数参数缺少静态类型".into(),
                            span: param.span,
                        })?,
                        name: param.name.clone(),
                        span: param.span,
                    })
                })
                .collect::<AliasResult<Vec<_>>>()?,
            implicit_bindings: Vec::new(),
            captures: Vec::new(),
            body: Box::new(lower_body(body, facts)?),
            span: *span,
            info,
        },
        crate::ast::Expr::Match {
            subject,
            arms,
            span,
        } => Expr::Match {
            subject: Box::new(lower_expr(subject, facts)?),
            arms: arms
                .iter()
                .map(|arm| lower_match_arm(arm, facts))
                .collect::<AliasResult<Vec<_>>>()?,
            span: *span,
            info,
        },
        crate::ast::Expr::Propagate { expr, span } => Expr::Propagate {
            expr: Box::new(lower_expr(expr, facts)?),
            span: *span,
            info,
        },
    })
}

fn lower_call_arg(arg: &crate::ast::CallArg, facts: &mut LowerFacts) -> AliasResult<CallArg> {
    Ok(CallArg {
        label: arg.label.clone(),
        value: lower_expr(&arg.value, facts)?,
        span: arg.span,
    })
}

fn lower_match_arm(arm: &crate::ast::MatchArm, facts: &mut LowerFacts) -> AliasResult<MatchArm> {
    let key = arm as *const crate::ast::MatchArm as usize;
    Ok(MatchArm {
        pattern: arm.pattern.clone(),
        binding_id: facts.match_binding_ids.remove(&key),
        body: match &arm.body {
            crate::ast::ArmBody::Block(stmts) => ArmBody::Block(
                stmts
                    .iter()
                    .map(|stmt| lower_stmt(stmt, facts))
                    .collect::<AliasResult<Vec<_>>>()?,
            ),
            crate::ast::ArmBody::Value(expr) => ArmBody::Value(Box::new(lower_expr(expr, facts)?)),
            crate::ast::ArmBody::Ret(expr) => ArmBody::Ret(Box::new(lower_expr(expr, facts)?)),
        },
        span: arm.span,
    })
}

fn validate_resolved_hir(program: &CheckedProgram) -> AliasResult<()> {
    fn check_expr(expr: &Expr) -> AliasResult<()> {
        if expr.ty().contains_unknown() {
            return Err(AliasError {
                msg: format!("内部 sema 不变式被破坏: HIR 仍含未确定类型 {}", expr.ty().name()),
                span: expr.span(),
            });
        }
        match expr {
            Expr::Ident(name, None, span, _) => {
                // None 只允许作为不求值的 builtin/constructor 直接 callee；父调用负责验证。
                let _ = (name, span);
            }
            Expr::Str(parts, ..) => {
                for part in parts {
                    if let StrPart::Hole(hole) = part {
                        check_expr(hole)?;
                    }
                }
            }
            Expr::Cast { expr, .. }
            | Expr::Neg { expr, .. }
            | Expr::Not { expr, .. }
            | Expr::BitNot { expr, .. }
            | Expr::Propagate { expr, .. } => check_expr(expr)?,
            Expr::Binary { lhs, rhs, .. } => {
                check_expr(lhs)?;
                check_expr(rhs)?;
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
                ..
            } => {
                check_expr(cond)?;
                check_expr(then_expr)?;
                check_expr(else_expr)?;
            }
            Expr::Call {
                callee,
                args,
                info,
                ..
            } => {
                let Some(target) = info.call_target.as_ref() else {
                    return Err(AliasError {
                        msg: "内部 sema 不变式被破坏: HIR Call 缺少 target".into(),
                        span: expr.span(),
                    });
                };
                match target {
                    CallTarget::FunctionValue => {
                        if matches!(callee.as_ref(), Expr::Ident(_, None, ..)) {
                            return Err(AliasError {
                                msg: "内部 sema 不变式被破坏: 函数值 callee 缺少 BindingId".into(),
                                span: callee.span(),
                            });
                        }
                        check_expr(callee)?;
                    }
                    _ => {
                        if !matches!(callee.as_ref(), Expr::Ident(..)) {
                            return Err(AliasError {
                                msg: "内部 sema 不变式被破坏: builtin/constructor callee 非直接名字".into(),
                                span: callee.span(),
                            });
                        }
                    }
                }
                for arg in args {
                    check_expr(&arg.value)?;
                }
            }
            Expr::MethodCall {
                recv, args, info, ..
            } => {
                if matches!(
                    info.call_target,
                    Some(CallTarget::Method(MethodTarget::User { id: None, .. }))
                ) {
                    return Err(AliasError {
                        msg: "内部 sema 不变式被破坏: HIR 用户方法缺少 MethodId".into(),
                        span: expr.span(),
                    });
                }
                check_expr(recv)?;
                for arg in args {
                    check_expr(&arg.value)?;
                }
            }
            Expr::Field { recv, .. } => check_expr(recv)?,
            Expr::Index { recv, idx, .. } => {
                check_expr(recv)?;
                check_expr(idx)?;
            }
            Expr::ArrayLit { elems, .. } => {
                for elem in elems {
                    check_expr(elem)?;
                }
            }
            Expr::FuncLit { body, .. } => check_body(body)?,
            Expr::Match { subject, arms, .. } => {
                check_expr(subject)?;
                for arm in arms {
                    match &arm.body {
                        ArmBody::Block(stmts) => check_stmts(stmts)?,
                        ArmBody::Value(value) | ArmBody::Ret(value) => check_expr(value)?,
                    }
                }
            }
            Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::Ident(_, Some(_), ..) | Expr::This(..) => {}
        }
        Ok(())
    }
    fn check_stmt(stmt: &Stmt) -> AliasResult<()> {
        match stmt {
            Stmt::Binding(binding) => check_expr(&binding.value),
            Stmt::Assign { value, .. } => check_expr(value),
            Stmt::FieldAssign { recv, value, .. } => {
                check_expr(recv)?;
                check_expr(value)
            }
            Stmt::ExprStmt { expr, .. } => check_expr(expr),
            Stmt::Return { value, .. } => {
                if let Some(value) = value { check_expr(value)?; }
                Ok(())
            }
            Stmt::If { branches, else_body, .. } => {
                for (cond, body) in branches {
                    check_expr(cond)?;
                    check_stmts(body)?;
                }
                if let Some(body) = else_body { check_stmts(body)?; }
                Ok(())
            }
            Stmt::While { cond, body, .. } => {
                check_expr(cond)?;
                check_stmts(body)
            }
            Stmt::For { iterable, body, .. } => {
                check_expr(iterable)?;
                check_stmts(body)
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => Ok(()),
        }
    }
    fn check_stmts(stmts: &[Stmt]) -> AliasResult<()> {
        for stmt in stmts { check_stmt(stmt)?; }
        Ok(())
    }
    fn check_body(body: &Body) -> AliasResult<()> {
        match body {
            Body::Block(stmts) => check_stmts(stmts),
            Body::Single(stmt) => check_stmt(stmt),
        }
    }

    for item in &program.items {
        match item {
            Item::Binding(binding) => check_expr(&binding.value)?,
            Item::StructDef(def) => {
                for field in &def.fields {
                    if field.ty.contains_unknown() {
                        return Err(AliasError {
                            msg: "内部 sema 不变式被破坏: 字段类型未确定".into(),
                            span: field.span,
                        });
                    }
                    if let Some(default) = &field.default { check_expr(default)?; }
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Closure capture resolution — 只基于已经固化的 BindingId。
// ---------------------------------------------------------------------------

fn populate_captures(program: &mut CheckedProgram) {
    let globals: HashSet<BindingId> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Binding(binding) if binding.receiver.is_none() => Some(binding.binding_id),
            _ => None,
        })
        .collect();

    for item in &mut program.items {
        match item {
            Item::Binding(binding) => populate_expr_captures(&mut binding.value, &globals),
            Item::StructDef(def) => {
                for field in &mut def.fields {
                    if let Some(default) = &mut field.default {
                        populate_expr_captures(default, &globals);
                    }
                }
            }
        }
    }
}

fn populate_expr_captures(expr: &mut Expr, globals: &HashSet<BindingId>) {
    match expr {
        Expr::FuncLit {
            params,
            implicit_bindings,
            captures,
            body,
            ..
        } => {
            *captures = analyze_function_captures(params, implicit_bindings, body, globals);
        }
        Expr::Str(parts, ..) => {
            for part in parts {
                if let StrPart::Hole(hole) = part { populate_expr_captures(hole, globals); }
            }
        }
        Expr::Cast { expr, .. }
        | Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Propagate { expr, .. } => populate_expr_captures(expr, globals),
        Expr::Binary { lhs, rhs, .. } => {
            populate_expr_captures(lhs, globals);
            populate_expr_captures(rhs, globals);
        }
        Expr::Ternary { cond, then_expr, else_expr, .. } => {
            populate_expr_captures(cond, globals);
            populate_expr_captures(then_expr, globals);
            populate_expr_captures(else_expr, globals);
        }
        Expr::Call { callee, args, .. } => {
            populate_expr_captures(callee, globals);
            for arg in args { populate_expr_captures(&mut arg.value, globals); }
        }
        Expr::MethodCall { recv, args, .. } => {
            populate_expr_captures(recv, globals);
            for arg in args { populate_expr_captures(&mut arg.value, globals); }
        }
        Expr::Field { recv, .. } => populate_expr_captures(recv, globals),
        Expr::Index { recv, idx, .. } => {
            populate_expr_captures(recv, globals);
            populate_expr_captures(idx, globals);
        }
        Expr::ArrayLit { elems, .. } => {
            for elem in elems { populate_expr_captures(elem, globals); }
        }
        Expr::Match { subject, arms, .. } => {
            populate_expr_captures(subject, globals);
            for arm in arms {
                match &mut arm.body {
                    ArmBody::Block(stmts) => populate_stmt_list_captures(stmts, globals),
                    ArmBody::Value(value) | ArmBody::Ret(value) => populate_expr_captures(value, globals),
                }
            }
        }
        Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::Ident(..) | Expr::This(..) => {}
    }
}

fn populate_stmt_list_captures(stmts: &mut [Stmt], globals: &HashSet<BindingId>) {
    for stmt in stmts {
        match stmt {
            Stmt::Binding(binding) => populate_expr_captures(&mut binding.value, globals),
            Stmt::Assign { value, .. } => populate_expr_captures(value, globals),
            Stmt::FieldAssign { recv, value, .. } => {
                populate_expr_captures(recv, globals);
                populate_expr_captures(value, globals);
            }
            Stmt::ExprStmt { expr, .. } => populate_expr_captures(expr, globals),
            Stmt::Return { value, .. } => {
                if let Some(value) = value { populate_expr_captures(value, globals); }
            }
            Stmt::If { branches, else_body, .. } => {
                for (cond, body) in branches {
                    populate_expr_captures(cond, globals);
                    populate_stmt_list_captures(body, globals);
                }
                if let Some(body) = else_body { populate_stmt_list_captures(body, globals); }
            }
            Stmt::While { cond, body, .. } => {
                populate_expr_captures(cond, globals);
                populate_stmt_list_captures(body, globals);
            }
            Stmt::For { iterable, body, .. } => {
                populate_expr_captures(iterable, globals);
                populate_stmt_list_captures(body, globals);
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
        }
    }
}

struct CaptureCollector<'a> {
    locals: HashSet<BindingId>,
    globals: &'a HashSet<BindingId>,
    seen: HashSet<BindingId>,
    ordered: Vec<BindingId>,
}

impl CaptureCollector<'_> {
    fn use_id(&mut self, id: BindingId) {
        if !self.locals.contains(&id) && !self.globals.contains(&id) && self.seen.insert(id) {
            self.ordered.push(id);
        }
    }
}

fn analyze_function_captures(
    params: &[Param],
    implicit_bindings: &[BindingId],
    body: &mut Body,
    globals: &HashSet<BindingId>,
) -> Vec<BindingId> {
    let mut locals: HashSet<BindingId> = params.iter().map(|param| param.binding_id).collect();
    locals.extend(implicit_bindings.iter().copied());
    collect_body_locals(body, &mut locals);

    let mut collector = CaptureCollector {
        locals,
        globals,
        seen: HashSet::new(),
        ordered: Vec::new(),
    };
    scan_body_uses(body, &mut collector);
    collector.ordered
}

fn collect_body_locals(body: &Body, locals: &mut HashSet<BindingId>) {
    match body {
        Body::Block(stmts) => collect_stmt_locals(stmts, locals),
        Body::Single(stmt) => collect_one_stmt_locals(stmt, locals),
    }
}

fn collect_stmt_locals(stmts: &[Stmt], locals: &mut HashSet<BindingId>) {
    for stmt in stmts { collect_one_stmt_locals(stmt, locals); }
}

fn collect_one_stmt_locals(stmt: &Stmt, locals: &mut HashSet<BindingId>) {
    match stmt {
        Stmt::Binding(binding) => {
            locals.insert(binding.binding_id);
            collect_expr_locals(&binding.value, locals);
        }
        Stmt::Assign { value, .. } => collect_expr_locals(value, locals),
        Stmt::FieldAssign { recv, value, .. } => {
            collect_expr_locals(recv, locals);
            collect_expr_locals(value, locals);
        }
        Stmt::ExprStmt { expr, .. } => collect_expr_locals(expr, locals),
        Stmt::Return { value, .. } => {
            if let Some(value) = value { collect_expr_locals(value, locals); }
        }
        Stmt::If { branches, else_body, .. } => {
            for (cond, body) in branches {
                collect_expr_locals(cond, locals);
                collect_stmt_locals(body, locals);
            }
            if let Some(body) = else_body { collect_stmt_locals(body, locals); }
        }
        Stmt::While { cond, body, .. } => {
            collect_expr_locals(cond, locals);
            collect_stmt_locals(body, locals);
        }
        Stmt::For { binding_id, iterable, body, .. } => {
            locals.insert(*binding_id);
            collect_expr_locals(iterable, locals);
            collect_stmt_locals(body, locals);
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
    }
}

fn collect_expr_locals(expr: &Expr, locals: &mut HashSet<BindingId>) {
    match expr {
        Expr::FuncLit { .. } => {}
        Expr::Str(parts, ..) => {
            for part in parts {
                if let StrPart::Hole(hole) = part { collect_expr_locals(hole, locals); }
            }
        }
        Expr::Cast { expr, .. }
        | Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Propagate { expr, .. } => collect_expr_locals(expr, locals),
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_locals(lhs, locals);
            collect_expr_locals(rhs, locals);
        }
        Expr::Ternary { cond, then_expr, else_expr, .. } => {
            collect_expr_locals(cond, locals);
            collect_expr_locals(then_expr, locals);
            collect_expr_locals(else_expr, locals);
        }
        Expr::Call { callee, args, .. } => {
            collect_expr_locals(callee, locals);
            for arg in args { collect_expr_locals(&arg.value, locals); }
        }
        Expr::MethodCall { recv, args, .. } => {
            collect_expr_locals(recv, locals);
            for arg in args { collect_expr_locals(&arg.value, locals); }
        }
        Expr::Field { recv, .. } => collect_expr_locals(recv, locals),
        Expr::Index { recv, idx, .. } => {
            collect_expr_locals(recv, locals);
            collect_expr_locals(idx, locals);
        }
        Expr::ArrayLit { elems, .. } => {
            for elem in elems { collect_expr_locals(elem, locals); }
        }
        Expr::Match { subject, arms, .. } => {
            collect_expr_locals(subject, locals);
            for arm in arms {
                if let Some(id) = arm.binding_id { locals.insert(id); }
                match &arm.body {
                    ArmBody::Block(stmts) => collect_stmt_locals(stmts, locals),
                    ArmBody::Value(value) | ArmBody::Ret(value) => collect_expr_locals(value, locals),
                }
            }
        }
        Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::Ident(..) | Expr::This(..) => {}
    }
}

fn scan_body_uses(body: &mut Body, collector: &mut CaptureCollector<'_>) {
    match body {
        Body::Block(stmts) => scan_stmt_uses(stmts, collector),
        Body::Single(stmt) => scan_one_stmt_uses(stmt, collector),
    }
}

fn scan_stmt_uses(stmts: &mut [Stmt], collector: &mut CaptureCollector<'_>) {
    for stmt in stmts { scan_one_stmt_uses(stmt, collector); }
}

fn scan_one_stmt_uses(stmt: &mut Stmt, collector: &mut CaptureCollector<'_>) {
    match stmt {
        Stmt::Binding(binding) => scan_expr_uses(&mut binding.value, collector),
        Stmt::Assign { target_id, value, .. } => {
            collector.use_id(*target_id);
            scan_expr_uses(value, collector);
        }
        Stmt::FieldAssign { recv, value, .. } => {
            scan_expr_uses(recv, collector);
            scan_expr_uses(value, collector);
        }
        Stmt::ExprStmt { expr, .. } => scan_expr_uses(expr, collector),
        Stmt::Return { value, .. } => {
            if let Some(value) = value { scan_expr_uses(value, collector); }
        }
        Stmt::If { branches, else_body, .. } => {
            for (cond, body) in branches {
                scan_expr_uses(cond, collector);
                scan_stmt_uses(body, collector);
            }
            if let Some(body) = else_body { scan_stmt_uses(body, collector); }
        }
        Stmt::While { cond, body, .. } => {
            scan_expr_uses(cond, collector);
            scan_stmt_uses(body, collector);
        }
        Stmt::For { iterable, body, .. } => {
            scan_expr_uses(iterable, collector);
            scan_stmt_uses(body, collector);
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
    }
}

fn scan_expr_uses(expr: &mut Expr, collector: &mut CaptureCollector<'_>) {
    match expr {
        Expr::Ident(_, Some(id), ..) => collector.use_id(*id),
        Expr::Ident(_, None, ..) | Expr::This(..) | Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) => {}
        Expr::Str(parts, ..) => {
            for part in parts {
                if let StrPart::Hole(hole) = part { scan_expr_uses(hole, collector); }
            }
        }
        Expr::Cast { expr, .. }
        | Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Propagate { expr, .. } => scan_expr_uses(expr, collector),
        Expr::Binary { lhs, rhs, .. } => {
            scan_expr_uses(lhs, collector);
            scan_expr_uses(rhs, collector);
        }
        Expr::Ternary { cond, then_expr, else_expr, .. } => {
            scan_expr_uses(cond, collector);
            scan_expr_uses(then_expr, collector);
            scan_expr_uses(else_expr, collector);
        }
        Expr::Call { callee, args, .. } => {
            scan_expr_uses(callee, collector);
            for arg in args { scan_expr_uses(&mut arg.value, collector); }
        }
        Expr::MethodCall { recv, args, .. } => {
            scan_expr_uses(recv, collector);
            for arg in args { scan_expr_uses(&mut arg.value, collector); }
        }
        Expr::Field { recv, .. } => scan_expr_uses(recv, collector),
        Expr::Index { recv, idx, .. } => {
            scan_expr_uses(recv, collector);
            scan_expr_uses(idx, collector);
        }
        Expr::ArrayLit { elems, .. } => {
            for elem in elems { scan_expr_uses(elem, collector); }
        }
        Expr::FuncLit { params, implicit_bindings, captures, body, .. } => {
            let child = analyze_function_captures(params, implicit_bindings, body, collector.globals);
            *captures = child.clone();
            for id in child { collector.use_id(id); }
        }
        Expr::Match { subject, arms, .. } => {
            scan_expr_uses(subject, collector);
            for arm in arms {
                match &mut arm.body {
                    ArmBody::Block(stmts) => scan_stmt_uses(stmts, collector),
                    ArmBody::Value(value) | ArmBody::Ret(value) => scan_expr_uses(value, collector),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Type projection visitor
// ---------------------------------------------------------------------------

impl CheckedProgram {
    pub(crate) fn for_each_ty(&self, visit: &mut impl FnMut(&Ty)) {
        for item in &self.items {
            match item {
                Item::Binding(binding) => visit_binding_types(binding, visit),
                Item::StructDef(def) => {
                    for field in &def.fields {
                        visit(&field.ty);
                        if let Some(default) = &field.default { visit_expr_types(default, visit); }
                    }
                }
            }
        }
    }
}

fn visit_binding_types(binding: &Binding, visit: &mut impl FnMut(&Ty)) {
    visit(&binding.ty);
    if let Some(receiver) = &binding.receiver { visit(receiver); }
    visit_expr_types(&binding.value, visit);
}

fn visit_body_types(body: &Body, visit: &mut impl FnMut(&Ty)) {
    match body {
        Body::Block(stmts) => visit_stmts_types(stmts, visit),
        Body::Single(stmt) => visit_stmt_types(stmt, visit),
    }
}

fn visit_stmts_types(stmts: &[Stmt], visit: &mut impl FnMut(&Ty)) {
    for stmt in stmts { visit_stmt_types(stmt, visit); }
}

fn visit_stmt_types(stmt: &Stmt, visit: &mut impl FnMut(&Ty)) {
    match stmt {
        Stmt::Binding(binding) => visit_binding_types(binding, visit),
        Stmt::Assign { value, .. } => visit_expr_types(value, visit),
        Stmt::FieldAssign { recv, value, .. } => {
            visit_expr_types(recv, visit);
            visit_expr_types(value, visit);
        }
        Stmt::ExprStmt { expr, .. } => visit_expr_types(expr, visit),
        Stmt::Return { value, .. } => {
            if let Some(value) = value { visit_expr_types(value, visit); }
        }
        Stmt::If { branches, else_body, .. } => {
            for (cond, body) in branches {
                visit_expr_types(cond, visit);
                visit_stmts_types(body, visit);
            }
            if let Some(body) = else_body { visit_stmts_types(body, visit); }
        }
        Stmt::While { cond, body, .. } => {
            visit_expr_types(cond, visit);
            visit_stmts_types(body, visit);
        }
        Stmt::For { ty, iterable, body, .. } => {
            visit(ty);
            visit_expr_types(iterable, visit);
            visit_stmts_types(body, visit);
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
    }
}

fn visit_expr_types(expr: &Expr, visit: &mut impl FnMut(&Ty)) {
    visit(expr.ty());
    if let Some(CallTarget::Method(MethodTarget::User { receiver, .. })) = expr.call_target() {
        visit(receiver);
    }
    match expr {
        Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::Ident(..) | Expr::This(..) => {}
        Expr::Str(parts, ..) => {
            for part in parts {
                if let StrPart::Hole(hole) = part { visit_expr_types(hole, visit); }
            }
        }
        Expr::Cast { expr, .. }
        | Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Propagate { expr, .. } => visit_expr_types(expr, visit),
        Expr::Binary { lhs, rhs, .. } => {
            visit_expr_types(lhs, visit);
            visit_expr_types(rhs, visit);
        }
        Expr::Ternary { cond, then_expr, else_expr, .. } => {
            visit_expr_types(cond, visit);
            visit_expr_types(then_expr, visit);
            visit_expr_types(else_expr, visit);
        }
        Expr::Call { callee, args, .. } => {
            visit_expr_types(callee, visit);
            for arg in args { visit_expr_types(&arg.value, visit); }
        }
        Expr::MethodCall { recv, args, .. } => {
            visit_expr_types(recv, visit);
            for arg in args { visit_expr_types(&arg.value, visit); }
        }
        Expr::Field { recv, .. } => visit_expr_types(recv, visit),
        Expr::Index { recv, idx, .. } => {
            visit_expr_types(recv, visit);
            visit_expr_types(idx, visit);
        }
        Expr::ArrayLit { elems, .. } => {
            for elem in elems { visit_expr_types(elem, visit); }
        }
        Expr::FuncLit { params, body, .. } => {
            for param in params { visit(&param.ty); }
            visit_body_types(body, visit);
        }
        Expr::Match { subject, arms, .. } => {
            visit_expr_types(subject, visit);
            for arm in arms {
                match &arm.body {
                    ArmBody::Block(stmts) => visit_stmts_types(stmts, visit),
                    ArmBody::Value(value) | ArmBody::Ret(value) => visit_expr_types(value, visit),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::{project_ty, projected_ty, VTy};

    #[test]
    fn checked_hir_has_exact_types_and_stable_targets() {
        let source = r#"
struct point { val i32 x = 7 }
func i32 point.bump = (i32 amount) -> return self.x + amount
func i32 helper = (i32 value) -> return value
func i32 main = () -> {
    val i32 outer = 4
    func i32 capture = () -> return outer
    val point p = point()
    val i32 a = p.x
    val i32 b = p bump 1
    val i32 c = helper outer
    return a + b + c + capture
}
"#;
        let tokens = crate::lexer::lex(source).unwrap();
        let program = crate::parser::parse(tokens).unwrap();
        let checked = crate::sema::check(program).unwrap();
        let projections = project_ty(&checked);
        checked.for_each_ty(&mut |ty| {
            assert!(!ty.contains_unknown(), "仍含未确定类型 {}", ty.name());
            assert!(!matches!(projected_ty(&projections, ty), VTy::Unknown));
        });

        let mut saw_field = false;
        let mut saw_method = false;
        let mut saw_capture = false;
        fn walk(expr: &Expr, saw_field: &mut bool, saw_method: &mut bool, saw_capture: &mut bool) {
            match expr {
                Expr::Ident(_, id, ..) => assert!(id.is_some(), "可求值 ident 必须有 BindingId"),
                Expr::Field { recv, field_index, .. } => {
                    *saw_field = true;
                    assert_eq!(*field_index, 0);
                    walk(recv, saw_field, saw_method, saw_capture);
                }
                Expr::MethodCall { recv, args, info, .. } => {
                    if matches!(info.call_target, Some(CallTarget::Method(MethodTarget::User { id: Some(_), .. }))) {
                        *saw_method = true;
                    }
                    walk(recv, saw_field, saw_method, saw_capture);
                    for arg in args { walk(&arg.value, saw_field, saw_method, saw_capture); }
                }
                Expr::FuncLit { captures, body, .. } => {
                    if !captures.is_empty() { *saw_capture = true; }
                    match body.as_ref() {
                        Body::Block(stmts) => for stmt in stmts { walk_stmt(stmt, saw_field, saw_method, saw_capture); },
                        Body::Single(stmt) => walk_stmt(stmt, saw_field, saw_method, saw_capture),
                    }
                }
                Expr::Str(parts, ..) => for part in parts { if let StrPart::Hole(e) = part { walk(e, saw_field, saw_method, saw_capture); } },
                Expr::Cast { expr, .. } | Expr::Neg { expr, .. } | Expr::Not { expr, .. } | Expr::BitNot { expr, .. } | Expr::Propagate { expr, .. } => walk(expr, saw_field, saw_method, saw_capture),
                Expr::Binary { lhs, rhs, .. } => { walk(lhs, saw_field, saw_method, saw_capture); walk(rhs, saw_field, saw_method, saw_capture); }
                Expr::Ternary { cond, then_expr, else_expr, .. } => { walk(cond, saw_field, saw_method, saw_capture); walk(then_expr, saw_field, saw_method, saw_capture); walk(else_expr, saw_field, saw_method, saw_capture); }
                Expr::Call { callee, args, info, .. } => {
                    if matches!(info.call_target, Some(CallTarget::FunctionValue)) { walk(callee, saw_field, saw_method, saw_capture); }
                    for arg in args { walk(&arg.value, saw_field, saw_method, saw_capture); }
                }
                Expr::Index { recv, idx, .. } => { walk(recv, saw_field, saw_method, saw_capture); walk(idx, saw_field, saw_method, saw_capture); }
                Expr::ArrayLit { elems, .. } => for e in elems { walk(e, saw_field, saw_method, saw_capture); },
                Expr::Match { subject, arms, .. } => {
                    walk(subject, saw_field, saw_method, saw_capture);
                    for arm in arms {
                        match &arm.body {
                            ArmBody::Block(stmts) => for stmt in stmts { walk_stmt(stmt, saw_field, saw_method, saw_capture); },
                            ArmBody::Value(e) | ArmBody::Ret(e) => walk(e, saw_field, saw_method, saw_capture),
                        }
                    }
                }
                Expr::Int(..) | Expr::Float(..) | Expr::Bool(..) | Expr::This(..) => {}
            }
        }
        fn walk_stmt(stmt: &Stmt, a: &mut bool, b: &mut bool, c: &mut bool) {
            match stmt {
                Stmt::Binding(x) => walk(&x.value, a, b, c),
                Stmt::Assign { value, .. } => walk(value, a, b, c),
                Stmt::FieldAssign { recv, value, .. } => { walk(recv, a, b, c); walk(value, a, b, c); }
                Stmt::ExprStmt { expr, .. } => walk(expr, a, b, c),
                Stmt::Return { value, .. } => if let Some(e) = value { walk(e, a, b, c); },
                Stmt::If { branches, else_body, .. } => {
                    for (cond, body) in branches { walk(cond, a, b, c); for s in body { walk_stmt(s, a, b, c); } }
                    if let Some(body) = else_body { for s in body { walk_stmt(s, a, b, c); } }
                }
                Stmt::While { cond, body, .. } => { walk(cond, a, b, c); for s in body { walk_stmt(s, a, b, c); } }
                Stmt::For { iterable, body, .. } => { walk(iterable, a, b, c); for s in body { walk_stmt(s, a, b, c); } }
                Stmt::Break { .. } | Stmt::Continue { .. } => {}
            }
        }

        for item in &checked.items {
            if let Item::Binding(binding) = item {
                walk(&binding.value, &mut saw_field, &mut saw_method, &mut saw_capture);
            }
        }
        assert!(saw_field);
        assert!(saw_method);
        assert!(saw_capture);
    }
}
