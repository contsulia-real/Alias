//! 静态语义层 (sema)。

mod decls;
mod exprs;
pub(crate) mod hir;
mod places;
mod stmts;
pub(crate) mod types;

use crate::ast::{BinOp, BindKind, Binding, Expr, Item, Program};
use crate::builtins::{classify_call_builtin, is_reserved_lexical_name, CallBuiltinName};
use crate::sema::hir::{BindingId, BuiltinCall, MethodId, MethodTarget, ResolvedConversion};
use crate::{AliasError, AliasResult, Span};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use types::{IntW, Ty};

/// 只在 sema 检查阶段存在；lowering 会把 target 固化到具体 HIR 节点并消除此状态。
pub(crate) struct LowerExprInfo {
    pub(crate) ty: Ty,
    pub(crate) call_target: Option<LowerCallTarget>,
    pub(crate) implicit_zero_callee: Option<Ty>,
}

/// 只允许存在于 sema 检查与 HIR lowering 之间；最终 HIR 按节点种类拆开调用目标。
#[derive(Clone, PartialEq)]
pub(crate) enum LowerCallTarget {
    FunctionValue,
    StructConstructor(String),
    ResultConstructor(crate::ast::CtorKind),
    Builtin(BuiltinCall),
    Borrow,
    Move,
    Typeof,
    ContextualConversion(ResolvedConversion),
    Method(MethodTarget),
}

#[derive(Clone)]
struct VarInfo {
    id: BindingId,
    ty: Ty,
    mutable: bool,
}

#[derive(Clone)]
struct FieldInfo {
    name: String,
    mutable: bool,
    ty: Ty,
    has_default: bool,
}

#[derive(Clone)]
pub(crate) struct StructInfo {
    fields: Vec<FieldInfo>,
}

#[derive(Clone)]
enum MethodInfo {
    Builtin {
        params: Vec<Ty>,
        ret: Ty,
        target: MethodTarget,
    },
    User {
        id: MethodId,
        params: Vec<Ty>,
        ret: Ty,
    },
}

impl MethodInfo {
    fn params(&self) -> &[Ty] {
        match self {
            Self::Builtin { params, .. } | Self::User { params, .. } => params,
        }
    }

    fn ret(&self) -> &Ty {
        match self {
            Self::Builtin { ret, .. } | Self::User { ret, .. } => ret,
        }
    }

    fn target(&self, receiver: &Ty) -> Option<MethodTarget> {
        match self {
            Self::Builtin { target, .. } => Some(target.clone()),
            Self::User { id, .. } => Some(MethodTarget::User {
                receiver: receiver.clone(),
                id: *id,
            }),
        }
    }
}

#[derive(Clone)]
struct BuiltinMethodSpec {
    name: &'static str,
    params: Vec<Ty>,
    ret: Ty,
    target: MethodTarget,
}

impl BuiltinMethodSpec {
    fn into_method_info(self) -> MethodInfo {
        MethodInfo::Builtin {
            params: self.params,
            ret: self.ret,
            target: self.target,
        }
    }
}

/// 可直接求值的内建调用名字到 HIR target 的唯一映射 owner。
fn resolved_builtin_call(name: &str) -> Option<BuiltinCall> {
    match classify_call_builtin(name) {
        Some(CallBuiltinName::Print) => Some(BuiltinCall::Print),
        Some(CallBuiltinName::Println) => Some(BuiltinCall::Println),
        Some(CallBuiltinName::Increase) => Some(BuiltinCall::Increase),
        Some(CallBuiltinName::Decrease) => Some(BuiltinCall::Decrease),
        Some(CallBuiltinName::From | CallBuiltinName::TryFrom | CallBuiltinName::Typeof) | None => {
            None
        }
    }
}

