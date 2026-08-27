//! Phase 2e 无括号文法泛化 — 正负向矩阵。
//! 优先级铁律: 无括号绑定紧于一切二元运算 (spec-notes 附录八)。

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

// ---------- 正向: 表达式位置吞参 ----------

#[test]
fn expr_pos_swallow_int() {
    let src = "func i32 dup = (i32 x) -> return x * 2\nfunc i32 main = () -> {\n    val i32 a = dup 5\n    return a\n}\n";
    assert_eq!(ok(src), 10);
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
    // 铁律对照: 显式括号后二元运算合法
    let src = "func i32 dup = (i32 x) -> return x * 2\nfunc i32 main = () -> {\n    val i32 a = dup 5\n    return (dup 3) + a\n}\n";
    assert_eq!(ok(src), 16); // (dup 3)=6, a=10, 6+10=16
}

// ---------- 正向: 方法中缀 ----------

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
    // 左结合: (1 combine 2) combine 3 = 6
    let src = format!("{COMBINE}func i32 main = () -> {{\n    return 1 combine 2 combine 3\n}}\n");
    assert_eq!(ok(&src), 6);
}

#[test]
fn method_infix_on_struct() {
    let src = "struct cell {\n    var i32 v = 0\n}\npub func i32 cell.bump = (i32 d) -> {\n    self.v = self.v + d;\n    return self.v\n}\nfunc i32 main = () -> {\n    var cell c = cell()\n    val i32 r = c bump 5\n    return r\n}\n";
    assert_eq!(ok(src), 5);
}

// ---------- 负向 ----------

#[test]
fn dangling_binop_after_swallow_rejected() {
    // 铁律: 吞参后二元运算符悬空 → 编译错误
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
    // 回归锚: 语句级内建吞参不受 P2e 影响
    let src = "func i32 main = () -> {\n    var i32 n = 0;\n    increase n\n    println n\n    return n\n}\n";
    assert_eq!(ok(src), 1);
}

#[test]
fn zero_arg_requires_parens() {
    // 零参裸名 = 函数值引用 (一等公民): 打印 <func> 而非调用;
    // 调用须显式括号 five()
    let src = "func i32 five = () -> return 5\nfunc i32 main = () -> {\n    println five\n    println five()\n    return five()\n}\n";
    assert_eq!(ok(src), 5);
}
