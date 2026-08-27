//! Pattern AST / match foundation 法律测试。
//!
//! 当前语言表面仍严格只有 result constructor pattern: ok(name) / err(name)。
//! 本组测试锁定 AST 形状以及既有穷尽性/重复臂诊断不漂移。

use alias::ast::{BindKind, Body, CtorKind, Expr, Item, Stmt};
use alias::{lexer::lex, parser::parse, run};

fn parsed_match_arms(src: &str) -> Vec<alias::ast::MatchArm> {
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

#[test]
fn ok_err_are_materialized_as_pattern_nodes() {
    let src = "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    val i32 v = match r {\n        ok(value) -> value\n        err(problem) -> -1\n    }\n    return v\n}\n";
    let arms = parsed_match_arms(src);
    assert_eq!(arms.len(), 2);
    assert_eq!(arms[0].pattern.ctor, CtorKind::Ok);
    assert_eq!(arms[0].pattern.binding, "value");
    assert_eq!(arms[1].pattern.ctor, CtorKind::Err);
    assert_eq!(arms[1].pattern.binding, "problem");
}

#[test]
fn pattern_binding_still_flows_through_sema_and_codegen() {
    let src = "func i32 main = () -> {\n    val result<i32, string> r = ok(20)\n    val i32 v = match r {\n        ok(value) -> value + 22\n        err(problem) -> -1\n    }\n    return v\n}\n";
    assert_eq!(run("pattern.as", src).unwrap(), 42);
}

#[test]
fn duplicate_constructor_coverage_is_still_rejected() {
    let src = "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    val i32 v = match r {\n        ok(x) -> x\n        ok(y) -> y\n        err(e) -> -1\n    }\n    return v\n}\n";
    let err = run("pattern.as", src).unwrap_err();
    assert!(err.msg.contains("match 重复覆盖 ok 臂"), "{}", err.msg);
}

#[test]
fn exhaustive_result_coverage_is_still_required() {
    let src = "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    val i32 v = match r {\n        ok(x) -> x\n    }\n    return v\n}\n";
    let err = run("pattern.as", src).unwrap_err();
    assert!(err.msg.contains("match 必须同时覆盖 ok 与 err"), "{}", err.msg);
}

#[test]
fn unsupported_constructor_surface_does_not_expand_yet() {
    let src = "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    val i32 v = match r {\n        some(x) -> x\n        err(e) -> -1\n    }\n    return v\n}\n";
    let err = run("pattern.as", src).unwrap_err();
    assert!(
        err.msg.contains("match 臂构造器必须是 ok 或 err"),
        "{}",
        err.msg
    );
}
