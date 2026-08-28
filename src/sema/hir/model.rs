//! typed HIR data model. No name resolution or backend inference belongs here.

pub use crate::ast::{BinOp, BindKind, CtorKind, Import, Pattern};
use crate::sema::types::Ty;
use crate::Span;
use std::collections::HashMap;

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
    pub(crate) implicit_zero_callee: Option<Ty>,
}

pub(crate) struct LowerFacts {
    pub(crate) exprs: HashMap<usize, ExprInfo>,
    pub(crate) bindings: HashMap<usize, Ty>,
    pub(crate) binding_ids: HashMap<usize, BindingId>,
    pub(crate) receivers: HashMap<usize, Ty>,
    pub(crate) method_ids: HashMap<usize, MethodId>,
    pub(crate) method_self_ids: HashMap<usize, BindingId>,
    pub(crate) fields: HashMap<usize, Ty>,
    pub(crate) field_indices: HashMap<usize, usize>,
    pub(crate) field_assign_indices: HashMap<usize, usize>,
    pub(crate) params: HashMap<usize, Ty>,
    pub(crate) param_ids: HashMap<usize, BindingId>,
    pub(crate) fors: HashMap<usize, Ty>,
    pub(crate) for_ids: HashMap<usize, BindingId>,
    pub(crate) assign_target_ids: HashMap<usize, BindingId>,
    pub(crate) match_binding_ids: HashMap<usize, BindingId>,
    pub(crate) expr_binding_ids: HashMap<usize, BindingId>,
}

#[derive(Debug, Clone)]
pub(crate) enum Expr {
    Int(u64, Span, ExprInfo),
    Float(f64, Span, ExprInfo),
    Bool(bool, Span, ExprInfo),
    Str(Vec<StrPart>, Span, ExprInfo),
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
        implicit_bindings: Vec<BindingId>,
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
