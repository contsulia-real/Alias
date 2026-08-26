//! 静态语义层 (sema) — Phase 1 纯新增。
//!
//! 职责: 符号表作用域链 (遍历顺序镜像迁移前实现) + 内部类型推断 +
//! 全量检查。接线位置: run() 中 lex → parse → **sema** → execute。
//!
//! 铁律 (违者即宪法违规):
//! - 推断类型永不外泄到语法或诊断 — 用户只见到声明类型与运行时类型名 (D3)
//! - 迁移检查的中文消息逐字节保留, span 与运行时原报错一致 (D4)
//! - sema 通过后交给唯一原生编译管线；运行时检查由生成代码与 runtime 承担
//! - 单错误契约: 首个诊断即返回, 与迁移前 fail-fast 求值顺序一致
//!
//! 模块划分 (纯机械拆分, 无逻辑改动):
//! - [`types`]: Ty 内部类型枚举与一致性操作 (types_match / check_type_slot)
//! - [`decls`]: 顶层声明登记 (struct 表构建 / 方法签名注册 / main 校验)
//! - [`exprs`]: 表达式检查 (构造 / match 穷尽性 / ? 合法性 / 方法调用 /
//!   字段访问 / 二元运算 / 调用元数实参)
//! - [`stmts`]: 语句与块检查 (绑定 / 赋值 / 循环条件 / return 一致性 /
//!   Q③ 落空 / 函数体校验)

mod decls;
mod exprs;
mod stmts;
pub(crate) mod types;

use crate::ast::{BinOp, Binding, Expr, Item, Program};
use crate::{AliasError, AliasResult, Span};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use types::Ty;
use types::int_literal_fits;

#[derive(Clone)]
struct VarInfo {
    ty: Ty,
    mutable: bool,
}

/// 结构体字段元数据 (Phase 2a)。default 只记有无 — 默认表达式本体
/// 由 codegen 从 AST 自取, sema 不搬运表达式。
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

/// 方法签名 (Phase 2c)。params 不含 self — self 是隐式 val 绑定,
/// 类型 = 接收者类型, 不进参数表 (用户批准设计)。
#[derive(Clone)]
struct MethodInfo {
    params: Vec<Ty>,
    ret: Ty,
    /// public 标志: 单编译单元内恒可调 — 检查机制就位,
    /// import 阶段 (Phase 5+) 翻转为跨单元强制
    #[allow(dead_code)] // 设计裁决: 现阶段只存储不强制 (spec-notes 附录五)
    public: bool,
    builtin: bool,
}

/// 作用域链: 子优先、逐父上溯 — 与迁移前的查找顺序逐行镜像。
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
    /// 当前函数上下文栈: 各层声明返回类型。空栈 = 顶层 (return 非法)。
    fn_ret: Vec<Ty>,
    /// 最后一个 func main 候选 (名字 main + kind Func + 函数值初始化):
    /// (签名, 绑定 span)。后写覆盖 — 镜像迁移前的 last-wins。
    main: Option<(Ty, Span)>,
    /// 结构体表: 名字 → 有序字段。与绑定/func 共用单一命名空间
    /// (重名即编译错误); 按项序登记 — 声明前不可见。
    structs: HashMap<String, StructInfo>,
    /// 方法表 (Phase 2c): 接收者类型名 → (方法名 → 签名)。
    /// 按类型划分命名空间 — string.append 与 stat.append 共存;
    /// 内建项先行播种, 用户项按项序登记 (先登记后查体 — 方法可递归)。
    methods: HashMap<String, HashMap<String, MethodInfo>>,
}

/// 对整个 Program 做全量静态检查。通过后交给原生代码生成器。
pub fn check(program: &Program) -> AliasResult<()> {
    let mut ck = Checker {
        fn_ret: Vec::new(),
        main: None,
        structs: HashMap::new(),
        methods: builtin_methods(),
    };
    let top = Scope::root();
    // import 只解析不执行 (Phase 5 前) — sema 不校验 import 内容
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

/// 内建字符串方法 (Phase 2c): 编译器提供, 不可被用户方法覆盖。
/// len = 字节长; upper/lower 仅 ASCII 字母; trim 剥离首尾空格/\t/\r/\n。
fn builtin_methods() -> HashMap<String, HashMap<String, MethodInfo>> {
    let seed: [(&str, Vec<Ty>, Ty); 4] = [
        ("len", vec![], Ty::Int(types::IntW::W32)),
        ("upper", vec![], Ty::Str),
        ("lower", vec![], Ty::Str),
        ("trim", vec![], Ty::Str),
    ];
    let mut table = HashMap::new();
    for (name, params, ret) in seed {
        table.insert(
            name.to_string(),
            MethodInfo { params, ret, public: true, builtin: true },
        );
    }
    let mut methods = HashMap::new();
    methods.insert("string".to_string(), table);
    methods
}

// ---------- 诊断构造 ----------

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

/// 裸数值字面量与声明槽位的统一 (Phase 3a 裁决① 前置守卫):
/// - 整数字面量 (含一元负号前缀): 装入整型槽位时按 [`types::int_literal_fits`]
///   校验 — 通过则类型取声明槽位, 越界即编译错误;
/// - 浮点字面量装入浮点槽位恒可舍入 — 类型取声明槽位;
/// - 其余组合返回 None — 不做字面量多态, 跨族/跨宽赋值须经转换内建。
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
            // wrapping 取负镜像 codegen 的按宽 wrapping 语义
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
