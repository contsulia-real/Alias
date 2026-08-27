use alias::{run, AliasError};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CASE_SEQ: AtomicUsize = AtomicUsize::new(0);

fn fail(src: &str) -> AliasError {
    run("conversion-law.as", src).expect_err("该程序应在编译期失败")
}

fn run_cli(src: &str) -> std::process::Output {
    let seq = CASE_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("alias-conversion-{}-{seq}", std::process::id()));
    std::fs::create_dir(&dir).expect("创建临时目录失败");
    let source = dir.join("case.as");
    std::fs::write(&source, src).expect("写入临时源码失败");
    let output = Command::new(env!("CARGO_BIN_EXE_alias"))
        .args(["run", source.to_str().unwrap()])
        .output()
        .expect("启动 Alias 编译器失败");
    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_dir(dir);
    output
}

#[test]
fn explicit_and_contextual_numeric_conversions_use_the_target_type() {
    let src = r#"
func i32 main = () -> {
    val i64 wide = 255
    val u8 byte = (u8) wide
    val i16 signed = from(byte)
    val f32 fractional = from signed
    val i32 result = (i32) fractional
    return result
}
"#;
    assert_eq!(run("conversion-forms.as", src).unwrap(), 255);
}

#[test]
fn displayable_values_convert_to_string_in_slots_and_interpolation() {
    let src = r#"
func i32 main = () -> {
    val u32 u = 1
    val string contextual = from u
    val string explicit = (string) u
    val string interpolated_from = '${from u}'
    val string interpolated_try = '${try_from u}'
    val string interpolated_explicit = '${(string) u}'
    val bool flag = true
    val array<u32> values = [u]
    val string array_display = from(values)
    if contextual != '1' { return 1 }
    if explicit != '1' { return 2 }
    if interpolated_from != '1' { return 3 }
    if interpolated_try != '1' { return 4 }
    if interpolated_explicit != '1' { return 5 }
    if (string) flag != 'true' { return 6 }
    if array_display != '<array>' { return 7 }
    return 0
}
"#;
    assert_eq!(run("string-conversions.as", src).unwrap(), 0);
}

#[test]
fn conversion_without_a_target_context_is_rejected() {
    for name in ["from", "try_from"] {
        let src = format!("func i32 main = () -> {{\n    println({name}(1))\n    return 0\n}}\n");
        let error = fail(&src);
        assert_eq!(error.msg, format!("{name} 需要目标类型上下文"));
    }
}

#[test]
fn from_requires_a_defined_conversion_relationship() {
    let error = fail(
        "func i32 main = () -> {\n    val string text = 'boy'\n    val i32 value = from(text)\n    return value\n}\n",
    );
    assert_eq!(
        error.msg,
        "绑定 'value' 声明类型为 i32: from 不存在 string → i32 转换"
    );

    let explicit = fail(
        "func i32 main = () -> {\n    val string text = 'boy'\n    val i32 value = (i32) text\n    return value\n}\n",
    );
    assert_eq!(
        explicit.msg,
        "绑定 'value' 声明类型为 i32: 不存在 string → i32 转换"
    );
}

#[test]
fn try_from_without_a_conversion_falls_back_to_the_source_type() {
    let fallback = r#"
func i32 main = () -> {
    val bool b = true
    val bool unchanged = try_from b
    return unchanged ? 0 : 1
}
"#;
    assert_eq!(run("try-from-fallback.as", fallback).unwrap(), 0);

    let direct = fail(
        "func i32 main = () -> {\n    val string b = 'boy'\n    val i32 a = b\n    return a\n}\n",
    );
    let attempted = fail(
        "func i32 main = () -> {\n    val string b = 'boy'\n    val i32 a = try_from(b)\n    return a\n}\n",
    );
    assert_eq!(attempted.msg, direct.msg);
    assert_eq!(
        attempted.msg,
        "绑定 'a' 声明类型为 i32: 需要 i32, 实际 string"
    );
}

#[test]
fn numeric_value_overflow_is_never_a_try_from_fallback() {
    let cases = [
        "func i32 main = () -> {\n    val i32 source = 256\n    val u8 out = (u8) source\n    return 0\n}\n",
        "func i32 main = () -> {\n    val i32 source = -1\n    val u64 out = from(source)\n    return 0\n}\n",
        "func i32 main = () -> {\n    val i32 source = 256\n    val u8 out = try_from(source)\n    return 0\n}\n",
    ];
    for src in cases {
        let output = run_cli(src);
        assert_eq!(output.status.code(), Some(1), "源码未以 1 中止:\n{src}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.starts_with("错误 @ ") && stderr.ends_with(" — 转换越界\n"),
            "转换诊断错误: {stderr:?}\n源码:\n{src}"
        );
    }
}

#[test]
fn full_u64_literal_range_is_preserved() {
    let src = r#"
func i32 main = () -> {
    val u64 max = 18446744073709551615
    println max
    val u64 one = 1
    val u64 below = max - one
    if below != 18446744073709551614 { return 1 }
    return 0
}
"#;
    let output = run_cli(src);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"18446744073709551615\n");
    assert_eq!(output.stderr, b"");

    let error = fail(
        "func i32 main = () -> {\n    val u64 too_large = 18446744073709551616\n    return 0\n}\n",
    );
    assert!(error.msg.contains("整数字面量超出 u64 表示范围"));
}

#[test]
fn retired_to_builtins_have_no_compatibility_alias() {
    let error = fail("func i32 main = () -> return to_i32(1)\n");
    assert_eq!(error.msg, "return 需要 i32: 未定义的绑定 'to_i32'");
}
