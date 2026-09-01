//! Pattern AST / match 当前法律测试。

use alias::run;

fn err(src: &str) -> String {
    match run(src) {
        Err(e) => e.msg,
        Ok(_) => panic!("应当报错"),
    }
}

#[test]
fn result_constructor_patterns_bind_and_select_by_variant() {
    let src = "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    val i32 v = match r {\n        ok(value) -> value\n        err(_) -> -1\n    }\n    return v\n}\n";
    assert_eq!(run(src).unwrap(), 1);
}

#[test]
fn catch_all_binding_exposes_the_whole_subject() {
    let src = "func i32 main = () -> {\n    val i32 n = 7\n    val i32 v = match n {\n        0 -> 0\n        item -> item\n    }\n    return v\n}\n";
    assert_eq!(run(src).unwrap(), 7);
}

#[test]
fn result_wildcard_payload_runs() {
    let src = "func i32 main = () -> {\n    val result<i32, string> r = ok(20)\n    val i32 v = match r {\n        ok(_) -> 42\n        err(_) -> -1\n    }\n    return v\n}\n";
    assert_eq!(run(src).unwrap(), 42);
}

#[test]
fn bool_literals_can_be_exhaustive() {
    let src = "func i32 main = () -> {\n    val bool b = true\n    val i32 v = match b {\n        true -> 42\n        false -> 0\n    }\n    return v\n}\n";
    assert_eq!(run(src).unwrap(), 42);
}

#[test]
fn integer_literal_with_wildcard_runs() {
    let src = "func i32 main = () -> {\n    val i32 n = 7\n    val i32 v = match n {\n        0 -> 1\n        7 -> 42\n        _ -> 2\n    }\n    return v\n}\n";
    assert_eq!(run(src).unwrap(), 42);
}

#[test]
fn string_literal_and_binding_whole_value_run() {
    let src = "func i32 main = () -> {\n    val string s = 'hello'\n    val i32 v = match s {\n        'bye' -> 1\n        text -> text.len()\n    }\n    return v\n}\n";
    assert_eq!(run(src).unwrap(), 5);
}

#[test]
fn whole_subject_binding_deep_clones_a_stable_dynamic_place() {
    let src = r#"
struct cell { var i32 value = 0 }
func i32 main = () -> {
    val cell source = cell(value = 7)
    match source {
        item -> { item.value = 99 }
    }
    return source.value
}
"#;
    assert_eq!(run(src).unwrap(), 7);
}

#[test]
fn constructor_payload_binding_does_not_partially_move_from_result() {
    let src = r#"
struct cell { var i32 value = 0 }
func i32 main = () -> {
    val result<cell, string> wrapped = ok(cell(value = 7))
    match wrapped {
        ok(item) -> { item.value = 99 }
        err(_) -> { return 1 }
    }
    return match wrapped {
        ok(item) -> item.value
        err(_) -> 2
    }
}
"#;
    assert_eq!(run(src).unwrap(), 7);
}

#[test]
fn whole_subject_binding_accepts_owned_temporary_transfer() {
    let src = r#"
struct cell { var i32 value = 0 }
func i32 main = () -> {
    return match cell(value = 7) {
        item -> item.value
    }
}
"#;
    assert_eq!(run(src).unwrap(), 7);
}

#[test]
fn stable_non_deep_cloneable_subject_does_not_fall_back_to_aliasing() {
    let src = r#"
func i32 main = () -> {
    val array<i32> values = [7]
    val iterator<i32> source = values.iterator()
    return match source {
        item -> 0
    }
}
"#;
    let error = err(src);
    assert!(error.contains("不支持 clone"), "{error}");
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