/// 内建方法的唯一静态契约 owner。
///
/// 名字解析和 resolved-HIR 最终校验都从这同一组 spec 查询；新增方法若只改调用解析
/// 而漏改验证器，或反过来，都会重新制造两个静态语义真相源。
fn builtin_method_specs(receiver: &Ty) -> Vec<BuiltinMethodSpec> {
    let spec = |name, params, ret, target| BuiltinMethodSpec {
        name,
        params,
        ret,
        target,
    };
    match receiver {
        ty if ty.is_numeric() => vec![
            spec(
                "plus",
                vec![ty.clone()],
                ty.clone(),
                MethodTarget::Numeric(BinOp::Add),
            ),
            spec(
                "minus",
                vec![ty.clone()],
                ty.clone(),
                MethodTarget::Numeric(BinOp::Sub),
            ),
            spec(
                "times",
                vec![ty.clone()],
                ty.clone(),
                MethodTarget::Numeric(BinOp::Mul),
            ),
            spec(
                "div",
                vec![ty.clone()],
                ty.clone(),
                MethodTarget::Numeric(BinOp::Div),
            ),
        ],
        Ty::Bool => vec![spec("not", vec![], Ty::Bool, MethodTarget::BoolNot)],
        Ty::Str => vec![
            spec("len", vec![], Ty::Int(IntW::W32), MethodTarget::StringLen),
            spec("upper", vec![], Ty::Str, MethodTarget::StringUpper),
            spec("lower", vec![], Ty::Str, MethodTarget::StringLower),
            spec("trim", vec![], Ty::Str, MethodTarget::StringTrim),
        ],
        Ty::Array(elem) => vec![
            spec("len", vec![], Ty::Int(IntW::W32), MethodTarget::ArrayLen),
            spec(
                "push",
                vec![(**elem).clone()],
                Ty::Unit,
                MethodTarget::ArrayPush,
            ),
            spec("pop", vec![], (**elem).clone(), MethodTarget::ArrayPop),
            spec(
                "iterator",
                vec![],
                Ty::Iterator(elem.clone()),
                MethodTarget::ArrayIterator,
            ),
        ],
        _ => Vec::new(),
    }
}

fn builtin_method(receiver: &Ty, name: &str) -> Option<MethodInfo> {
    builtin_method_specs(receiver)
        .into_iter()
        .find(|spec| spec.name == name)
        .map(BuiltinMethodSpec::into_method_info)
}

fn builtin_method_by_target(receiver: &Ty, target: &MethodTarget) -> Option<MethodInfo> {
    builtin_method_specs(receiver)
        .into_iter()
        .find(|spec| &spec.target == target)
        .map(BuiltinMethodSpec::into_method_info)
}

fn ensure_user_lexical_name(name: &str, span: Span) -> AliasResult<()> {
    if is_reserved_lexical_name(name) {
        Err(AliasError {
            msg: format!("预定义名字 '{name}' 不能用于用户声明"),
            span,
        })
    } else {
        Ok(())
    }
}

struct Scope {
    entries: RefCell<HashMap<String, VarInfo>>,
    parent: Option<Rc<Scope>>,
}

type Env = Rc<Scope>;

impl Scope {
    fn root() -> Env {
        Rc::new(Scope {
            entries: RefCell::new(HashMap::new()),
            parent: None,
        })
    }

    fn child(parent: &Env) -> Env {
        Rc::new(Scope {
            entries: RefCell::new(HashMap::new()),
            parent: Some(parent.clone()),
        })
    }

    fn get(env: &Env, name: &str) -> Option<VarInfo> {
        let mut cur = Some(env.clone());
        while let Some(scope) = cur {
            if let Some(info) = scope.entries.borrow().get(name) {
                return Some(info.clone());
            }
            cur = scope.parent.clone();
        }
        None
    }

    fn get_here(env: &Env, name: &str) -> Option<VarInfo> {
        env.entries.borrow().get(name).cloned()
    }

    fn insert(env: &Env, name: String, info: VarInfo) {
        env.entries.borrow_mut().insert(name, info);
    }
}

