//! sema::types — 内部类型系统。
//!
//! 拥有: [`Ty`] 推断类型枚举 (冻结类型集 + 结构体/result 投影)、
//! 一致性比较 [`types_match`]、类型槽校验 [`check_type_slot`]。
//! 本模块内容永不跨出 sema — 诊断只用 [`Ty::name`] 的运行时词汇表 (D3)。

use super::StructInfo;
use crate::ast::TypeExpr;
use crate::{AliasError, AliasResult, Span};
use std::collections::HashMap;

/// 内部类型。冻结类型集 {i32,bool,string,func,unit} + 结构体 (Phase 2a)
/// 的检查器投影 (D3)。本枚举永不跨出 sema — 诊断只用 [`Ty::name`]
/// 的运行时词汇表 (结构体显示其名)。
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Ty {
    Int,
    Bool,
    Str,
    Unit,
    /// 具名函数类型: 参数类型 + 返回类型
    Func { params: Vec<Ty>, ret: Box<Ty> },
    /// 声明为 `func` 的多态函数值 — 签名未知, 调用点不做元数/实参检查
    FuncPoly,
    /// 结构体实例 (引用语义: 值即泄漏堆块指针), 携带结构体名
    Struct(String),
    /// result<T,E> 内建泛型枚举 (Phase 2b): 值即泄漏堆块指针
    /// {tag, payload}; 构造器单侧推断时另一侧为 Unknown
    Result(Box<Ty>, Box<Ty>),
    /// array<T> 内建泛型 (Phase 2d): 值即泄漏堆块指针 (引用语义);
    /// 空字面量 [] 的元素类型为 Unknown, 由声明上下文统一
    Array(Box<Ty>),
    /// 已报错或签名不可知的子树 — 抑制级联诊断, 不再产生新消息
    Unknown,
}

impl Ty {
    /// 与迁移前 Value::type_name 逐字对齐 (Unit 显示为 "()");
    /// 结构体名即类型词汇 — 诊断按声明词汇表渲染 (D3)。
    pub(super) fn name(&self) -> String {
        match self {
            Ty::Int => "i32".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "string".into(),
            Ty::Func { .. } | Ty::FuncPoly => "func".into(),
            Ty::Unit => "()".into(),
            Ty::Struct(s) => s.clone(),
            Ty::Result(t, e) => format!("result<{}, {}>", t.name(), e.name()),
            Ty::Array(t) => format!("array<{}>", t.name()),
            Ty::Unknown => "未知".into(),
        }
    }

    pub(super) fn is_unknown(&self) -> bool {
        matches!(self, Ty::Unknown)
    }
}

/// 一致性比较: Unknown 恒兼容 (级联抑制); FuncPoly 接受任意函数形态;
/// result 结构递归 — 单侧推断的构造器结果 (一侧 Unknown) 与声明侧统一。
pub(super) fn types_match(want: &Ty, got: &Ty) -> bool {
    if want == got || want.is_unknown() || got.is_unknown() {
        return true;
    }
    match (want, got) {
        (Ty::Result(t1, e1), Ty::Result(t2, e2)) => {
            types_match(t1, t2) && types_match(e1, e2)
        }
        (Ty::Array(a), Ty::Array(b)) => types_match(a, b),
        _ => matches!(want, Ty::FuncPoly) && matches!(got, Ty::Func { .. } | Ty::FuncPoly),
    }
}

/// 类型槽校验: result<T,E> (恰两参) 与 array<T> (恰一参, Phase 2d)
/// 内建泛型展开, 递归校验; 其余泛型形状报命名 Phase 错误; 未知名拒绝
/// (parser 接受任意名字, 此处按 D3 冻结类型集 + 结构体表收紧)。
pub(super) fn check_type_slot(
    te: &TypeExpr,
    span: Span,
    structs: &HashMap<String, StructInfo>,
) -> AliasResult<Ty> {
    match te {
        TypeExpr::Generic(name, args) => {
            let want_arity = match name.as_str() {
                "result" => 2,
                "array" => 1,
                _ => {
                    return Err(AliasError {
                        msg: format!("泛型类型 {} 尚未实现 (Phase 5+)", te.display()),
                        span,
                    })
                }
            };
            if args.len() != want_arity {
                return Err(AliasError {
                    msg: format!("{name} 需要 {want_arity} 个类型参数, 实际 {} 个", args.len()),
                    span,
                });
            }
            let mut ts = Vec::with_capacity(args.len());
            for a in args {
                ts.push(check_type_slot(a, span, structs)?);
            }
            if name == "result" {
                Ok(Ty::Result(
                    Box::new(ts.swap_remove(0)),
                    Box::new(ts.swap_remove(0)),
                ))
            } else {
                Ok(Ty::Array(Box::new(ts.swap_remove(0))))
            }
        }
        TypeExpr::Named(n) => match n.as_str() {
            "i32" => Ok(Ty::Int),
            "bool" => Ok(Ty::Bool),
            "string" => Ok(Ty::Str),
            "unit" => Ok(Ty::Unit),
            "func" => Ok(Ty::FuncPoly),
            other => {
                if structs.contains_key(other) {
                    Ok(Ty::Struct(other.to_string()))
                } else {
                    Err(AliasError { msg: format!("未知类型名 '{other}'"), span })
                }
            }
        },
    }
}
