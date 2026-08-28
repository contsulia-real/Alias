use alias::{run, AliasError};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CASE_SEQ: AtomicUsize = AtomicUsize::new(0);

fn next_case_seq() -> usize {
    // 只为同一测试进程生成互不相同的临时目录后缀，不承担状态发布或 happens-before；
    // Relaxed 已提供所需的唯一原子递增语义。
    CASE_SEQ.fetch_add(1, Ordering::Relaxed)
}

fn fail(src: &str) -> AliasError {
    run(src).expect_err("该程序应在编译期失败")
}

fn run_cli(src: &str) -> std::process::Output {
    let seq = next_case_seq();
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
    val i32 converted = (i32) fractional
    return converted
}
"#;
    assert_eq!(run(src).unwrap(), 255);
}

#[test]
fn assignments_propagate_their_target_type_into_conversions() {
    let src = r#"
struct aaa {
    var i32 x = 1
}
func i32 main = () -> {
    val aaa ass = aaa()
    val u32 u = 7
    ass.x = from u
    var i32 direct = 0
    direct = try_from u
    return ass.x * 10 + direct
}
"#;
    assert_eq!(run(src).unwrap(), 77);
}

#[test]
fn every_typed_value_slot_propagates_its_target_into_conversions() {
    let src = r#"
val u32 seed = 3
struct defaults {
    val i32 value = from seed
}
struct box {
    var i32 value = 0
}
func i32 through_return = (u32 value) -> return from value
func i32 accept = (i32 value) -> return value
func i32 main = () -> {
    val u32 source = 4
    val i32 declared = from source
    var i32 assigned = 0
    assigned = from source
    val box field = box(value = from source)
    field.value = from source
    val i32 argument = accept(from source)
    val array<i32> values = [from source]
    val result<i32, string> wrapped = ok(from source)
    val i32 result_value = match wrapped {
        ok(value) -> value
        err(_) -> 0
    }
    val i32 matched = match true {
        true -> from source
        false -> 0
    }
    val i32 composite = (from source) + (from source)
    val defaults defaulted = defaults()
    return declared + assigned + field.value + argument + values[0] + result_value + matched + composite + through_return(source) + defaulted.value
}
"#;
    assert_eq!(run(src).unwrap(), 43);
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
    assert_eq!(run(src).unwrap(), 0);
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
    assert_eq!(run(fallback).unwrap(), 0);

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
fn try_from_fallback_preserves_outer_slot_diagnostics() {
    let pairs = [
        (
            "val array<i32> out = [b]",
            "val array<i32> out = [try_from b]",
            "绑定 'out' 声明类型为 array<i32>: 数组元素类型不一致: i32 与 string",
        ),
        (
            "val result<i32, string> out = ok(b)",
            "val result<i32, string> out = ok(try_from b)",
            "绑定 'out' 声明类型为 result<i32, string>: 需要 i32, 实际 string",
        ),
        (
            "val i32 out = match true { true -> b, false -> 0 }",
            "val i32 out = match true { true -> try_from b, false -> 0 }",
            "绑定 'out' 声明类型为 i32: 需要 i32, 实际 string",
        ),
        (
            "val i32 out = b + 1",
            "val i32 out = (try_from b) + 1",
            "绑定 'out' 声明类型为 i32: 运算符 + 不适用于 string 与 i32",
        ),
        (
            "val i32 out = accept(b)",
            "val i32 out = accept(try_from b)",
            "绑定 'out' 声明类型为 i32: 第 1 个实参需要 i32, 实际 string",
        ),
        (
            "val box out = box(value = b)",
            "val box out = box(value = try_from b)",
            "绑定 'out' 声明类型为 box: 字段 'value' 需要 i32, 实际 string",
        ),
        (
            "values.push(b)",
            "values.push(try_from b)",
            "第 1 个实参需要 i32, 实际 string",
        ),
        (
            "receiver.accept_value(b)",
            "receiver.accept_value(try_from b)",
            "第 1 个实参需要 i32, 实际 string",
        ),
    ];
    for (direct, attempted, expected_message) in pairs {
        let program = |statement: &str| {
            format!(
                "struct box {{ val i32 value = 0 }}\nfunc i32 accept = (i32 value) -> return value\nfunc i32 i32.accept_value = (i32 value) -> return value\nfunc i32 main = () -> {{\n    val string b = 'boy'\n    val array<i32> values = []\n    val i32 receiver = 1\n    {statement}\n    return 0\n}}\n"
            )
        };
        let direct_error = fail(&program(direct));
        let attempted_error = fail(&program(attempted));
        assert_eq!(attempted_error.msg, direct_error.msg, "语句: {attempted}");
        assert_eq!(attempted_error.msg, expected_message, "语句: {attempted}");
    }

    let default_program = |expression: &str| {
        format!(
            "val string b = 'boy'\nstruct box {{ val i32 value = {expression} }}\nfunc i32 main = () -> return 0\n"
        )
    };
    let direct_default = fail(&default_program("b"));
    let attempted_default = fail(&default_program("try_from b"));
    assert_eq!(attempted_default.msg, direct_default.msg);
    assert_eq!(
        attempted_default.msg,
        "字段 'value' 声明类型为 i32, 实际 string"
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
fn to_i32_is_not_a_predefined_name() {
    let error = fail("func i32 main = () -> return to_i32(1)\n");
    assert_eq!(error.msg, "return 需要 i32: 未定义的绑定 'to_i32'");
}