struct Checker {
    fn_ret: Vec<Ty>,
    /// 只记录当前函数体内的循环深度；进入新的函数字面量会临时清零。
    loop_depth: usize,
    main: Option<(BindingId, Ty, Span)>,
    structs: HashMap<String, StructInfo>,
    /// 这里只保存用户方法；内建方法统一来自 builtin_method_specs()。
    methods: HashMap<String, HashMap<String, MethodInfo>>,

    next_binding_id: u32,
    next_method_id: u32,
    next_loan_id: u32,

    expr_facts: HashMap<usize, LowerExprInfo>,
    binding_types: HashMap<usize, Ty>,
    binding_ids: HashMap<usize, BindingId>,
    receiver_types: HashMap<usize, Ty>,
    method_ids: HashMap<usize, MethodId>,
    method_self_ids: HashMap<usize, BindingId>,
    field_types: HashMap<usize, Ty>,
    field_indices: HashMap<usize, usize>,
    assignment_places: HashMap<usize, hir::LowerPlaceInfo>,
    borrow_places: HashMap<usize, hir::LowerBorrowInfo>,
    borrowed_bindings: HashMap<BindingId, bool>,
    move_places: HashMap<usize, hir::LowerPlaceInfo>,
    owning_reads: HashMap<usize, hir::LowerOwningReadInfo>,
    ctor_arg_indices: HashMap<usize, usize>,
    param_types: HashMap<usize, Ty>,
    param_ids: HashMap<usize, BindingId>,
    for_types: HashMap<usize, Ty>,
    for_ids: HashMap<usize, BindingId>,
    match_binding_ids: HashMap<usize, BindingId>,
    expr_binding_ids: HashMap<usize, BindingId>,
}

impl Checker {
    fn fresh_binding_id(&mut self) -> AliasResult<BindingId> {
        let id = BindingId(self.next_binding_id);
        self.next_binding_id = self
            .next_binding_id
            .checked_add(1)
            .ok_or_else(|| AliasError {
                msg: "内部 sema 不变式被破坏: BindingId 耗尽".into(),
                span: Span::default(),
            })?;
        Ok(id)
    }

    fn fresh_method_id(&mut self) -> AliasResult<MethodId> {
        let id = MethodId(self.next_method_id);
        self.next_method_id = self
            .next_method_id
            .checked_add(1)
            .ok_or_else(|| AliasError {
                msg: "内部 sema 不变式被破坏: MethodId 耗尽".into(),
                span: Span::default(),
            })?;
        Ok(id)
    }

    fn fresh_loan_id(&mut self) -> AliasResult<hir::LoanId> {
        let id = hir::LoanId(self.next_loan_id);
        self.next_loan_id = self.next_loan_id.checked_add(1).ok_or_else(|| AliasError {
            msg: "内部 sema 不变式被破坏: LoanId 耗尽".into(),
            span: Span::default(),
        })?;
        Ok(id)
    }

    fn binding_id_for(&mut self, binding: &Binding) -> AliasResult<BindingId> {
        // facts 的 identity 只在同一次 check(program) → hir::lower(program) 调用链内有效。
        // 这段期间 AST 仍由原 Program 持有且不得 move/clone-replace 节点；lower 会用同一
        // 地址消费 fact。若未来在两阶段之间加入 AST 重写，必须先改成稳定 NodeId。
        let key = binding as *const Binding as usize;
        if let Some(id) = self.binding_ids.get(&key) {
            return Ok(*id);
        }
        let id = self.fresh_binding_id()?;
        self.binding_ids.insert(key, id);
        Ok(id)
    }
}

