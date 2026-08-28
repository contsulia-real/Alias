//! 无括号函数调用 / 方法中缀语法法律。
//! 方法中缀是任意 `a.XXX(b)` → `a XXX b` / `a.XXX()` → `a XXX` 的糖；
//! parser 不得把任意两项 `IDENT IDENT` 预先解释成零参方法。

use alias::run;

fn ok(src: &str) -> i32 {
    run("test.as", src).unwrap_or_else(|e| panic!("应当通过, 实际: {e}"))
}

fn err(src: &str) -> String {
    match run("test.as", src) {
        Err(e) => e.msg,
        Ok(_) => panic!("应当报错"),
    }
}

const COMBINE: &str = "pub func i32 i32.combine = (i32 other) -> return self + other\n";

#[test]
fn expr_pos_swallow_int() {
    let src = "func i32 dup = (i32 x) -> return x * 2\nfunc i32 main = () -> {\n    val i32 a = dup 5\n    return a\n}\n";
    assert_eq!(ok(src), 10);
}

#[test]
fn identifier_argument_is_single_arg_function_call() {
    let src = "func u32 id = (u32 x) -> return x\nfunc i32 main = () -> {\n    val u32 x = 7\n    val u32 y = id x\n    if y == 7 { return 0 }\n    return 1\n}\n";
    assert_eq!(ok(src), 0);
}

#[test]
fn expr_pos_swallow_string() {
    let src = "func string wrap = (string s) -> return '[${s}]'\nfunc i32 main = () -> {\n    val string w = wrap 'hi'\n    while w != '[hi]' { return 1 }\n    return 0\n}\n";
    assert_eq!(ok(src), 0);
}

#[test]
fn expr_pos_swallow_paren_group() {
    let src = "func i32 dup = (i32 x) -> return x * 2\nfunc i32 main = () -> {\n    return dup (5 + 1)\n}\n";
    assert_eq!(ok(src), 12);
}

#[test]
fn explicit_parens_allow_binop_after() {
    let src = "func i32 dup = (i32 x) -> return x * 2\nfunc i32 main = () -> {\n    val i32 a = dup 5\n    return (dup 3) + a\n}\n";
    assert_eq!(ok(src), 16);
}

#[test]
fn method_infix_two_arg() {
    let src = format!(
        "{COMBINE}func i32 main = () -> {{\n    val i32 b = 3 combine 4\n    return b\n}}\n"
    );
    assert_eq!(ok(&src), 7);
}

#[test]
fn method_infix_zero_arg() {
    let src = "pub func string string.shout = () -> return '${self}!'\nfunc i32 main = () -> {\n    val string s = 'hi'\n    val string t = s shout\n    while t != 'hi!' { return 1 }\n    return 0\n}\n";
    assert_eq!(ok(src), 0);
}

#[test]
fn method_infix_left_assoc_chain() {
    let src = format!("{COMBINE}func i32 main = () -> {{\n    return 1 combine 2 combine 3\n}}\n");
    assert_eq!(ok(&src), 6);
}

#[test]
fn method_infix_on_struct() {
    let src = "struct cell {\n    var i32 v = 0\n}\npub func i32 cell.bump = (i32 d) -> {\n    self.v = self.v + d;\n    return self.v\n}\nfunc i32 main = () -> {\n    var cell c = cell()\n    val i32 r = c bump 5\n    return r\n}\n";
    assert_eq!(ok(src), 5);
}

#[test]
fn dangling_binop_after_swallow_rejected() {
    let src = "func i32 dup = (i32 x) -> return x * 2\nfunc i32 main = () -> {\n    val i32 a = dup 5 + 1\n    return a\n}\n";
    let msg = err(src);
    assert!(
        msg.contains("无法开始一个表达式") || msg.contains("意外的表达式"),
        "实际: {msg}"
    );
}

#[test]
fn unknown_method_infix_rejected() {
    let src = "func i32 main = () -> {\n    val i32 x = 3 nosuch 4\n    return x\n}\n";
    let msg = err(src);
    assert!(msg.contains("nosuch"), "应指明未知方法, 实际: {msg}");
}

#[test]
fn bare_builtin_still_works_stmt_pos() {
    let src = "func i32 main = () -> {\n    var i32 n = 0;\n    increase n\n    println n\n    return n\n}\n";
    assert_eq!(ok(src), 1);
}

#[test]
fn zero_arg_function_may_omit_parens() {
    let src = "func i32 five = () -> return 5\nfunc i32 main = () -> {\n    val i32 a = five\n    return a\n}\n";
    assert_eq!(ok(src), 5);
}

#[test]
fn explicit_zero_arg_call_remains_valid() {
    let src = "func i32 five = () -> return 5\nfunc i32 main = () -> return five()\n";
    assert_eq!(ok(src), 5);
}
