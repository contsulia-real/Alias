use alias::{run, AliasError};

fn fail(src: &str) -> AliasError {
    run(src).expect_err("该程序应在编译期失败")
}

#[test]
fn typeof_supports_parenthesized_and_noparen_static_queries() {
    let src = r#"
func i32 main = () -> {
    val u32 u = 1
    val array<u32> values = [u]
    val iterator<u32> cursor = values.iterator()
    val result<u32, string> outcome = ok(u)
    val string bare = typeof u
    if bare != 'u32' { return 1 }
    if typeof(values) != 'array<u32>' { return 2 }
    if '${typeof u}' != 'u32' { return 3 }
    if typeof((string) u) != 'string' { return 4 }
    if (typeof cursor) != 'iterator<u32>' { return 5 }
    if (typeof outcome) != 'result<u32, string>' { return 6 }
    return 0
}
"#;
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn typeof_checks_but_never_evaluates_its_expression() {
    let src = r#"
func i32 main = () -> {
    val string division = typeof(1 / 0)
    if division != 'i32' { return 1 }
    return 0
}
"#;
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn typeof_validates_arity_and_the_queried_expression() {
    for src in [
        "func i32 main = () -> {\n    println(typeof())\n    return 0\n}\n",
        "func i32 main = () -> {\n    println(typeof(1, 2))\n    return 0\n}\n",
    ] {
        assert_eq!(fail(src).msg, "typeof 恰好接受 1 个参数");
    }

    let undefined =
        fail("func i32 main = () -> {\n    val string name = typeof missing\n    return 0\n}\n");
    assert_eq!(
        undefined.msg,
        "绑定 'name' 声明类型为 string: 未定义的绑定 'missing'"
    );

    for expression in ["[]", "ok(1)"] {
        let source = format!(
            "func i32 main = () -> {{\n    val string name = typeof({expression})\n    return 0\n}}\n"
        );
        let error = fail(&source);
        assert!(
            error.msg.contains("typeof 无法确定实参的静态类型"),
            "实际诊断: {}",
            error.msg
        );
    }
}
