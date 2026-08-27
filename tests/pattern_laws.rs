//! Pattern AST / match 第一批完整法律测试。

use alias::ast::{BindKind, Body, CtorKind, Expr, Item, MatchArm, Pattern, Stmt};
use alias::{lexer::lex, parser::parse, run};

fn parsed_match_arms(src: &str) -> Vec<MatchArm> {
    let program = parse(lex(src).unwrap()).unwrap();
    let main = program
        .items
        .into_iter()
        .find_map(|item| match item {
            Item::Binding(b) if b.kind == BindKind::Func && b.name == "main" => Some(b),
            _ => None,
        })
        .expect("main binding");
    let Expr::FuncLit { body, .. } = main.value else {
        panic!("main 必须是函数字面量");
    };
    let Body::Block(stmts) = *body else {
        panic!("测试夹具要求块体 main");
    };
    for stmt in stmts {
        if let Stmt::Binding(binding) = stmt {
            if let Expr::Match { arms, .. } = binding.value {
                return arms;
            }
        }
    }
    panic!("未找到 match 表达式")
}

fn err(src: &str) -> String {
    match run("pattern.as", src) {
        Err(e) => e.msg,
        Ok(_) => panic!("应当报错"),
    }
}

#[test]
fn result_constructors_are_real_pattern_nodes() {
    let src = "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    val i32 v = match r {\n        ok(value) -> value\n        err(_) -> -1\n    }\n    return v\n}\n";
    let arms = parsed_match_arms(src);
    assert!(matches!(
        &arms[0].pattern,
        Pattern::Constructor { ctor: CtorKind::Ok, binding: Some(name), .. } if name == "value"
    ));
    assert!(matches!(
        &arms[1].pattern,
        Pattern::Constructor { ctor: CtorKind::Err, binding: None, .. }
    ));
}

#[test]
fn wildcard_and_binding_are_distinct_patterns() {
    let src = "func i32 main = () -> {\n    val i32 n = 7\n    val i32 v = match n {\n        0 -> 0\n        item -> item\n    }\n    return v\n}\n";
    let arms = parsed_match_arms(src);
    assert!(matches!(&arms[0].pattern, Pattern::Int { value: 0, .. }));
    assert!(matches!(&arms[1].pattern, Pattern::Binding { name, .. } if name == "item"));
}

#[test]
fn result_wildcard_payload_runs() {
    let src = "func i32 main = () -> {\n    val result<i32, string> r = ok(20)\n    val i32 v = match r {\n        ok(_) -> 42\n        err(_) -> -1\n    }\n    return v\n}\n";
    assert_eq!(run("pattern.as", src).unwrap(), 42);
}

#[test]
fn bool_literals_can_be_exhaustive() {
    let src = "func i32 main = () -> {\n    val bool b = true\n    val i32 v = match b {\n        true -> 42\n        false -> 0\n    }\n    return v\n}\n";
    assert_eq!(run("pattern.as", src).unwrap(), 42);
}

#[test]
fn integer_literal_with_wildcard_runs() {
    let src = "func i32 main = () -> {\n    val i32 n = 7\n    val i32 v = match n {\n        0 -> 1\n        7 -> 42\n        _ -> 2\n    }\n    return v\n}\n";
    assert_eq!(run("pattern.as", src).unwrap(), 42);
}

#[test]
fn string_literal_and_binding_whole_value_run() {
    let src = "func i32 main = () -> {\n    val string s = 'hello'\n    val i32 v = match s {\n        'bye' -> 1\n        text -> text.len()\n    }\n    return v\n}\n";
    assert_eq!(run("pattern.as", src).unwrap(), 5);
}

#[test]
fn duplicate_literal_pattern_is_rejected() {
    let src = "func i32 main = () -> {\n    val i32 n = 1\n    val i32 v = match n {\n        1 -> 1\n        1 -> 2\n        _ -> 3\n    }\n    return v\n}\n";
    assert!(err(src).contains("match 重复 Pattern: 1"));
}

#[test]
fn arm_after_catch_all_is_unreachable() {
    let src = "func i32 main = () -> {\n    val i32 n = 1\n    val i32 v = match n {\n        value -> value\n        1 -> 2\n    }\n    return v\n}\n";
    assert!(err(src).contains("match 存在不可达 Pattern"));
}

#[test]
fn bool_must_be_exhaustive_without_catch_all() {
    let src = "func i32 main = () -> {\n    val bool b = true\n    val i32 v = match b {\n        true -> 1\n    }\n    return v\n}\n";
    assert!(err(src).contains("match bool 必须覆盖 true 与 false"));
}

#[test]
fn open_integer_domain_requires_catch_all() {
    let src = "func i32 main = () -> {\n    val i32 n = 1\n    val i32 v = match n {\n        1 -> 1\n        2 -> 2\n    }\n    return v\n}\n";
    assert!(err(src).contains("必须提供 _ 或绑定 Pattern 作为兜底"));
}

#[test]
fn literal_pattern_type_mismatch_is_rejected() {
    let src = "func i32 main = () -> {\n    val bool b = true\n    val i32 v = match b {\n        1 -> 1\n        _ -> 0\n    }\n    return v\n}\n";
    assert!(err(src).contains("整数字面量 Pattern 不适用于 bool"));
}

#[test]
fn interpolated_string_is_not_a_literal_pattern() {
    let src = "func i32 main = () -> {\n    val string x = 'x'\n    val i32 v = match x {\n        '${x}' -> 1\n        _ -> 0\n    }\n    return v\n}\n";
    assert!(err(src).contains("match 字符串 Pattern 必须是纯字面量"));
}
