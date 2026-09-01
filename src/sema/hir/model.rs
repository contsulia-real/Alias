//! typed HIR data model. No name resolution or backend inference belongs here.

pub use crate::ast::{BinOp, BindKind, CtorKind, Pattern};
pub(crate) use crate::sema::types::BindingId;
use crate::sema::types::{ParamEffect, ReturnBorrowSource, ReturnEffect, Ty};
use crate::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct MethodId(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct LoanId(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FunctionId(pub(crate) u32);

#[derive(Debug, Clone)]
pub(crate) struct CheckedProgram {
    pub(crate) main_id: BindingId,
    pub(crate) items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub(crate) enum Item {
    Binding(Box<Binding>),
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
    pub(crate) relation: Option<StorageRelation>,
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
    pub(crate) effect: Option<ParamEffect>,
}

/// 闭包环境保存 binding cell，但捕获对 referent 的静态权限必须独立固化为普通 loan。
/// 若只保留 BindingId，ownership flow 就只能把 capture 当成永久 alias exposure，无法按
/// closure value 的最后一次使用结束 NLL region。
#[derive(Debug, Clone)]
pub(crate) struct Capture {
    pub(crate) binding_id: BindingId,
    pub(crate) loan_id: LoanId,
    pub(crate) source: Place,
    pub(crate) kind: Option<BorrowKind>,
}

/// sema 已解析的 Place projection。Place 只表达 storage identity/projection；读取、写入、
/// borrow、move 等外层操作另行决定语义。Field/Index 的 base 必须继续是 Place，不能退回
/// 任意 Expr，否则 overlap/loan 会再次依赖源码形状或运行时地址猜测。
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
        base: Box<Place>,
        field_index: usize,
        info: PlaceInfo,
    },
    Index {
        base: Box<Place>,
        index: Box<Expr>,
        info: PlaceInfo,
    },
}

impl Place {
    pub(crate) fn info(&self) -> &PlaceInfo {
        match self {
            Self::Local { info, .. } | Self::Field { info, .. } | Self::Index { info, .. } => info,
        }
    }

    pub(crate) fn ty(&self) -> &Ty {
        &self.info().ty
    }

    pub(crate) fn span(&self) -> Span {
        self.info().span
    }

    pub(crate) fn root_binding_id(&self) -> BindingId {
        let mut place = self;
        loop {
            match place {
                Self::Local { binding_id, .. } => return *binding_id,
                Self::Field { base, .. } | Self::Index { base, .. } => place = base,
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Stmt {
    Binding(Binding),
    Assign {
        target: Place,
        value: Expr,
        operation: Option<AssignmentOperation>,
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
        element_plan: DeepClonePlan,
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
    pub(crate) pass: Option<ArgumentPass>,
}

#[derive(Debug, Clone)]
pub(crate) enum CallResult {
    Inline,
    Owned,
    Borrowed {
        loan_id: LoanId,
        source: Place,
        source_writable: bool,
        kind: Option<BorrowKind>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum ReturnPass {
    Inline,
    OwnedValue,
    OwnedTransfer {
        source: Place,
    },
    BorrowPlace {
        source: Place,
        origin: ReturnBorrowSource,
    },
    BorrowValue {
        origin: ReturnBorrowSource,
    },
}

/// Caller-side execution contract resolved from the callee's frozen parameter effect. Borrow
/// variants carry the canonical source Place and loan generation; codegen only evaluates `value`
/// because the pass contract is a static ownership fact, not a second runtime calling convention.
#[derive(Debug, Clone)]
pub(crate) enum ArgumentPass {
    Inline,
    ReadBorrow { loan_id: LoanId, source: Place },
    WriteBorrow { loan_id: LoanId, source: Place },
    BorrowTemporary { kind: BorrowKind },
    Owned,
}

/// sema 已经裁决好的 deep-clone 执行计划。codegen 只能逐层执行该 plan，不能根据
/// `Ty`/VTy 再判断一个类型是否 DeepCloneable，也不能把 aggregate 退化成引用复制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeepClonePlan {
    Inline,
    String,
    Struct {
        name: String,
        fields: Vec<DeepClonePlan>,
    },
    Array(Box<DeepClonePlan>),
    Result {
        ok: Box<DeepClonePlan>,
        err: Box<DeepClonePlan>,
    },
}

/// sema 已经裁决好的 shallow-clone 执行计划。`Inline` 只作为递归安全叶存在；
/// user-level shallow 根必须是 Struct/Result，从而确实产生一个新的独立 aggregate owner。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShallowClonePlan {
    Inline,
    Struct {
        name: String,
        fields: Vec<ShallowClonePlan>,
    },
    Result {
        ok: Box<ShallowClonePlan>,
        err: Box<ShallowClonePlan>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuiltinCall {
    Print,
    Println,
    Increase,
    Decrease,
    DeepClone(DeepClonePlan),
    ShallowClone(ShallowClonePlan),
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
    User {
        receiver: Ty,
        id: MethodId,
        param_effects: Option<Vec<ParamEffect>>,
        return_effect: Option<ReturnEffect>,
    },
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

/// 已经产生 Value 的表达式继续记录 ownership-sensitive 细分类。
///
/// `InlineValue` 是不携带独立动态 ownership 的普通标量值。`General` 只保留当前阶段
/// 仍需后续 ownership/effect 语义才能继续细分的 Value；codegen 不得把它当 fallback。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValueCategory {
    InlineValue,
    General,
    OwnedTemporary,
    BorrowedValue,
}

/// Value 产生时可固化的 initial compile-time ownership capability。
///
/// 显式 Move 之后的程序点 `Moved` 状态由 `hir/ownership_flow.rs` 的 CFG dataflow 表达，
/// 不伪装成每个 Expr 自带的 initial capability。后续 Consumed 操作真正进入 HIR 时也必须
/// 进入同一个程序点分析 owner，而不是提前增加无消费者的枚举占位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnershipCapability {
    None,
    Available,
}

/// sema 已证明的 binding slot relation。Owning cell 保存值；Borrowed cell 保存 referent
/// address。两者物理解释不同，codegen 必须消费该事实，不能从 value bit pattern 反推 relation。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StorageRelation {
    Owning,
    Borrowed,
}

/// An owning write either copies an inline value or consumes an already prepared owner. Keeping
/// this semantic distinction in HIR is required even though both currently become a machine store:
/// replacement destruction and raw initialization must never reconstruct transfer responsibility
/// from the physical value representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwningWrite {
    InlineCopy,
    OwnershipTransfer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AssignmentOperation {
    Replace(OwningWrite),
    RebindBorrowedAlias,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BorrowKind {
    Read,
    Write,
}

/// 表达式在完成 sema 后首先区分“一个可继续投影/读取的存储 Place”与“已经产生的 Value”。
/// Value 内部再由 `ValueCategory` 固化 InlineValue/OwnedTemporary/BorrowedValue 等当前已落地
/// 语义；尚未实现的 null/effect 语义不得由 codegen 猜测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExprCategory {
    Place,
    Value(ValueCategory),
}

#[derive(Debug, Clone)]
pub(crate) struct ExprInfo {
    /// sema 已完成全部目标类型传播后的最终静态类型。backend 只能消费该结果，
    /// 不得再次根据运算符、上下文或源码名字决定子表达式应采用什么类型。
    pub(crate) ty: Ty,
    /// 节点构造期间暂为 None；lower_expr 在返回这个 resolved HIR 节点前立即写回 category。
    /// final-HIR gate 保证进入 codegen 前必为 Some 且与节点语义形状一致。
    pub(crate) category: Option<ExprCategory>,
    /// 只保存当前 phase 已经证明的 capability：InlineValue=None，OwnedTemporary=Available。
    /// `Option::None` 表示该 Expr 当前没有可固化的 capability fact（例如 Place/General），
    /// 不是 `OwnershipCapability::None` 的别名，也不得被后端当作 fallback。
    pub(crate) ownership_capability: Option<OwnershipCapability>,
    /// 只允许直接处于语言 return 位置的表达式携带；function-effect finalization 写回，
    /// final gate 独立复算。这里和 call result 都必须保持 boxed：两者含完整 Place，若
    /// 内嵌会膨胀每个 Expr，并让合法深度边界在 HIR 析构/发射时耗尽编译线程栈。
    pub(crate) return_pass: Option<Box<ReturnPass>>,
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
        result: Option<Box<CallResult>>,
        target: CallTarget,
        span: Span,
        info: ExprInfo,
    },
    MethodCall {
        recv: Box<Expr>,
        receiver_pass: Option<ArgumentPass>,
        args: Vec<CallArg>,
        result: Option<Box<CallResult>>,
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
        function_id: FunctionId,
        params: Vec<Param>,
        implicit_bindings: Vec<BindingId>,
        captures: Vec<Capture>,
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
    /// Ordinary stable-Place read into a semantically owning storage context. The recursive plan
    /// is frozen by sema; backend execution must not infer cloneability from the physical value.
    ReadPlace {
        source: Box<Place>,
        plan: DeepClonePlan,
        span: Span,
        info: ExprInfo,
    },
    /// Non-owning alias to a resolved Place. `kind` is filled by the loan analysis from actual
    /// uses; codegen may consume the source address only after the final HIR gate proves it.
    Borrow {
        loan_id: LoanId,
        source: Box<Place>,
        kind: Option<BorrowKind>,
        span: Span,
        info: ExprInfo,
    },
    /// Explicit ownership transfer from a sema-resolved Place. Keeping the Place as payload makes
    /// source identity and later overlap checks independent of the original AST expression shape.
    Move {
        source: Box<Place>,
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
            | Self::Propagate { info, .. }
            | Self::ReadPlace { info, .. }
            | Self::Borrow { info, .. }
            | Self::Move { info, .. } => info,
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
            | Self::Propagate { info, .. }
            | Self::ReadPlace { info, .. }
            | Self::Borrow { info, .. }
            | Self::Move { info, .. } => info,
        }
    }

    pub(crate) fn ty(&self) -> &Ty {
        &self.info().ty
    }

    pub(crate) fn category(&self) -> Option<ExprCategory> {
        self.info().category
    }

    pub(crate) fn value_category(&self) -> Option<ValueCategory> {
        match self.category()? {
            ExprCategory::Place => None,
            ExprCategory::Value(category) => Some(category),
        }
    }

    pub(crate) fn ownership_capability(&self) -> Option<OwnershipCapability> {
        self.info().ownership_capability
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
            | Self::Propagate { span, .. }
            | Self::ReadPlace { span, .. }
            | Self::Borrow { span, .. }
            | Self::Move { span, .. } => *span,
        }
    }
}