pub(crate) fn check(program: Program) -> AliasResult<hir::CheckedProgram> {
    let mut ck = Checker {
        fn_ret: Vec::new(),
        loop_depth: 0,
        main: None,
        structs: HashMap::new(),
        methods: HashMap::new(),
        next_binding_id: 0,
        next_method_id: 0,
        next_loan_id: 0,
        expr_facts: HashMap::new(),
        binding_types: HashMap::new(),
        binding_ids: HashMap::new(),
        receiver_types: HashMap::new(),
        method_ids: HashMap::new(),
        method_self_ids: HashMap::new(),
        field_types: HashMap::new(),
        field_indices: HashMap::new(),
        assignment_places: HashMap::new(),
        borrow_places: HashMap::new(),
        borrowed_bindings: HashMap::new(),
        move_places: HashMap::new(),
        owning_reads: HashMap::new(),
        ctor_arg_indices: HashMap::new(),
        param_types: HashMap::new(),
        param_ids: HashMap::new(),
        for_types: HashMap::new(),
        for_ids: HashMap::new(),
        match_binding_ids: HashMap::new(),
        expr_binding_ids: HashMap::new(),
    };
    let top = Scope::root();
    for item in &program.items {
        match item {
            Item::Binding(b) => {
                if b.receiver.is_some() {
                    ck.method_def(b, &top)?;
                } else {
                    ensure_user_lexical_name(&b.name, b.span)?;
                    if b.kind == BindKind::Func {
                        if let Expr::FuncLit { params, .. } = &b.value {
                            if Scope::get_here(&top, &b.name).is_some() {
                                return Err(AliasError {
                                    msg: format!("同一顶层作用域不能重复声明绑定 '{}'", b.name),
                                    span: b.span,
                                });
                            }
                            let ret = types::check_return_type_slot(&b.ty, b.span, &ck.structs)?;
                            let mut ptys = Vec::with_capacity(params.len());
                            for p in params {
                                ensure_user_lexical_name(&p.name, p.span)?;
                                ptys.push(types::check_value_type_slot(
                                    &p.ty,
                                    p.span,
                                    &ck.structs,
                                )?);
                            }
                            let id = ck.binding_id_for(b)?;
                            Scope::insert(
                                &top,
                                b.name.clone(),
                                VarInfo {
                                    id,
                                    ty: Ty::Func {
                                        params: ptys,
                                        ret: Box::new(ret),
                                    },
                                    mutable: false,
                                },
                            );
                        }
                    }
                    ck.bind(b, &top)?;
                }
            }
            Item::StructDef(sd) => ck.struct_def(sd, &top)?,
        }
    }
    let main_id = ck.validate_main()?;
    hir::lower(
        program,
        hir::LowerFacts {
            next_loan_id: ck.next_loan_id,
            exprs: ck.expr_facts,
            bindings: ck.binding_types,
            binding_ids: ck.binding_ids,
            receivers: ck.receiver_types,
            method_ids: ck.method_ids,
            method_self_ids: ck.method_self_ids,
            fields: ck.field_types,
            field_indices: ck.field_indices,
            assignment_places: ck.assignment_places,
            borrow_places: ck.borrow_places,
            move_places: ck.move_places,
            owning_reads: ck.owning_reads,
            params: ck.param_types,
            param_ids: ck.param_ids,
            fors: ck.for_types,
            for_ids: ck.for_ids,
            match_binding_ids: ck.match_binding_ids,
            expr_binding_ids: ck.expr_binding_ids,
            ctor_arg_indices: ck.ctor_arg_indices,
        },
        main_id,
    )
}

fn op_mismatch(op: BinOp, l: &Ty, r: &Ty, span: Span) -> AliasError {
    AliasError {
        msg: format!(
            "运算符 {} 不适用于 {} 与 {}",
            op.display(),
            l.name(),
            r.name()
        ),
        span,
    }
}

fn decl_mismatch(b: &Binding, want: &Ty, got: &Ty) -> AliasError {
    AliasError {
        msg: format!(
            "绑定 '{}' 声明类型为 {}, 实际 {}",
            b.name,
            want.name(),
            got.name()
        ),
        span: b.span,
    }
}
