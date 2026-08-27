//! 静态语义层 (sema)。

mod decls;
mod exprs;
mod stmts;
pub(crate) mod types;

use crate::ast::{BinOp, Binding, Expr, Item, Program};
use crate::{AliasError, AliasResult, Span};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use types::int_literal_fits;
use types::{FloatW, IntW, Ty, UIntW};

#[derive(Clone)]
struct VarInfo {
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
    params: Vec<Ty>,
    ret: Ty,
    #[allow(dead_code)]
    public: bool,
    builtin: bool,
}

struct Scope {
    entries: RefCell<HashMap<String, VarInfo>>,
    parent: Option<Rc<Scope>>,
}

type Env = Rc<Scope>;

impl Scope {
    fn root() -> Env {
        Rc::new(Scope { entries: RefCell::new(HashMap::new()), parent: None })
    }

    fn child(parent: &Env) -> Env {
        Rc::new(Scope { entries: RefCell::new(HashMap::new()), parent: Some(parent.clone()) })
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
    main: Option<(Ty, Span)>,
    structs: HashMap<String, StructInfo>,
    methods: HashMap<String, HashMap<String, MethodInfo>>,
}

pub fn check(program: &Program) -> AliasResult<()> {
    let mut ck = Checker {
        fn_ret: Vec::new(),
        loop_depth: 0,
        main: None,
        structs: HashMap::new(),
        methods: builtin_methods(),
    };
    let top = Scope::root();
    for item in &program.items {
        match item {
            Item::Binding(b) => {
                if b.receiver.is_some() {
                    ck.method_def(b, &top)?;
                } else {
                    ck.bind(b, &top)?;
                }
            }
            Item::StructDef(sd) => ck.struct_def(sd, &top)?,
        }
    }
    ck.validate_main()
}

/// 编译器内建扩展函数。运算名字和符号运算共享同一静态类型规则；
/// 原生后端按接收者静态类型把它们发射到同一运算指令路径。
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
        strings.insert(name.to_string(), MethodInfo { params, ret, public: true, builtin: true });
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
                    params: vec![ty.clone()],
                    ret: ty.clone(),
                    public: true,
                    builtin: true,
                },
            );
        }
    }

    methods.entry("bool".into()).or_default().insert(
        "not".into(),
        MethodInfo { params: vec![], ret: Ty::Bool, public: true, builtin: true },
    );
    methods
}

fn op_mismatch(op: BinOp, l: &Ty, r: &Ty, span: Span) -> AliasError {
    AliasError {
        msg: format!("运算符 {} 不适用于 {} 与 {}", op.display(), l.name(), r.name()),
        span,
    }
}

fn decl_mismatch(b: &Binding, want: &Ty, got: &Ty) -> AliasError {
    AliasError {
        msg: format!("绑定 '{}' 声明类型为 {}, 实际 {}", b.name, want.name(), got.name()),
        span: b.span,
    }
}

pub(super) fn literal_slot_unify(declared: &Ty, value: &Expr) -> Option<AliasResult<Ty>> {
    let span = value.span();
    if let Expr::Float(..) = value {
        return if matches!(declared, Ty::Float(_)) {
            Some(Ok(declared.clone()))
        } else {
            None
        };
    }
    let v = match value {
        Expr::Int(n, _) => *n,
        Expr::Neg { expr, .. } => match expr.as_ref() {
            Expr::Int(n, _) => n.wrapping_neg(),
            _ => return None,
        },
        _ => return None,
    };
    if !matches!(declared, Ty::Int(_) | Ty::UInt(_)) {
        return None;
    }
    Some(if int_literal_fits(declared, v) {
        Ok(declared.clone())
    } else {
        Err(AliasError {
            msg: format!("字面量 {v} 超出 {} 的表示范围", declared.name()),
            span,
        })
    })
}
