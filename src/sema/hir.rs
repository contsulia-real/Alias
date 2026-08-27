//! sema 输出的类型化 HIR。parser AST 只描述语法；进入 codegen 的每个表达式
//! 都携带最终静态类型，调用表达式还携带 sema 已解析的调用目标。

// HIR 保留语句、声明和实参 span 作为源码溯源元数据；并非每个 span 都会在
// 当前原生后端产生运行时诊断。
#![allow(dead_code)]

pub use crate::ast::{BinOp, BindKind, CtorKind, Import, Pattern};
use crate::sema::types::Ty;
use crate::{AliasError, AliasResult, Span};
use std::collections::HashMap;

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
    pub(crate) ty: Ty,
    pub(crate) name: String,
    pub(crate) span: Span,
}

#[derive(Debug, Clone)]
pub(crate) enum Stmt {
    Binding(Binding),
    Assign {
        target: String,
        value: Expr,
        span: Span,
    },
    FieldAssign {
        recv: Box<Expr>,
        field: String,
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
        ty: Ty,
        name: String,
        iterable: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
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
    User { receiver: Ty, name: String },
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
}

pub(super) struct LowerFacts {
    pub(super) exprs: HashMap<usize, ExprInfo>,
    pub(super) bindings: HashMap<usize, Ty>,
    pub(super) receivers: HashMap<usize, Ty>,
    pub(super) fields: HashMap<usize, Ty>,
    pub(super) params: HashMap<usize, Ty>,
    pub(super) fors: HashMap<usize, Ty>,
}

#[derive(Debug, Clone)]
pub(crate) enum Expr {
    Int(u64, Span, ExprInfo),
    Float(f64, Span, ExprInfo),
    Bool(bool, Span, ExprInfo),
    Str(Vec<StrPart>, Span, ExprInfo),
    Ident(String, Span, ExprInfo),
    This(Span, ExprInfo),
    Cast {
        expr: Box<Expr>,
        span: Span,
        info: ExprInfo,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
        info: ExprInfo,
    },
    Neg {
        expr: Box<Expr>,
        span: Span,
        info: ExprInfo,
    },
    Not {
        expr: Box<Expr>,
        span: Span,
        info: ExprInfo,
    },
    BitNot {
        expr: Box<Expr>,
        span: Span,
        info: ExprInfo,
    },
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
        span: Span,
        info: ExprInfo,
    },
    Index {
        recv: Box<Expr>,
        idx: Box<Expr>,
        span: Span,
        info: ExprInfo,
    },
    ArrayLit {
        elems: Vec<Expr>,
        span: Span,
        info: ExprInfo,
    },
    FuncLit {
        params: Vec<Param>,
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
    Propagate {
        expr: Box<Expr>,
        span: Span,
        info: ExprInfo,
    },
}

impl Expr {
    pub(crate) fn info(&self) -> &ExprInfo {
        match self {
            Self::Int(_, _, info)
            | Self::Float(_, _, info)
            | Self::Bool(_, _, info)
            | Self::Str(_, _, info)
            | Self::Ident(_, _, info)
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
            | Self::Ident(_, span, _)
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
    let checked = CheckedProgram {
        imports: program.imports.clone(),
        items: program
            .items
            .iter()
            .map(|item| lower_item(item, &mut facts))
            .collect::<AliasResult<Vec<_>>>()?,
    };
    if let Some(info) = facts.exprs.values().next() {
        return Err(AliasError {
            msg: format!(
                "内部 sema 不变式被破坏: 存在未消费的表达式类型 {}",
                info.ty.name()
            ),
            span: Span::default(),
        });
    }
    let leftovers = [
        ("绑定", facts.bindings.len()),
        ("方法接收者", facts.receivers.len()),
        ("结构体字段", facts.fields.len()),
        ("函数参数", facts.params.len()),
        ("for 循环变量", facts.fors.len()),
    ];
    if let Some((what, count)) = leftovers.into_iter().find(|(_, count)| *count != 0) {
        return Err(AliasError {
            msg: format!("内部 sema 不变式被破坏: 存在 {count} 条未消费的{what}类型"),
            span: Span::default(),
        });
    }
    Ok(checked)
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
                    Ok(StructField {
                        name: field.name.clone(),
                        mutable: field.mutable,
                        ty: take_node_type(&mut facts.fields, field, field.span, "结构体字段")?,
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
    Ok(Binding {
        is_pub: binding.is_pub,
        kind: binding.kind,
        ty: take_node_type(&mut facts.bindings, binding, binding.span, "绑定")?,
        name: binding.name.clone(),
        receiver: facts
            .receivers
            .remove(&(binding as *const crate::ast::Binding as usize)),
        value: lower_expr(&binding.value, facts)?,
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
    Ok(match stmt {
        crate::ast::Stmt::Binding(binding) => Stmt::Binding(lower_binding(binding, facts)?),
        crate::ast::Stmt::Assign {
            target,
            value,
            span,
        } => Stmt::Assign {
            target: target.clone(),
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
            ty: _,
            name,
            iterable,
            body,
            span,
        } => Stmt::For {
            ty: take_node_type(&mut facts.fors, stmt, *span, "for 循环变量")?,
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
    let Some(info) = facts.exprs.remove(&key) else {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: 表达式缺少静态类型".into(),
            span: expr.span(),
        });
    };
    let is_call = matches!(
        expr,
        crate::ast::Expr::Call { .. } | crate::ast::Expr::MethodCall { .. }
    );
    if is_call && info.call_target.is_none() {
        return Err(AliasError {
            msg: "内部 sema 不变式被破坏: 调用表达式缺少已解析目标".into(),
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
        crate::ast::Expr::Ident(name, span) => Expr::Ident(name.clone(), *span, info),
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
                    Ok(Param {
                        ty: take_node_type(&mut facts.params, param, param.span, "函数参数")?,
                        name: param.name.clone(),
                        span: param.span,
                    })
                })
                .collect::<AliasResult<Vec<_>>>()?,
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
    Ok(MatchArm {
        pattern: arm.pattern.clone(),
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

fn take_node_type<T>(
    facts: &mut HashMap<usize, Ty>,
    node: &T,
    span: Span,
    what: &str,
) -> AliasResult<Ty> {
    facts
        .remove(&(node as *const T as usize))
        .ok_or_else(|| AliasError {
            msg: format!("内部 sema 不变式被破坏: {what}缺少静态类型"),
            span,
        })
}

impl CheckedProgram {
    pub(crate) fn for_each_ty(&self, visit: &mut impl FnMut(&Ty)) {
        for item in &self.items {
            match item {
                Item::Binding(binding) => visit_binding_types(binding, visit),
                Item::StructDef(def) => {
                    for field in &def.fields {
                        visit(&field.ty);
                        if let Some(default) = &field.default {
                            visit_expr_types(default, visit);
                        }
                    }
                }
            }
        }
    }
}

fn visit_binding_types(binding: &Binding, visit: &mut impl FnMut(&Ty)) {
    visit(&binding.ty);
    if let Some(receiver) = &binding.receiver {
        visit(receiver);
    }
    visit_expr_types(&binding.value, visit);
}

fn visit_body_types(body: &Body, visit: &mut impl FnMut(&Ty)) {
    match body {
        Body::Block(stmts) => visit_stmts_types(stmts, visit),
        Body::Single(stmt) => visit_stmt_types(stmt, visit),
    }
}

fn visit_stmts_types(stmts: &[Stmt], visit: &mut impl FnMut(&Ty)) {
    for stmt in stmts {
        visit_stmt_types(stmt, visit);
    }
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
            if let Some(value) = value {
                visit_expr_types(value, visit);
            }
        }
        Stmt::If {
            branches,
            else_body,
            ..
        } => {
            for (cond, body) in branches {
                visit_expr_types(cond, visit);
                visit_stmts_types(body, visit);
            }
            if let Some(body) = else_body {
                visit_stmts_types(body, visit);
            }
        }
        Stmt::While { cond, body, .. } => {
            visit_expr_types(cond, visit);
            visit_stmts_types(body, visit);
        }
        Stmt::For {
            ty, iterable, body, ..
        } => {
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
                if let StrPart::Hole(hole) = part {
                    visit_expr_types(hole, visit);
                }
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
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            visit_expr_types(cond, visit);
            visit_expr_types(then_expr, visit);
            visit_expr_types(else_expr, visit);
        }
        Expr::Call { callee, args, .. } => {
            visit_expr_types(callee, visit);
            for arg in args {
                visit_expr_types(&arg.value, visit);
            }
        }
        Expr::MethodCall { recv, args, .. } => {
            visit_expr_types(recv, visit);
            for arg in args {
                visit_expr_types(&arg.value, visit);
            }
        }
        Expr::Field { recv, .. } => visit_expr_types(recv, visit),
        Expr::Index { recv, idx, .. } => {
            visit_expr_types(recv, visit);
            visit_expr_types(idx, visit);
        }
        Expr::ArrayLit { elems, .. } => {
            for elem in elems {
                visit_expr_types(elem, visit);
            }
        }
        Expr::FuncLit { params, body, .. } => {
            for param in params {
                visit(&param.ty);
            }
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
    use std::collections::HashSet;

    #[test]
    fn every_expression_shape_has_an_exact_projection_and_calls_have_targets() {
        let source = r#"
struct point {
    val i32 x = 7
}
func i32 point.bump = (i32 amount) -> return self.x + amount
func result<i32, string> source = () -> return ok(4)
func result<i32, string> propagated = () -> {
    val i32 value = source()?
    return ok(value)
}
func i32 helper = (i32 value) -> {
    val string self_name = typeof(this)
    return value
}
func i32 main = () -> {
    var i32 mutable = 1
    increase(mutable)
    decrease(mutable)
    val f64 floating = 1.5
    val bool flag = true
    val string text = 'v=$mutable'
    val i64 casted = (i64) mutable
    val i32 binary = mutable + 1
    val i32 negative = -mutable
    val bool inverted = !flag
    val i32 complemented = ~mutable
    val i32 selected = flag ? mutable : 0
    val i32 called = helper(mutable)
    val i32 numeric_method = mutable.plus(1)
    val bool bool_method = flag.not()
    val i32 string_len = text.len()
    val string string_upper = text.upper()
    val string string_lower = text.lower()
    val string string_method = text.trim()
    val point p = point()
    val i32 field = p.x
    val i32 user_method = p.bump(1)
    var array<i32> values = [1, 2]
    values.push(3)
    val i32 array_len = values.len()
    val i32 popped = values.pop()
    val iterator<i32> iterator = values.iterator()
    val i32 indexed = values[0]
    val i32 invoked = ((i32 value) -> return value + 1)(1)
    val i32 matched = match flag {
        true -> 1,
        false -> 2,
    }
    val string converted = from(mutable)
    val bool fallback = try_from(flag)
    println(text)
    print(text)
    return binary + selected + called + numeric_method + field + user_method + array_len + popped + indexed + invoked + matched
}
"#;
        let tokens = crate::lexer::lex(source).unwrap();
        let program = crate::parser::parse(tokens).unwrap();
        let checked = crate::sema::check(program).unwrap();
        let projections = project_ty(&checked);
        checked.for_each_ty(&mut |ty| {
            assert!(
                !ty.contains_unknown(),
                "投影输入仍含未确定类型 {}",
                ty.name()
            );
            assert!(!matches!(projected_ty(&projections, ty), VTy::Unknown));
        });
        let mut shapes = HashSet::new();
        for item in &checked.items {
            match item {
                Item::Binding(binding) => visit_expr(&binding.value, &mut shapes),
                Item::StructDef(def) => {
                    for field in &def.fields {
                        if let Some(default) = &field.default {
                            visit_expr(default, &mut shapes);
                        }
                    }
                }
            }
        }
        assert_eq!(
            shapes,
            HashSet::from([
                "int",
                "float",
                "bool",
                "str",
                "ident",
                "this",
                "cast",
                "binary",
                "neg",
                "not",
                "bit_not",
                "ternary",
                "call",
                "method_call",
                "field",
                "index",
                "array_lit",
                "func_lit",
                "match",
                "propagate",
                "target:function",
                "target:struct",
                "target:result",
                "target:print",
                "target:println",
                "target:typeof",
                "target:from",
                "target:try_from",
                "target:increase",
                "target:decrease",
                "target:numeric_method",
                "target:bool_not",
                "target:string_len",
                "target:string_upper",
                "target:string_lower",
                "target:string_trim",
                "target:array_len",
                "target:array_push",
                "target:array_pop",
                "target:array_iterator",
                "target:user_method",
            ])
        );
    }

    fn visit_body(body: &Body, shapes: &mut HashSet<&'static str>) {
        match body {
            Body::Block(stmts) => visit_stmts(stmts, shapes),
            Body::Single(stmt) => visit_stmt(stmt, shapes),
        }
    }

    fn visit_stmts(stmts: &[Stmt], shapes: &mut HashSet<&'static str>) {
        for stmt in stmts {
            visit_stmt(stmt, shapes);
        }
    }

    fn visit_stmt(stmt: &Stmt, shapes: &mut HashSet<&'static str>) {
        match stmt {
            Stmt::Binding(binding) => visit_expr(&binding.value, shapes),
            Stmt::Assign { value, .. } => visit_expr(value, shapes),
            Stmt::FieldAssign { recv, value, .. } => {
                visit_expr(recv, shapes);
                visit_expr(value, shapes);
            }
            Stmt::ExprStmt { expr, .. } => visit_expr(expr, shapes),
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    visit_expr(value, shapes);
                }
            }
            Stmt::If {
                branches,
                else_body,
                ..
            } => {
                for (cond, body) in branches {
                    visit_expr(cond, shapes);
                    visit_stmts(body, shapes);
                }
                if let Some(body) = else_body {
                    visit_stmts(body, shapes);
                }
            }
            Stmt::While { cond, body, .. } => {
                visit_expr(cond, shapes);
                visit_stmts(body, shapes);
            }
            Stmt::For { iterable, body, .. } => {
                visit_expr(iterable, shapes);
                visit_stmts(body, shapes);
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
        }
    }

    fn visit_expr(expr: &Expr, shapes: &mut HashSet<&'static str>) {
        assert!(
            !expr.ty().contains_unknown(),
            "{}:{} 仍含未确定类型 {}",
            expr.span().line,
            expr.span().col,
            expr.ty().name()
        );
        if matches!(expr, Expr::Call { .. } | Expr::MethodCall { .. }) {
            assert!(expr.call_target().is_some(), "调用表达式缺少已解析目标");
        } else {
            assert!(expr.call_target().is_none(), "非调用表达式携带了调用目标");
        }
        if let Some(target) = expr.call_target() {
            let target_shape = match target {
                CallTarget::FunctionValue => "target:function",
                CallTarget::StructConstructor(_) => "target:struct",
                CallTarget::ResultConstructor(_) => "target:result",
                CallTarget::Builtin(builtin) => match builtin {
                    BuiltinCall::Print => "target:print",
                    BuiltinCall::Println => "target:println",
                    BuiltinCall::Typeof => "target:typeof",
                    BuiltinCall::From => "target:from",
                    BuiltinCall::TryFrom => "target:try_from",
                    BuiltinCall::Increase => "target:increase",
                    BuiltinCall::Decrease => "target:decrease",
                },
                CallTarget::Method(method) => match method {
                    MethodTarget::Numeric(_) => "target:numeric_method",
                    MethodTarget::BoolNot => "target:bool_not",
                    MethodTarget::StringLen => "target:string_len",
                    MethodTarget::StringUpper => "target:string_upper",
                    MethodTarget::StringLower => "target:string_lower",
                    MethodTarget::StringTrim => "target:string_trim",
                    MethodTarget::ArrayLen => "target:array_len",
                    MethodTarget::ArrayPush => "target:array_push",
                    MethodTarget::ArrayPop => "target:array_pop",
                    MethodTarget::ArrayIterator => "target:array_iterator",
                    MethodTarget::User { .. } => "target:user_method",
                },
            };
            shapes.insert(target_shape);
        }

        let shape = match expr {
            Expr::Int(..) => "int",
            Expr::Float(..) => "float",
            Expr::Bool(..) => "bool",
            Expr::Str(parts, ..) => {
                for part in parts {
                    if let StrPart::Hole(hole) = part {
                        visit_expr(hole, shapes);
                    }
                }
                "str"
            }
            Expr::Ident(..) => "ident",
            Expr::This(..) => "this",
            Expr::Cast { expr, .. } => {
                visit_expr(expr, shapes);
                "cast"
            }
            Expr::Binary { lhs, rhs, .. } => {
                visit_expr(lhs, shapes);
                visit_expr(rhs, shapes);
                "binary"
            }
            Expr::Neg { expr, .. } => {
                visit_expr(expr, shapes);
                "neg"
            }
            Expr::Not { expr, .. } => {
                visit_expr(expr, shapes);
                "not"
            }
            Expr::BitNot { expr, .. } => {
                visit_expr(expr, shapes);
                "bit_not"
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
                ..
            } => {
                visit_expr(cond, shapes);
                visit_expr(then_expr, shapes);
                visit_expr(else_expr, shapes);
                "ternary"
            }
            Expr::Call { callee, args, .. } => {
                visit_expr(callee, shapes);
                for arg in args {
                    visit_expr(&arg.value, shapes);
                }
                "call"
            }
            Expr::MethodCall { recv, args, .. } => {
                visit_expr(recv, shapes);
                for arg in args {
                    visit_expr(&arg.value, shapes);
                }
                "method_call"
            }
            Expr::Field { recv, .. } => {
                visit_expr(recv, shapes);
                "field"
            }
            Expr::Index { recv, idx, .. } => {
                visit_expr(recv, shapes);
                visit_expr(idx, shapes);
                "index"
            }
            Expr::ArrayLit { elems, .. } => {
                for elem in elems {
                    visit_expr(elem, shapes);
                }
                "array_lit"
            }
            Expr::FuncLit { body, .. } => {
                visit_body(body, shapes);
                "func_lit"
            }
            Expr::Match { subject, arms, .. } => {
                visit_expr(subject, shapes);
                for arm in arms {
                    match &arm.body {
                        ArmBody::Block(stmts) => visit_stmts(stmts, shapes),
                        ArmBody::Value(value) | ArmBody::Ret(value) => visit_expr(value, shapes),
                    }
                }
                "match"
            }
            Expr::Propagate { expr, .. } => {
                visit_expr(expr, shapes);
                "propagate"
            }
        };
        shapes.insert(shape);
    }
}
