//! pub / 余数 / 位运算 / 移位法律测试。

use alias::run;

fn err(src: &str) -> String {
    match run(src) {
        Err(e) => e.msg,
        Ok(_) => panic!("应当报错"),
    }
}

#[test]
fn pub_named_recursion_runs() {
    let src = "pub func u32 fact = (u32 x) -> {\n    if x == 0 { return 1 }\n    return x * fact(x - 1)\n}\nfunc i32 main = () -> {\n    val u32 v = fact(5)\n    if v != 120 { return 1 }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn public_has_no_keyword_or_compatibility_diagnostic() {
    let src = "public func i32 nope = () -> return 1\nfunc i32 main = () -> return 0\n";
    let msg = err(src);
    assert!(!msg.contains("废弃"), "不应存在 public 迁移兼容诊断: {msg}");
    assert!(
        !msg.contains("请使用 pub"),
        "不应存在 public 迁移兼容诊断: {msg}"
    );
}

#[test]
fn remainder_runs_for_signed_and_unsigned_integers() {
    let src = "func i32 main = () -> {\n    val i32 a = 17 % 5\n    val i32 b = -17 % 5\n    val u32 u = 17\n    val u32 v = u % 5\n    if a != 2 { return 1 }\n    if b != -2 { return 2 }\n    if v != 2 { return 3 }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn bitwise_family_runs() {
    let src = "func i32 main = () -> {\n    val i32 a = 12 & 10\n    val i32 b = 12 | 3\n    val i32 c = 12 ^ 10\n    val i32 d = ~0\n    if a != 8 { return 1 }\n    if b != 15 { return 2 }\n    if c != 6 { return 3 }\n    if d != -1 { return 4 }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn shifts_preserve_signedness() {
    let src = "func i32 main = () -> {\n    val i32 a = 3 << 4\n    val i32 b = 48 >> 2\n    val i32 c = -8 >> 1\n    val u32 u = 16\n    val u32 d = u >> 2\n    if a != 48 { return 1 }\n    if b != 12 { return 2 }\n    if c != -4 { return 3 }\n    if d != 4 { return 4 }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn integer_expression_adopts_declared_slot_type() {
    let src = "func i32 main = () -> {\n    val u8 a = 1 | 2\n    val u8 b = ~a\n    val u8 c = b & 255\n    if a != 3 { return 1 }\n    if c != 252 { return 2 }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn bitwise_precedence_is_stable() {
    let src = "func i32 main = () -> {\n    val i32 v = 1 + 2 << 3 & 31\n    return v\n}\n";
    assert_eq!(run(src).unwrap(), 24);
}

#[test]
fn float_remainder_is_rejected() {
    let src = "func i32 main = () -> {\n    val f64 x = 5.0 % 2.0\n    return 0\n}\n";
    assert!(err(src).contains("运算符 % 不适用于 f64 与 f64"));
}

#[test]
fn mixed_integer_bitwise_is_rejected() {
    let src = "func i32 main = () -> {\n    val u32 a = 1\n    val i32 b = 2\n    val u32 c = a & b\n    return 0\n}\n";
    assert!(err(src).contains("u32 与 i32 禁止隐式混算"));
}

#[test]
fn compound_assignment_is_not_part_of_the_language() {
    let src = "func i32 main = () -> {\n    var i32 x = 1\n    x += 1\n    return x\n}\n";
    assert!(run(src).is_err());
}
