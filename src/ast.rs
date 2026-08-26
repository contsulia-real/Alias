//! AST — 严格对应已定稿文法。
//!
//! 核心不变式(违反即 parser 报错, 不静默):
//! - 绑定统一文法: (public)? (val|var|func) <类型> <名字> = <表达式>
//!   类型槽强制非空 — 语言没有类型推断
//! - 函数字面量是裸表达式: (参数) -> 体; func 只是绑定词
//! - for/while 条件必须是 bool 表达式(语义检查在 sema)

use crate::Span;

#[derive(Debug, Clone)]
pub struct Program {
    pub imports: Vec<Import>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub enum Item {
    /// 顶层绑定: 目前主要是 func main 等
    Binding(Binding),
    /// struct 定义 (Phase 2a): 名字与 func/绑定共用单一命名空间
    StructDef(StructDef),
}

/// struct <名字> { 字段... } — 字段即实例内绑定 (val/var 显式可变性)
#[derive(Debug, Clone)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<StructField>,
    pub span: Span,
}

/// 字段: (val|var) <类型> <名字> (= 表达式)?
/// default = 声明期默认值; 无默认的字段在构造点必须显式命名传入
#[derive(Debug, Clone)]
pub struct StructField {
    pub name: String,
    pub mutable: bool,
    pub ty: TypeExpr,
    pub default: Option<Expr>,
    pub span: Span,
}

/// import { a.b } from './x.as' — Phase 1 解析后暂存不执行
#[derive(Debug, Clone)]
pub struct Import {
    pub names: Vec<String>,
    pub from: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindKind {
    Val,
    Var,
    Func,
}

/// 统一绑定: val/var/func <类型> <名字> = <表达式>
/// name 允许点路径(string.append)以承载扩展方法定义
#[derive(Debug, Clone)]
pub struct Binding {
    pub public: bool,
    pub kind: BindKind,
    pub ty: TypeExpr,
    pub name: String,
    /// 扩展方法定义 (Phase 2c): Some((接收者类型名, 方法名)) —
    /// 仅 func 绑定且名字为单点路径时由 parser 产出;
    /// 此时 name = 方法名, ty = 返回类型槽。方法不是绑定:
    /// 不进作用域/全局槽位, 由 sema 方法表与 codegen 静态分派接管
    pub receiver: Option<(String, String)>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
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
                args.iter().map(|a| a.display()).collect::<Vec<_>>().join(", ")
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

/// 函数体两种形态:
///   块体   = () -> { 语句... }
///   箭头体 = () -> return 表达式   (demo 先例均带 return)
#[derive(Debug, Clone)]
pub enum Body {
    Block(Vec<Stmt>),
    ArrowExpr(Box<Expr>),
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Binding(Binding),
    Assign { target: String, value: Expr, span: Span },
    /// recv.field = expr (Phase 2a) — 字段级可变性独立于绑定可变性
    FieldAssign { recv: Box<Expr>, field: String, value: Expr, span: Span },
    ExprStmt { expr: Expr, span: Span },
    Return { value: Option<Expr>, span: Span },
    For { cond: Expr, body: Vec<Stmt>, span: Span },
    While { cond: Expr, body: Vec<Stmt>, span: Span },
}

#[derive(Debug, Clone)]
pub enum StrPartAst {
    Lit(String),
    Hole(Box<Expr>),
}

/// match 臂构造器种类 — result<T,E> 内建枚举的 ok/err (Phase 2b)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtorKind {
    Ok,
    Err,
}

/// match 臂体三形态:
///   块体       = { 语句... }        尾表达式语句 = 臂值; return 收尾 = never 流
///   箭头值体   = -> 表达式          臂值即该表达式
///   箭头返回体 = -> return 表达式   never 流 (函数返回, 无臂值)
#[derive(Debug, Clone)]
pub enum ArmBody {
    Block(Vec<Stmt>),
    Value(Box<Expr>),
    Ret(Box<Expr>),
}

/// match 分支: ctor(绑定) -> 体; 绑定为 val 语义新绑定, 作用域 = 臂体
#[derive(Debug, Clone)]
pub struct MatchArm {
    pub ctor: CtorKind,
    pub binding: String,
    pub body: ArmBody,
    pub span: Span,
}

/// 调用实参: label = Some 时为命名实参 (`name = expr`)。
/// 文法上命名/位置实参共用一处语法空间 — 是否合法由 sema 按被调方裁决
/// (结构体构造必须全命名, 函数调用必须全位置)。
#[derive(Debug, Clone)]
pub struct CallArg {
    pub label: Option<String>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64, Span),
    /// 浮点字面量 (Phase 3a) — f64 承载, f32 目标按编译期舍入检查
    Float(f64, Span),
    Bool(bool, Span),
    Str(Vec<StrPartAst>, Span),
    Ident(String, Span),

    Binary { op: BinOp, lhs: Box<Expr>, rhs: Box<Expr>, span: Span },
    Neg { expr: Box<Expr>, span: Span },

    /// f(args) 与语句级无括号调用 increase i / println x 统一为 Call;
    /// 结构体构造 stat(k = v) 同形 — callee 为结构体名时 sema 判定为构造
    Call { callee: Box<Expr>, args: Vec<CallArg>, span: Span },
    /// receiver.name(args) — 实参形态与 Call 同 (命名实参在此恒非法, sema 拒)
    MethodCall { recv: Box<Expr>, name: String, args: Vec<CallArg>, span: Span },
    /// receiver.name
    Field { recv: Box<Expr>, name: String, span: Span },
    /// arr[i] (P5 已裁决; Phase 2d 落地为读语义 + 越界运行时守卫)
    Index { recv: Box<Expr>, idx: Box<Expr>, span: Span },

    /// 数组字面量 (Phase 2d): [e1, e2, ...] — 元素类型须一致,
    /// 实例 = 泄漏堆块, 变量持指针 (引用语义)
    ArrayLit { elems: Vec<Expr>, span: Span },

    FuncLit { params: Vec<Param>, body: Box<Body>, span: Span },

    /// match 表达式 (Phase 2b): 主语须为 result<T,E>, ok/err 臂必须穷尽;
    /// 值 = 非 never 臂的公共类型
    Match { subject: Box<Expr>, arms: Vec<MatchArm>, span: Span },
    /// expr? 传播糖 (P6): 脱糖 = match expr { ok(v) -> v, err(e) -> return err(e) };
    /// 仅当所在函数声明返回同型错误 result 时合法
    Propagate { expr: Box<Expr>, span: Span },

    Unit(Span),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    NotEq,
}

impl BinOp {
    pub fn display(&self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::EqEq => "==",
            BinOp::NotEq => "!=",
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
            | Expr::Unit(s) => *s,
            Expr::Binary { span, .. }
            | Expr::Neg { span, .. }
            | Expr::Call { span, .. }
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
