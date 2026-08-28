//! Alias 预定义语言名字的唯一分类表。
//!
//! 这些名字属于语言本身而不是普通词法绑定。parser 只查询语法所需的分类，
//! sema 负责拒绝用户声明覆盖它们；不要在各层复制字符串名单。

pub(crate) const CALL_BUILTINS: &[&str] = &[
    "print",
    "println",
    "from",
    "try_from",
    "typeof",
    "increase",
    "decrease",
];

pub(crate) const RESULT_CONSTRUCTORS: &[&str] = &["ok", "err"];

pub(crate) const TYPE_NAMES: &[&str] = &[
    "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64", "bool",
    "string", "unit", "func", "result", "array", "iterator",
];

pub(crate) fn is_no_paren_builtin(name: &str) -> bool {
    CALL_BUILTINS.contains(&name)
}

pub(crate) fn is_result_constructor(name: &str) -> bool {
    RESULT_CONSTRUCTORS.contains(&name)
}

pub(crate) fn is_reserved_lexical_name(name: &str) -> bool {
    CALL_BUILTINS.contains(&name) || RESULT_CONSTRUCTORS.contains(&name) || TYPE_NAMES.contains(&name)
}
