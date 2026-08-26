//! sema::types — 内部类型系统 (Phase 3a 全量数值类型)。
//!
//! 拥有: [`Ty`] 推断类型枚举 (全量类型集 + 结构体/result/array 投影)、
//! 一致性比较 [`types_match`]、类型槽校验 [`check_type_slot`]。
//! 本模块内容永不跨出 sema — 诊断只用 [`Ty::name`] 的运行时词汇表 (D3)。

use super::StructInfo;
use crate::ast::TypeExpr;
use crate::{AliasError, AliasResult, Span};
use std::collections::HashMap;

/// 无符号整数宽度 (符号性由 Ty::UInt 承载; 与 IntW 同宽集)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UIntW {
    U8,
    U16,
    U32,
    U64,
}

impl UIntW {
    pub(crate) fn bits(self) -> u32 {
        match self {
            UIntW::U8 => 8,
            UIntW::U16 => 16,
            UIntW::U32 => 32,
            UIntW::U64 => 64,
        }
    }
}

/// 整数宽度 (符号性由 Ty::Int/Ty::UInt 承载)。
/// 宽度词汇表为全编译器共享 (codegen VTy 镜像引用) — Ty 本体永不跨出 sema。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntW {
    W8,
    W16,
    W32,
    W64,
}

impl IntW {
    pub(crate) fn bits(self) -> u32 {
        match self {
            IntW::W8 => 8,
            IntW::W16 => 16,
            IntW::W32 => 32,
            IntW::W64 => 64,
        }
    }
}

/// 浮点宽度
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FloatW {
    F32,
    F64,
}

impl FloatW {
    pub(crate) fn name(self) -> &'static str {
        match self {
            FloatW::F32 => "f32",
            FloatW::F64 => "f64",
        }
    }

    // 所有权律: cranelift 类型映射仅 codegen 子系统拥有 (codegen/mod.rs
    // 的 cl_type 单一映射点) — 此处不放任何 cranelift 类型。
}

/// 内部类型 (Phase 3a 全量数值集): 有符号/无符号整数按宽度区分;
/// 浮点 f32/f64。本枚举永不跨出 sema — 诊断只用 [`Ty::name`]。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ty {
    /// 有符号整数 — 运算按宽度 wrapping, 存储规范化到声明宽度
    Int(IntW),
    /// 无符号整数 — 比较与除法用无符号谓词, 显示无负号
    UInt(UIntW),
    /// 浮点 — 双通道原生值 (F32/F64 cranelift 类型), 禁止与整型混算
    Float(FloatW),
    Bool,
    Str,
    Unit,
    /// 具名函数类型: 参数类型 + 返回类型
    Func { params: Vec<Ty>, ret: Box<Ty> },
    /// 声明为 `func` 的多态函数值 — 签名未知, 调用点不做元数/实参检查
    FuncPoly,
    /// 结构体实例 (引用语义: 值即泄漏堆块指针), 携带结构体名
    Struct(String),
    /// result<T,E> 内建泛型枚举 (Phase 2b)
    Result(Box<Ty>, Box<Ty>),
    /// array<T> 内建泛型 (Phase 2d)
    Array(Box<Ty>),
    /// 已报错或签名不可知的子树 — 抑制级联诊断, 不再产生新消息
    Unknown,
}

impl Ty {
    /// 运行时词汇表 (D3); typeof 内建同名输出
    pub(crate) fn name(&self) -> String {
        match self {
            Ty::Int(w) => match w {
                IntW::W8 => "i8".into(),
                IntW::W16 => "i16".into(),
                IntW::W32 => "i32".into(),
                IntW::W64 => "i64".into(),
            },
            Ty::UInt(w) => match w {
                UIntW::U8 => "u8".into(),
                UIntW::U16 => "u16".into(),
                UIntW::U32 => "u32".into(),
                UIntW::U64 => "u64".into(),
            },
            Ty::Float(w) => w.name().into(),
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

    pub(crate) fn is_unknown(&self) -> bool {
        matches!(self, Ty::Unknown)
    }

    /// 数值族判定 (转换内建与混算诊断共用)
    pub(crate) fn is_numeric(&self) -> bool {
        matches!(
            self,
            Ty::Int(_) | Ty::UInt(_) | Ty::Float(_)
        )
    }
}

/// 一致性比较: Unknown 恒兼容 (级联抑制); FuncPoly 接受任意函数形态;
/// result/array 结构递归 — 单侧推断的构造器结果与声明侧统一。
/// 数值类型严格同名匹配 (禁止隐式混算/跨宽度 — 用户裁决③④)。
pub(crate) fn types_match(want: &Ty, got: &Ty) -> bool {
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

/// 数值字面量的编译期范围校验: Int 字面量 (i64 承载) 装入声明槽位时
/// 越界即编译错误 (存储规范化裁决①的前置守卫); Float 字面量恒可舍入。
pub(crate) fn int_literal_fits(ty: &Ty, v: i64) -> bool {
    match ty {
        Ty::Int(w) => match w {
            IntW::W8 => v >= i8::MIN as i64 && v <= i8::MAX as i64,
            IntW::W16 => v >= i16::MIN as i64 && v <= i16::MAX as i64,
            IntW::W32 => v >= i32::MIN as i64 && v <= i32::MAX as i64,
            IntW::W64 => true,
        },
        Ty::UInt(w) => match w {
            UIntW::U8 => v >= 0 && v <= u8::MAX as i64,
            UIntW::U16 => v >= 0 && v <= u16::MAX as i64,
            UIntW::U32 => v >= 0 && v <= u32::MAX as i64,
            UIntW::U64 => v >= 0,
        },
        _ => true,
    }
}

/// 类型槽校验: result<T,E> (恰两参) 与 array<T> (恰一参) 内建泛型展开,
/// 递归校验; 其余泛型形状报命名 Phase 错误; 未知名拒绝 (含 float/double —
/// 仅收 fX 命名, 用户裁决④)。
pub(crate) fn check_type_slot(
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
            "i8" => Ok(Ty::Int(IntW::W8)),
            "i16" => Ok(Ty::Int(IntW::W16)),
            "i32" => Ok(Ty::Int(IntW::W32)),
            "i64" => Ok(Ty::Int(IntW::W64)),
            "u8" => Ok(Ty::UInt(UIntW::U8)),
            "u16" => Ok(Ty::UInt(UIntW::U16)),
            "u32" => Ok(Ty::UInt(UIntW::U32)),
            "u64" => Ok(Ty::UInt(UIntW::U64)),
            "f32" => Ok(Ty::Float(FloatW::F32)),
            "f64" => Ok(Ty::Float(FloatW::F64)),
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
