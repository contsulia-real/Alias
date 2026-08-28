//! 静态语义层 (sema)。

mod decls;
mod exprs;
pub(crate) mod hir;
mod stmts;
pub(crate) mod types;

use crate::ast::{BinOp, BindKind, Binding, Expr, Item, Program};
use crate::sema::hir::{BindingId, ExprInfo, MethodId};
use crate::{AliasError, AliasResult, Span};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use types::{FloatW, IntW, Ty, UIntW};

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
struct MethodInfo {
    /// 只有用户方法携带稳定 MethodId；内建方法由专用 MethodTarget 表示。
    id: Option<MethodId>,
    params: Vec<Ty>,
    ret: Ty,
    #[allow(dead_code)]
    is_pub: bool,
    builtin: bool,
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
    methods: HashMap<String, HashMap<String, MethodInfo>>,

    next_binding_id: u32,
    next_method_id: u32,

    expr_facts: HashMap<usize, ExprInfo>,
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
        methods: builtin_methods(),
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
                    if b.kind == BindKind::Func {
                        if let Expr::FuncLit { params, .. } = &b.value {
                            let ret = types::check_return_type_slot(&b.ty, b.span, &ck.structs)?;
                            let mut ptys = Vec::with_capacity(params.len());
                            for p in params {
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
    let _main_id = ck.validate_main()?;
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
        },
    )
}

fn builtin_methods() -> HashMap<String, HashMap<String, MethodInfo>> {
    let mut methods: HashMap<String, HashMap<String, MethodInfo>> = HashMap::new();

    let string_seed: [(&str, Vec<Ty>, Ty); 4] = [
        ("len", vec![], Ty::Int(IntW::W32)),
        ("upper", vec![], Ty::Str),
        ("lower", vec![], Ty::Str),
        ("trim", vec![], Ty::Str),
    ];
    let mut strings = HashMap::new();
    for (name, params, ret) in string_seed {
        strings.insert(
            name.to_string(),
            MethodInfo {
                id: None,
                params,
                ret,
                is_pub: true,
                builtin: true,
            },
        );
    }
    methods.insert("string".to_string(), strings);

    let numerics = vec![
        Ty::Int(IntW::W8),
        Ty::Int(IntW::W16),
        Ty::Int(IntW::W32),
        Ty::Int(IntW::W64),
        Ty::UInt(UIntW::U8),
        Ty::UInt(UIntW::U16),
        Ty::UInt(UIntW::U32),
        Ty::UInt(UIntW::U64),
        Ty::Float(FloatW::F32),
        Ty::Float(FloatW::F64),
    ];
    for ty in numerics {
        let table = methods.entry(ty.name()).or_default();
        for name in ["plus", "minus", "times", "div"] {
            table.insert(
                name.to_string(),
                MethodInfo {
                    id: None,
                    params: vec![ty.clone()],
                    ret: ty.clone(),
                    is_pub: true,
                    builtin: true,
                },
            );
        }
    }

    methods.entry("bool".into()).or_default().insert(
        "not".into(),
        MethodInfo {
            id: None,
            params: vec![],
            ret: Ty::Bool,
            is_pub: true,
            builtin: true,
        },
    );
    methods
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
