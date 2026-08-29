//! typed HIR data model. No name resolution or backend inference belongs here.

pub use crate::ast::{BinOp, BindKind, CtorKind, Pattern};
use crate::sema::types::Ty;
use crate::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct BindingId(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct MethodId(pub(crate) u32);

#[derive(Debug, Clone)]
pub(crate) struct CheckedProgram {
    pub(crate) main_id: BindingId,
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
}

#[derive(Debug, Clone)]
pub(crate) struct StructField {
    pub(crate) ty: Ty,
    pub(crate) mutable: bool,
    pub(crate) default: Option<Expr>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone)]
pub(crate) struct Binding {
    pub(crate) binding_id: BindingId,
    pub(crate) owner: BindingOwner,
    pub(crate) kind: BindKind,
    pub(crate) ty: Ty,
    pub(crate) name: String,
    pub(crate) value: Expr,
    pub(crate) span: Span,
}

#[derive(Debug, Clone)]
pub(crate) enum BindingOwner {
    Ordinary,
    Method {
        method_id: MethodId,
        self_id: BindingId,
        receiver: Ty,
    },
}

impl Binding {
    pub(crate) fn is_method(&self) -> bool {
        matches!(self.owner, BindingOwner::Method { .. })
    }
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
}

/// sema 已解析的可写 target。这里只表达当前前端真正支持的 local/field 写入；
/// codegen 不得再从源码形状恢复 target identity 或 target type。
#[derive(Debug, Clone)]
pub(crate) struct PlaceInfo {
    pub(crate) ty: Ty,
    pub(crate) span: Span,
}

#[derive(Debug, Clone)]
pub(crate) enum Place {
    Local {
        binding_id: BindingId,
        info: PlaceInfo,
    },
    Field {
        recv: Box<Expr>,
        field_index: usize,
        info: PlaceInfo,
    },
}

impl Place {
    pub(crate) fn info(&self) -> &PlaceInfo {
        match self {
            Self::Local { info, .. } | Self::Field { info, .. } => info,
        }
    }

    pub(crate) fn ty(&self) -> &Ty {
        &self.info().ty
    }

    pub(crate) fn span(&self) -> Span {
        self.info().span
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Stmt {
    Binding(Binding),
    Assign {
        target: Place,
        value: Expr,
    },
    Expr {
        expr: Expr,
    },
    Return {
        value: Option<Expr>,
    },
    If {
        branches: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
    },
    For {
        binding_id: BindingId,
        ty: Ty,
        iterable: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    Break,
    Continue,
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
}

#[derive(Debug, Clone)]
pub(crate) struct CallArg {
    pub(crate) value: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuiltinCall {
    Print,
    Println,
    Increase,
    Decrease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedConversion {
    Convert,
    Identity,
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
    User { receiver: Ty, id: MethodId },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CallTarget {
    FunctionValue,
    StructConstructor {
        name: String,
        /// 与 Call.args 同序；每个命名实参对应的结构体字段索引由 sema 固化。
        arg_field_indices: Vec<usize>,
    },
    ResultConstructor(CtorKind),
    Builtin(BuiltinCall),
}

/// 表达式在完成 sema 后首先区分“一个可继续投影/读取的存储 Place”与“已经产生的 Value”。
/// OwnedTemporary/BorrowedValue/Null 属于 Value 的后续细分类，不与这一层 Place/Value 事实竞争。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExprCategory {
    Place,
    Value,
}

#[derive(Debug, Clone)]
pub(crate) struct ExprInfo {
    /// sema 已完成全部目标类型传播后的最终静态类型。backend 只能消费该结果，
    /// 不得再次根据运算符、上下文或源码名字决定子表达式应采用什么类型。
    pub(crate) ty: Ty,
    /// 节点构造期间暂为 None；lower_expr 在返回这个 resolved HIR 节点前立即写回 category。
    /// final-HIR gate 保证进入 codegen 前必为 Some 且与节点语义形状一致。
    pub(crate) category: Option<ExprCategory>,
}

#[derive(Debug, Clone)]
pub(crate) enum Expr {
    Int(u64, Span, ExprInfo),
    Float(f64, Span, ExprInfo),
    Bool(bool, Span, ExprInfo),
    Str(Vec<StrPart>, Span, ExprInfo),
    Ident(String, Option<BindingId>, Span, ExprInfo),
    This(Span, ExprInfo),
    Cast {
        expr: Box<Expr>,
        span: Span,
        info: ExprInfo,
    },
    Convert {
        expr: Box<Expr>,
        mode: ResolvedConversion,
        span: Span,
        info: ExprInfo,
    },
    /// `typeof` 在 sema 已取得静态类型名；operand 在 lowering 时只为消费 facts 而遍历，
    /// 不进入最终 HIR，因此不会求值、捕获变量或让 backend 重新解释类型名称。
    Typeof {
        type_name: String,
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
        target: CallTarget,
        span: Span,
        info: ExprInfo,
    },
    MethodCall {
        recv: Box<Expr>,
        args: Vec<CallArg>,
        target: MethodTarget,
        span: Span,
        info: ExprInfo,
    },
    Field {
        recv: Box<Expr>,
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
    ArrayLit {
        elems: Vec<Expr>,
        span: Span,
        info: ExprInfo,
    },
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
            | Self::Ident(_, _, _, info)
            | Self::This(_, info) => info,
            Self::Cast { info, .. }
            | Self::Convert { info, .. }
            | Self::Typeof { info, .. }
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

    pub(super) fn info_mut(&mut self) -> &mut ExprInfo {
        match self {
            Self::Int(_, _, info)
            | Self::Float(_, _, info)
            | Self::Bool(_, _, info)
            | Self::Str(_, _, info)
            | Self::Ident(_, _, _, info)
            | Self::This(_, info) => info,
            Self::Cast { info, .. }
            | Self::Convert { info, .. }
            | Self::Typeof { info, .. }
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

    pub(crate) fn category(&self) -> Option<ExprCategory> {
        self.info().category
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
            | Self::Convert { span, .. }
            | Self::Typeof { span, .. }
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
