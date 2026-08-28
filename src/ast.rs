//! AST — 严格对应已冻结文法。

use crate::Span;

#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Binding(Binding),
    StructDef(StructDef),
}

#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<StructField>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub mutable: bool,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindKind {
    Val,
    Var,
    Func,
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub kind: BindKind,
    pub ty: TypeExpr,
    pub name: String,
    pub receiver: Option<TypeExpr>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeExpr {
    Named(String),
    Generic(String, Vec<TypeExpr>),
}

impl TypeExpr {
    pub fn display(&self) -> String {
        match self {
            TypeExpr::Named(n) => n.clone(),
            TypeExpr::Generic(n, args) => format!(
                "{n}<{}>",
                args.iter()
                    .map(|a| a.display())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Param {
    pub ty: TypeExpr,
    pub name: String,
    pub span: Span,
}

/// 花括号块，或恰好一条无花括号语句。单语句不是隐式返回。
#[derive(Debug, Clone)]
pub enum Body {
    Block(Vec<Stmt>),
    Single(Box<Stmt>),
}

#[derive(Debug, Clone)]
pub enum Stmt {
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
    Expr {
        expr: Expr,
    },
    Return {
        value: Option<Expr>,
        span: Span,
    },
    If {
        branches: Vec<(Expr, Vec<Stmt>)>,
        else_body: Option<Vec<Stmt>>,
    },
    While {
        cond: Expr,
        body: Vec<Stmt>,
        span: Span,
    },
    For {
        ty: TypeExpr,
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
pub enum StrPartAst {
    Lit(String),
    Hole(Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CtorKind {
    Ok,
    Err,
}

/// 当前 match Pattern：通配、整体绑定、整数/布尔/字符串字面量，
/// 以及 result 的 ok/err 构造器 Pattern。构造器载荷只允许名字或 `_`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    Wildcard {
        span: Span,
    },
    Binding {
        name: String,
        span: Span,
    },
    Int {
        value: i128,
        span: Span,
    },
    Bool {
        value: bool,
        span: Span,
    },
    Str {
        value: String,
        span: Span,
    },
    Constructor {
        ctor: CtorKind,
        binding: Option<String>,
        span: Span,
    },
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wildcard { span }
            | Pattern::Binding { span, .. }
            | Pattern::Int { span, .. }
            | Pattern::Bool { span, .. }
            | Pattern::Str { span, .. }
            | Pattern::Constructor { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ArmBody {
    Block(Vec<Stmt>),
    Value(Box<Expr>),
    Ret(Box<Expr>),
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: ArmBody,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct CallArg {
    pub label: Option<String>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(u64, Span),
    Float(f64, Span),
    Bool(bool, Span),
    Str(Vec<StrPartAst>, Span),
    Ident(String, Span),
    This(Span),

    Cast {
        target: TypeExpr,
        expr: Box<Expr>,
        span: Span,
    },

    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    Neg {
        expr: Box<Expr>,
        span: Span,
    },
    Not {
        expr: Box<Expr>,
        span: Span,
    },
    BitNot {
        expr: Box<Expr>,
        span: Span,
    },
    Ternary {
        cond: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
        span: Span,
    },

    Call {
        callee: Box<Expr>,
        args: Vec<CallArg>,
        span: Span,
    },
    /// 两项无括号邻接。parser 不在这里猜 `f x` 是单参函数调用还是
    /// `value method` 的零参方法中缀；该裁决必须由 sema 基于 lhs 静态类型完成。
    Juxtapose {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    MethodCall {
        recv: Box<Expr>,
        name: String,
        args: Vec<CallArg>,
        span: Span,
    },
    Field {
        recv: Box<Expr>,
        name: String,
        span: Span,
    },
    Index {
        recv: Box<Expr>,
        idx: Box<Expr>,
        span: Span,
    },
    ArrayLit {
        elems: Vec<Expr>,
        span: Span,
    },
    FuncLit {
        params: Vec<Param>,
        body: Box<Body>,
        span: Span,
    },
    Match {
        subject: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
    Propagate {
        expr: Box<Expr>,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    NotEq,
    BitAnd,
    BitXor,
    BitOr,
    And,
    Or,
}

impl BinOp {
    pub fn display(&self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::EqEq => "==",
            BinOp::NotEq => "!=",
            BinOp::BitAnd => "&",
            BinOp::BitXor => "^",
            BinOp::BitOr => "|",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(_, s)
            | Expr::Float(_, s)
            | Expr::Bool(_, s)
            | Expr::Str(_, s)
            | Expr::Ident(_, s)
            | Expr::This(s) => *s,
            Expr::Binary { span, .. }
            | Expr::Cast { span, .. }
            | Expr::Neg { span, .. }
            | Expr::Not { span, .. }
            | Expr::BitNot { span, .. }
            | Expr::Ternary { span, .. }
            | Expr::Call { span, .. }
            | Expr::Juxtapose { span, .. }
            | Expr::MethodCall { span, .. }
            | Expr::Field { span, .. }
            | Expr::Index { span, .. }
            | Expr::ArrayLit { span, .. }
            | Expr::FuncLit { span, .. }
            | Expr::Match { span, .. }
            | Expr::Propagate { span, .. } => *span,
        }
    }
}
