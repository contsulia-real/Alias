//! 命名递归 / 整数字面量目标类型 / 嵌套无括号输出调用回归法律。
//!
//! 本批只修三个实现缺口，不扩大一般隐式转换，也不推翻 P2e 的二元运算绑定规则。

use alias::run;

fn err(src: &str) -> String {
    match run("test.as", src) {
        Err(e) => e.msg,
        Ok(_) => panic!("应当报错"),
    }
}

#[test]
fn pub_u32_named_recursion_and_nested_print_call_run() {
    let src = "pub func u32 fact_while = (u32 x) -> {\n    if x == 0 {\n        return 1\n    }\n    return fact_while (x-1);\n}\nfunc i32 main = () -> {\n    println fact_while 3;\n    println fact_while 0;\n    return 0;\n}\n";
    assert_eq!(run("test.as", src).unwrap(), 0);
}

#[test]
fn rhs_integer_literal_adopts_established_left_integer_type() {
    let src = "func i32 main = () -> {\n    val u32 x = 3\n    if x == 0 { return 1 }\n    val u32 y = x - 1\n    if y != 2 { return 2 }\n    return 0\n}\n";
    assert_eq!(run("test.as", src).unwrap(), 0);
}

#[test]
fn rhs_integer_literal_is_range_checked_for_left_type() {
    let src =
        "func i32 main = () -> {\n    val u8 x = 1\n    val u8 y = x + 256\n    return 0\n}\n";
    let msg = err(src);
    assert!(msg.contains("字面量 256 超出 u8 的表示范围"), "实际: {msg}");
}

#[test]
fn mixed_integer_variables_remain_forbidden() {
    let src = "func i32 main = () -> {\n    val u32 a = 1\n    val i32 b = 2\n    val u32 c = a + b\n    return 0\n}\n";
    let msg = err(src);
    assert!(msg.contains("u32 与 i32 禁止隐式混算"), "实际: {msg}");
}

#[test]
fn dangling_binary_after_noparen_call_remains_rejected() {
    let src = "func i32 dup = (i32 x) -> return x * 2\nfunc i32 main = () -> {\n    val i32 a = dup 5 + 1\n    return a\n}\n";
    let msg = err(src);
    assert!(
        msg.contains("无法开始一个表达式") || msg.contains("意外的表达式"),
        "实际: {msg}"
    );
}

#[test]
fn later_function_is_not_made_visible_as_a_forward_reference() {
    let src = "func i32 first = () -> return later()\nfunc i32 later = () -> return 1\nfunc i32 main = () -> return first()\n";
    let msg = err(src);
    assert!(msg.contains("未定义的绑定 'later'"), "实际: {msg}");
}
