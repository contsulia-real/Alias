//! Phase 1 语义冒烟测试 — 每条用例对应一条宪法裁决的可执行验证。

use alias::run;

fn ok(src: &str) -> i32 {
    run("test.as", src).unwrap_or_else(|e| panic!("应当通过, 实际: {e}"))
}

fn should_fail(src: &str) -> String {
    match run("test.as", src) {
        Err(e) => e.msg,
        Ok(_) => panic!("应当报错"),
    }
}

#[test]
fn count_to_ten_demo() {
    let src = include_str!("../demos/count_to_ten.as");
    assert_eq!(run("count_to_ten.as", src).unwrap(), 0);
}

#[test]
fn arithmetic_and_precedence() {
    let src = "
func i32 main = () -> {
    var i32 x = 6;
    increase x
    val i32 y = x * 7;
    return y - 1
}
";
    // (6+1)*7-1 = 48
    assert_eq!(ok(src), 48);
}

#[test]
fn closure_reads_latest_value() {
    let src = "
func i32 main = () -> {
    var i32 n = 0;
    func bool lt3 = (i32 cap) -> return n < cap
    var i32 rounds = 0;
    for lt3(3) {
        increase n
        increase rounds
    }
    return rounds
}
";
    assert_eq!(ok(src), 3);
}

#[test]
fn string_interpolation() {
    let src = r#"
func bool main = () -> {
    var i32 i = 4;
    return 'n=$i' == 'n=4'
}
"#;
    // main 返回 bool: true → 退出码 0
    assert_eq!(run("t.as", src).unwrap(), 0);
}

#[test]
fn val_reassignment_rejected() {
    let src = "
func i32 main = () -> {
    val i32 a = 1;
    a = 2;
    return 0
}
";
    let msg = should_fail(src);
    assert!(msg.contains("val"), "报错应指明 val 不可赋值, 实际: {msg}");
}

#[test]
fn missing_type_slot_rejected() {
    // 类型槽强制非空 — 无推断 (宪法法律)
    let src = "
func i32 main = () -> {
    var x = 1;
    return 0
}
";
    let msg = should_fail(src);
    assert!(msg.contains("类型槽"), "实际: {msg}");
}

#[test]
fn no_paren_call_single_arg_only() {
    // increase 后同行吞一个一元表达式; 跨行不吞
    let src = "
func i32 main = () -> {
    var i32 a = 10;
    decrease a
    return a
}
";
    assert_eq!(ok(src), 9);
}

#[test]
fn while_false_is_dead_code() {
    let src = "
func i32 main = () -> {
    while false {
        return 7
    }
    return 3
}
";
    assert_eq!(ok(src), 3);
}
