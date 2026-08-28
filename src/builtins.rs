//! Alias 预定义语言名字的唯一分类表。
//!
//! 这些名字属于语言本身而不是普通词法绑定。parser 只查询语法所需的分类，
//! sema 消费结构化 builtin 身份；不要在各层复制字符串名单或再次按名字恢复身份。

use crate::ast::CtorKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CallBuiltinName {
    Print,
    Println,
    From,
    TryFrom,
    Typeof,
    Increase,
    Decrease,
}

pub(crate) fn classify_call_builtin(name: &str) -> Option<CallBuiltinName> {
    Some(match name {
        "print" => CallBuiltinName::Print,
        "println" => CallBuiltinName::Println,
        "from" => CallBuiltinName::From,
        "try_from" => CallBuiltinName::TryFrom,
        "typeof" => CallBuiltinName::Typeof,
        "increase" => CallBuiltinName::Increase,
        "decrease" => CallBuiltinName::Decrease,
        _ => return None,
    })
}

pub(crate) fn classify_result_constructor(name: &str) -> Option<CtorKind> {
    Some(match name {
        "ok" => CtorKind::Ok,
        "err" => CtorKind::Err,
        _ => return None,
    })
}

pub(crate) const TYPE_NAMES: &[&str] = &[
    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64", "bool", "string", "unit",
    "func", "result", "array", "iterator",
];

pub(crate) fn is_no_paren_builtin(name: &str) -> bool {
    classify_call_builtin(name).is_some()
}

pub(crate) fn is_output_builtin(name: &str) -> bool {
    matches!(
        classify_call_builtin(name),
        Some(CallBuiltinName::Print | CallBuiltinName::Println)
    )
}

pub(crate) fn is_reserved_lexical_name(name: &str) -> bool {
    classify_call_builtin(name).is_some()
        || classify_result_constructor(name).is_some()
        || TYPE_NAMES.contains(&name)
}
