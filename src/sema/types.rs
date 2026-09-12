//! sema::types — Alias 内部静态类型系统。

use super::StructInfo;
use crate::ast::TypeExpr;
use crate::{AliasError, AliasResult, Span};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct BindingId(pub(crate) u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ParamEffect {
    ReadBorrow,
    WriteBorrow,
    Owned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ReturnBorrowSource {
    Parameter(usize),
    SelfValue,
    Global(BindingId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ReturnEffect {
    Inline,
    Owned,
    /// The returned state is owned, but retains one source loan. This must remain distinct from
    /// Borrowed: the machine return carries the value, not the address of another value's cell.
    OwnedBorrowing(ReturnBorrowSource),
    Borrowed(ReturnBorrowSource),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Ty {
    Int(IntW),
    UInt(UIntW),
    Float(FloatW),
    Bool,
    Str,
    Unit,
    Func {
        params: Vec<Ty>,
        /// check 阶段尚未拥有函数体 fixed-point 事实，因此为 None；parameter-effect
        /// finalization 必须在 final HIR 前写回完整、与 params 等长的向量。
        param_effects: Option<Vec<ParamEffect>>,
        /// 与 parameter effects 同属完整 semantic signature；check 阶段尚未拥有函数体
        /// return-source facts，final HIR 前必须写回唯一的 resolved effect。
        return_effect: Option<ReturnEffect>,
        ret: Box<Ty>,
    },
    FuncPoly,
    Struct(String),
    Result(Box<Ty>, Box<Ty>),
    Array(Box<Ty>),
    /// iterator<T> 是真实语言/runtime 类型；数组 iterator 携带结构版本号并在消费时
    /// fail-fast 检查失效，当前 `for` 也可直接消费 iterator 值。
    Iterator(Box<Ty>),
    /// Pointer nullability is a static fact only. Ownership relation remains a separate HIR
    /// contract, and both forms project to the same four-lane capability ABI.
    Ptr {
        pointee: Box<Ty>,
        nullable: bool,
    },
    Unknown,
}

impl Ty {
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
            Ty::Unit => "unit".into(),
            Ty::Struct(s) => s.clone(),
            Ty::Result(t, e) => format!("result<{}, {}>", t.name(), e.name()),
            Ty::Array(t) => format!("array<{}>", t.name()),
            Ty::Iterator(t) => format!("iterator<{}>", t.name()),
            Ty::Ptr { pointee, nullable } => {
                format!("ptr<{}>{}", pointee.name(), if *nullable { "?" } else { "" })
            }
            Ty::Unknown => "未知".into(),
        }
    }

    pub(crate) fn is_unknown(&self) -> bool {
        matches!(self, Ty::Unknown)
    }

    pub(crate) fn contains_unknown(&self) -> bool {
        match self {
            Ty::Unknown => true,
            Ty::Func { params, ret, .. } => {
                params.iter().any(Ty::contains_unknown) || ret.contains_unknown()
            }
            Ty::Result(ok, err) => ok.contains_unknown() || err.contains_unknown(),
            Ty::Array(elem) | Ty::Iterator(elem) | Ty::Ptr { pointee: elem, .. } => {
                elem.contains_unknown()
            }
            _ => false,
        }
    }

    pub(crate) fn contains_unit(&self) -> bool {
        match self {
            Ty::Unit => true,
            Ty::Func { params, ret, .. } => {
                params.iter().any(Ty::contains_unit) || ret.contains_unit()
            }
            Ty::Result(ok, err) => ok.contains_unit() || err.contains_unit(),
            Ty::Array(elem) | Ty::Iterator(elem) | Ty::Ptr { pointee: elem, .. } => {
                elem.contains_unit()
            }
            _ => false,
        }
    }

    pub(crate) fn is_numeric(&self) -> bool {
        matches!(self, Ty::Int(_) | Ty::UInt(_) | Ty::Float(_))
    }
}

pub(crate) fn types_match(want: &Ty, got: &Ty) -> bool {
    if want == got || want.is_unknown() || got.is_unknown() {
        return true;
    }
    match (want, got) {
        (Ty::Result(t1, e1), Ty::Result(t2, e2)) => types_match(t1, t2) && types_match(e1, e2),
        (Ty::Array(a), Ty::Array(b)) | (Ty::Iterator(a), Ty::Iterator(b)) => types_match(a, b),
        (
            Ty::Ptr {
                pointee: left,
                nullable: left_nullable,
            },
            Ty::Ptr {
                pointee: right,
                nullable: right_nullable,
            },
        ) => left_nullable == right_nullable && types_match(left, right),
        _ => matches!(want, Ty::FuncPoly) && matches!(got, Ty::Func { .. } | Ty::FuncPoly),
    }
}

pub(crate) fn int_literal_fits(ty: &Ty, magnitude: u64, negative: bool) -> bool {
    match ty {
        Ty::Int(w) => {
            let limit = 1u64 << (w.bits() - 1);
            if negative {
                magnitude <= limit
            } else {
                magnitude < limit
            }
        }
        Ty::UInt(w) => {
            !negative
                && match w {
                    UIntW::U8 => magnitude <= u8::MAX as u64,
                    UIntW::U16 => magnitude <= u16::MAX as u64,
                    UIntW::U32 => magnitude <= u32::MAX as u64,
                    UIntW::U64 => true,
                }
        }
        _ => true,
    }
}

pub(crate) fn default_positive_int_ty(value: u64) -> Ty {
    if value <= i32::MAX as u64 {
        Ty::Int(IntW::W32)
    } else if value <= i64::MAX as u64 {
        Ty::Int(IntW::W64)
    } else {
        Ty::UInt(UIntW::U64)
    }
}

pub(crate) fn default_negative_int_ty(magnitude: u64) -> Option<Ty> {
    if magnitude <= (i32::MAX as u64) + 1 {
        Some(Ty::Int(IntW::W32))
    } else if magnitude <= (i64::MAX as u64) + 1 {
        Some(Ty::Int(IntW::W64))
    } else {
        None
    }
}

pub(crate) fn check_type_slot(
    te: &TypeExpr,
    span: Span,
    structs: &HashMap<String, StructInfo>,
) -> AliasResult<Ty> {
    match te {
        TypeExpr::Generic(name, args) => {
            let want_arity = match name.as_str() {
                "result" => 2,
                "array" | "iterator" | "ptr" => 1,
                _ => {
                    return Err(AliasError {
                        msg: format!("泛型类型 {} 尚未实现", te.display()),
                        span,
                    })
                }
            };
            if args.len() != want_arity {
                return Err(AliasError {
                    msg: format!(
                        "{name} 需要 {want_arity} 个类型参数, 实际 {} 个",
                        args.len()
                    ),
                    span,
                });
            }
            let mut ts = Vec::with_capacity(args.len());
            for a in args {
                ts.push(check_type_slot(a, span, structs)?);
            }
            match name.as_str() {
                "result" => Ok(Ty::Result(Box::new(ts.remove(0)), Box::new(ts.remove(0)))),
                "array" => Ok(Ty::Array(Box::new(ts.remove(0)))),
                "iterator" => Ok(Ty::Iterator(Box::new(ts.remove(0)))),
                "ptr" => {
                    let pointee = ts.remove(0);
                    if matches!(pointee, Ty::Unit | Ty::FuncPoly | Ty::Unknown) {
                        return Err(AliasError {
                            msg: format!("ptr pointee {} 不是完整可存储类型", pointee.name()),
                            span,
                        });
                    }
                    Ok(Ty::Ptr {
                        pointee: Box::new(pointee),
                        nullable: false,
                    })
                }
                _ => Err(AliasError {
                    msg: "内部 sema 不变式被破坏: 泛型类型分类漂移".into(),
                    span,
                }),
            }
        }
        TypeExpr::Nullable(inner) => match check_type_slot(inner, span, structs)? {
            Ty::Ptr {
                pointee,
                nullable: false,
            } => Ok(Ty::Ptr {
                pointee,
                nullable: true,
            }),
            Ty::Ptr { nullable: true, .. } => Err(AliasError {
                msg: "ptr 类型已经是 nullable".into(),
                span,
            }),
            other => Err(AliasError {
                msg: format!("当前只支持 ptr<T>?，{} 不能使用 nullable 后缀", other.name()),
                span,
            }),
        },
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
                    Err(AliasError {
                        msg: format!("未知类型名 '{other}'"),
                        span,
                    })
                }
            }
        },
    }
}

pub(crate) fn check_value_type_slot(
    te: &TypeExpr,
    span: Span,
    structs: &HashMap<String, StructInfo>,
) -> AliasResult<Ty> {
    let ty = check_type_slot(te, span, structs)?;
    if ty.contains_unit() {
        Err(AliasError {
            msg: "unit 只能作为函数返回类型".into(),
            span,
        })
    } else {
        Ok(ty)
    }
}

pub(crate) fn check_return_type_slot(
    te: &TypeExpr,
    span: Span,
    structs: &HashMap<String, StructInfo>,
) -> AliasResult<Ty> {
    let ty = check_type_slot(te, span, structs)?;
    if ty != Ty::Unit && ty.contains_unit() {
        Err(AliasError {
            msg: "unit 只能单独作为函数返回类型".into(),
            span,
        })
    } else {
        Ok(ty)
    }
}
