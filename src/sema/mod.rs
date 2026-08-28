//! 静态语义层 (sema)。

mod decls;
mod exprs;
pub(crate) mod hir;
mod stmts;
pub(crate) mod types;

use crate::ast::{BinOp, BindKind, Binding, Expr, Item, Program};
use crate::builtins::is_reserved_lexical_name;
use crate::sema::hir::{
    BindingId, BuiltinCall, MethodId, MethodTarget, ResolvedConversion,
};
use crate::{AliasError, AliasResult, Span};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use types::{FloatW, IntW, Ty, UIntW};

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

fn builtin_method(receiver: &Ty, name: &str) -> Option<MethodInfo> {
    let (params, ret, target) = match receiver {
        ty if ty.is_numeric() => {
            let op = match name {
                "plus" => BinOp::Add,
                "minus" => BinOp::Sub,
                "times" => BinOp::Mul,
                "div" => BinOp::Div,
                _ => return None,
            };
            (vec![ty.clone()], ty.clone(), MethodTarget::Numeric(op))
        }
        Ty::Bool if name == "not" => (vec![], Ty::Bool, MethodTarget::BoolNot),
        Ty::Str => match name {
            "len" => (vec![], Ty::Int(IntW::W32), MethodTarget::StringLen),
            "upper" => (vec![], Ty::Str, MethodTarget::StringUpper),
            "lower" => (vec![], Ty::Str, MethodTarget::StringLower),
            "trim" => (vec![], Ty::Str, MethodTarget::StringTrim),
            _ => return None,
        },
        Ty::Array(elem) => match name {
            "len" => (vec![], Ty::Int(IntW::W32), MethodTarget::ArrayLen),
            "push" => (vec![(**elem).clone()], Ty::Unit, MethodTarget::ArrayPush),
            "pop" => (vec![], (**elem).clone(), MethodTarget::ArrayPop),
            "iterator" => (
                vec![],
                Ty::Iterator(elem.clone()),
                MethodTarget::ArrayIterator,
            ),
            _ => return None,
        },
        _ => return None,
    };
    Some(MethodInfo::Builtin {
        params,
        ret,
        target,
    })
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
    /// 这里只保存用户方法。内建方法由 builtin_method() 这一 owner 按接收者类型解析。
    methods: HashMap<String, HashMap<String, MethodInfo>>,

    next_binding_id: u32,
    next_method_id: u32,

    expr_facts: HashMap<usize, LowerExprInfo>,
    binding_types: HashMap<usize, Ty>,
    binding_ids: HashMap<usize, BindingId>,
    receiver_types: HashMap<usize, Ty>,
    method_ids: HashMap<usize, MethodId>,
    method_self_ids: HashMap<usize, BindingId>,
    field_types: HashMap<usize, Ty>,
    field_indices: HashMap<usize, usize>,
    field_assign_indices: HashMap<usize, usize>,
    ctor_arg_indices: HashMap<usize, usize>,
    param_types: HashMap<usize, Ty>,
    param_ids: HashMap<usize, BindingId>,
    for_types: HashMap<usize, Ty>,
    for_ids: HashMap<usize, BindingId>,
    assign_target_ids: HashMap<usize, BindingId>,
    match_binding_ids: HashMap<usize, BindingId>,
    expr_binding_ids: HashMap<usize, BindingId>,
}

impl Checker {
    fn fresh_binding_id(&mut self) -> BindingId {
        let id = BindingId(self.next_binding_id);
        self.next_binding_id = self
            .next_binding_id
            .checked_add(1)
            .unwrap_or_else(|| panic!("内部 sema 不变式被破坏: BindingId 耗尽"));
        id
    }

    fn fresh_method_id(&mut self) -> MethodId {
        let id = MethodId(self.next_method_id);
        self.next_method_id = self
            .next_method_id
            .checked_add(1)
            .unwrap_or_else(|| panic!("内部 sema 不变式被破坏: MethodId 耗尽"));
        id
    }

    fn binding_id_for(&mut self, binding: &Binding) -> BindingId {
        // facts 的 identity 只在同一次 check(program) → hir::lower(program) 调用链内有效。
        // 这段期间 AST 仍由原 Program 持有且不得 move/clone-replace 节点；lower 会用同一
        // 地址消费 fact。若未来在两阶段之间加入 AST 重写，必须先改成稳定 NodeId。
        let key = binding as *const Binding as usize;
        if let Some(id) = self.binding_ids.get(&key) {
            return *id;
        }
        let id = self.fresh_binding_id();
        self.binding_ids.insert(key, id);
        id
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
        expr_facts: HashMap::new(),
        binding_types: HashMap::new(),
        binding_ids: HashMap::new(),
        receiver_types: HashMap::new(),
        method_ids: HashMap::new(),
        method_self_ids: HashMap::new(),
        field_types: HashMap::new(),
        field_indices: HashMap::new(),
        field_assign_indices: HashMap::new(),
        ctor_arg_indices: HashMap::new(),
        param_types: HashMap::new(),
        param_ids: HashMap::new(),
        for_types: HashMap::new(),
        for_ids: HashMap::new(),
        assign_target_ids: HashMap::new(),
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
                            let id = ck.binding_id_for(b);
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
            exprs: ck.expr_facts,
            bindings: ck.binding_types,
            binding_ids: ck.binding_ids,
            receivers: ck.receiver_types,
            method_ids: ck.method_ids,
            method_self_ids: ck.method_self_ids,
            fields: ck.field_types,
            field_indices: ck.field_indices,
            field_assign_indices: ck.field_assign_indices,
            params: ck.param_types,
            param_ids: ck.param_ids,
            fors: ck.for_types,
            for_ids: ck.for_ids,
            assign_target_ids: ck.assign_target_ids,
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
