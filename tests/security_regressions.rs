//! 内存安全与健壮性回归：每条都对应一次代码生成/运行时审计发现。

use alias::run;
use std::process::Command;

#[test]
fn narrow_shadow_uses_the_inner_cell_type() {
    let src = "func i32 main = () -> {\n    var i64 x = 9000000000\n    while true {\n        var i8 x = 1\n        x = 2\n        return to_i32(x)\n    }\n    return 3\n}\n";
    assert_eq!(run("shadow.as", src).unwrap(), 2);
}

#[test]
fn local_function_shadow_keeps_its_own_signature() {
    let src = "func i32 f = (i32 x) -> return x + 1\nfunc i32 main = () -> {\n    func f64 f = (f64 x) -> return x + 1.0\n    val f64 y = f(1.5)\n    while y != 2.5 { return 1 }\n    return 0\n}\n";
    assert_eq!(run("fn-shadow.as", src).unwrap(), 0);
}

#[test]
fn empty_string_and_empty_array_paths_do_not_dereference_null() {
    let src = "func i32 main = () -> {\n    val string empty = ''\n    while empty != ''.trim() { return 1 }\n    while '${empty}${empty}' != '' { return 2 }\n    var array<string> values = []\n    values.push(empty)\n    while values.pop() != '' { return 3 }\n    return 0\n}\n";
    assert_eq!(run("empty.as", src).unwrap(), 0);
}

#[test]
fn runtime_abort_returns_an_error_without_killing_the_host() {
    let bad = "func i32 main = () -> return 1 / 0\n";
    let err = run("bad.as", bad).expect_err("除零必须报错");
    assert_eq!(err.msg, "除以零");

    let good = "func i32 main = () -> return 7\n";
    assert_eq!(run("good.as", good).unwrap(), 7);
}

#[test]
fn concurrent_jit_runtime_errors_do_not_mix_span_tables() {
    let threads = (0..12)
        .map(|padding| {
            std::thread::spawn(move || {
                let src = format!(
                    "{}func i32 main = () -> return 1 / 0\n",
                    "\n".repeat(padding)
                );
                let err = run("concurrent.as", &src).expect_err("除零必须报错");
                (padding + 1, err)
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        let (line, err) = thread.join().unwrap();
        assert_eq!(err.msg, "除以零");
        assert_eq!(err.span.line as usize, line);
    }
}

#[test]
fn large_integer_literal_is_not_truncated_to_i32() {
    let src = "func i32 main = () -> {\n    val i64 wide = 2147483649\n    val i64 base = 2147483648\n    return to_i32(wide / base)\n}\n";
    assert_eq!(run("wide.as", src).unwrap(), 1);
}

#[test]
fn malformed_huge_integer_is_a_diagnostic_not_a_panic() {
    let src = format!(
        "func i32 main = () -> {{\n    val i64 x = {}\n    return 0\n}}\n",
        "9".repeat(10_000)
    );
    let err = run("huge-number.as", &src).expect_err("超大整数必须拒绝");
    assert!(err.msg.contains("整数"), "实际诊断: {}", err.msg);
}

#[test]
fn excessive_expression_nesting_is_rejected() {
    let nested = format!("{}0{}", "(".repeat(129), ")".repeat(129));
    let src = format!("func i32 main = () -> return {nested}\n");
    let err = run("nested.as", &src).expect_err("超深嵌套必须拒绝");
    assert!(err.msg.contains("语法嵌套超过 128 层上限"));
}

#[test]
fn excessive_generic_type_nesting_is_rejected() {
    let ty = format!("{}i32{}", "array<".repeat(129), ">".repeat(129));
    let src = format!("func i32 main = () -> {{\n    val {ty} x = []\n    return 0\n}}\n");
    let err = run("nested-type.as", &src).expect_err("超深泛型必须拒绝");
    assert!(err.msg.contains("类型嵌套超过 128 层上限"));
}

#[test]
fn excessive_interpolation_nesting_is_rejected_in_lexer() {
    let mut value = "'x'".to_string();
    for _ in 0..129 {
        value = format!("'${{{value}}}'");
    }
    let src = format!("func i32 main = () -> {{\n    println {value}\n    return 0\n}}\n");
    let err = run("nested-interpolation.as", &src).expect_err("超深插值必须拒绝");
    assert!(err.msg.contains("字符串插值嵌套超过 128 层上限"));
}

#[test]
fn build_refuses_a_non_as_input_instead_of_overwriting_it() {
    let dir = std::env::temp_dir().join(format!("alias-overwrite-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("program.EXE");
    let original = b"func i32 main = () -> return 0\n";
    std::fs::write(&input, original).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_alias"))
        .args(["build", input.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(std::fs::read(&input).unwrap(), original);

    let _ = std::fs::remove_file(input);
    let _ = std::fs::remove_dir(dir);
}
