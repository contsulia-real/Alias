//! result/match/?/转义当前法律测试 — 正负矩阵，负向断言精确中文消息 + 行:列。
//!
//! 语义锚点：
//! - result<T,E> 内建枚举: ok/err 为类型构造器 (非名字分派函数)
//! - result 的构造器 Pattern 由 ok/err 穷尽；match 使用统一 Pattern AST
//! - ? 脱糖 = match e { ok(v) -> v, err(e) -> return err(e) } — 仅同型错误
//! - 全 never 臂 match 等价 return 收尾，可满足非 unit 函数必返回要求
//!
//! 列号语义: token span col = max(可视列-1, 1) (lexer.rs span_here)。

use alias::{run, AliasError};

fn fail(src: &str) -> AliasError {
    match run(src) {
        Err(e) => e,
        Ok(_) => panic!("应当报错"),
    }
}

fn assert_law(src: &str, want_sub: &str, line: u32, col: u32) {
    let e = fail(src);
    assert!(
        e.msg.contains(want_sub),
        "消息应含「{want_sub}」, 实际: {}",
        e.msg
    );
    assert_eq!(
        (e.span.line, e.span.col),
        (line, col),
        "span 不符: 实际 {}:{} — {}",
        e.span.line,
        e.span.col,
        e.msg
    );
}

// ---------------------------------------------------------------------------
// 正向矩阵
// ---------------------------------------------------------------------------

/// 值产出臂: match 是表达式, 两臂同型给值, 公共类型即匹配值。
#[test]
fn value_producing_arms_common_type() {
    let src = "func i32 main = () -> {\n    val result<i32, string> r = ok(21)\n    val i32 v = match r {\n        ok(x) -> x * 2\n        err(e) -> -1\n    }\n    return v\n}\n";
    assert_eq!(run(src).unwrap(), 42);
}

/// never 流臂作语句: err 臂 return 直接决定函数退出码。
#[test]
fn never_arm_as_statement_drives_exit() {
    let src = "func result<i32, string> mk = (i32 n) -> {\n    while n == 0 {\n        return err('零')\n    }\n    return ok(n)\n}\nfunc i32 run = () -> {\n    match mk(0) {\n        ok(v) -> {\n            println(v)\n            return 0\n        }\n        err(e) -> {\n            println(e)\n            return 3\n        }\n    }\n}\nfunc i32 main = () -> {\n    return run()\n}\n";
    assert_eq!(run(src).unwrap(), 3);
}

/// ? 快乐路径: 同型错误跨 T 传播, ok 值解包继续。
#[test]
fn propagate_happy_path() {
    let src = "func result<i32, string> half = (i32 n) -> {\n    while n < 0 {\n        return err('负数')\n    }\n    return ok(n / 2)\n}\nfunc result<i32, string> quarter = (i32 n) -> {\n    val i32 h = half(n)?\n    return ok(h / 2)\n}\nfunc i32 main = () -> {\n    match quarter(8) {\n        ok(v) -> println(v)\n        err(e) -> println(e)\n    }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

/// ? 穿透循环: 循环体内触发的 err 经 ? 直接传出函数 (脱糖的 return 流)。
#[test]
fn propagate_through_loop_bubbles_err() {
    let src = "func result<i32, string> inv = (i32 n) -> {\n    while n == 0 {\n        return err('除零')\n    }\n    return ok(10 / n)\n}\nfunc result<i32, string> first_ok_or_bubble = (i32 n) -> {\n    while n >= 0 {\n        val i32 v = inv(n)?\n        return ok(v)\n    }\n    return err('不可达')\n}\nfunc i32 main = () -> {\n    match first_ok_or_bubble(0) {\n        ok(v) -> println(v)\n        err(e) -> println(e)\n    }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

/// 结构体载荷: 臂绑定字段读 + var 字段写; 全 never 臂 match 作函数尾。
#[test]
fn struct_payload_arms_and_never_tail() {
    let src = "struct stat {\n    val i32 lines = 0\n    var i32 bytes\n}\nfunc result<stat, string> mk = (i32 l) -> {\n    while l < 0 {\n        return err('负')\n    }\n    return ok(stat(lines = l, bytes = l * 2))\n}\nfunc i32 main = () -> {\n    match mk(5) {\n        ok(s) -> {\n            s.bytes = s.bytes + 40\n            return s.lines + s.bytes\n        }\n        err(e) -> {\n            println(e)\n            return 1\n        }\n    }\n}\n";
    assert_eq!(run(src).unwrap(), 55);
}

/// 一般 Pattern 允许非 result 主语。
#[test]
fn general_match_allows_non_result_subject() {
    let src = "func i32 main = () -> {\n    val i32 v = match 5 {\n        5 -> 42\n        _ -> 0\n    }\n    return v\n}\n";
    assert_eq!(run(src).unwrap(), 42);
}

/// 转义往返: \\0 \\' \\\" \\\\ 解码一致; \\t 与 \\n 解码不同。
#[test]
fn escape_round_trips() {
    let src = "func i32 main = () -> {\n    while 'x\\0y' != 'x\\0y' {\n        return 1\n    }\n    while 'it\\'s \\\"q\\\" \\\\z' != 'it\\'s \\\"q\\\" \\\\z' {\n        return 2\n    }\n    while 'a\\tb' == 'a\\nb' {\n        return 3\n    }\n    return 42\n}\n";
    assert_eq!(run(src).unwrap(), 42);
}

// ---------------------------------------------------------------------------
// 负向矩阵 — 穷尽性与臂形状
// ---------------------------------------------------------------------------

#[test]
fn missing_err_arm_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    val i32 v = match r {\n        ok(x) -> x\n    }\n    return v\n}\n",
        "match 必须同时覆盖 ok 与 err",
        3, 16,
    );
}

#[test]
fn missing_ok_arm_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    val i32 v = match r {\n        err(e) -> -1\n    }\n    return v\n}\n",
        "match 必须同时覆盖 ok 与 err",
        3, 16,
    );
}

#[test]
fn constructor_pattern_requires_result_subject() {
    let src = "func i32 main = () -> {\n    val i32 v = match 5 {\n        ok(x) -> x\n        _ -> 0\n    }\n    return v\n}\n";
    let e = fail(src);
    assert!(
        e.msg.contains("构造器 Pattern 需要 result 主语, 实际 i32"),
        "实际: {}",
        e.msg
    );
}

#[test]
fn duplicate_ok_arm_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    val i32 v = match r {\n        ok(x) -> x\n        ok(y) -> y\n        err(e) -> -1\n    }\n    return v\n}\n",
        "match 重复覆盖 ok 臂",
        5, 8,
    );
}

#[test]
fn incompatible_arm_types_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    val i32 v = match r {\n        ok(x) -> x\n        err(e) -> 'str'\n    }\n    return v\n}\n",
        "绑定 'v' 声明类型为 i32: 需要 i32, 实际 string",
        5, 18,
    );
}

#[test]
fn arm_binding_is_val_semantics() {
    assert_law(
        "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    match r {\n        ok(x) -> {\n            x = 2\n            println(x)\n        }\n        err(e) -> println(e)\n    }\n    return 0\n}\n",
        "'x' 是 val 绑定, 不可重新赋值",
        5, 12,
    );
}

// ---------------------------------------------------------------------------
// 负向矩阵 — 构造器与类型流
// ---------------------------------------------------------------------------

#[test]
fn wrong_payload_type_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val result<i32, string> r = ok('x')\n    return 0\n}\n",
        "绑定 'r' 声明类型为 result<i32, string>: 需要 i32, 实际 string",
        2,
        35,
    );
}

#[test]
fn ok_ctor_arity_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val result<i32, string> r = ok()\n    return 0\n}\n",
        "ok 构造恰好接受 1 个参数",
        2,
        34,
    );
}

#[test]
fn result_one_param_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val result<i32> r = ok(1)\n    return 0\n}\n",
        "result 需要 2 个类型参数, 实际 1 个",
        2,
        4,
    );
}

#[test]
fn other_generic_still_rejected() {
    // result/array/iterator 之外的泛型按当前规范拒绝。
    assert_law(
        "func i32 main = () -> {\n    val sender<i32> a = 1\n    return 0\n}\n",
        "泛型类型 sender<i32> 尚未实现",
        2,
        4,
    );
}

// ---------------------------------------------------------------------------
// 负向矩阵 — ? 传播糖合法性
// ---------------------------------------------------------------------------

#[test]
fn propagate_on_non_result_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val i32 x = 5?\n    return x\n}\n",
        "? 只能作用于 result 值, 实际 i32",
        2,
        17,
    );
}

#[test]
fn propagate_error_type_mismatch_rejected() {
    assert_law(
        "func result<i32, bool> other = () -> {\n    return ok(1)\n}\nfunc result<i32, string> f = () -> {\n    val i32 v = other()?\n    return ok(v)\n}\nfunc i32 main = () -> {\n    return 0\n}\n",
        "? 错误类型不匹配: 表达式错误为 bool, 所在函数错误为 string",
        5, 23,
    );
}

#[test]
fn propagate_outside_function_rejected() {
    assert_law(
        "val result<i32, string> g = ok(1)\nval i32 x = g?\nfunc i32 main = () -> {\n    return 0\n}\n",
        "? 需要所在函数返回 result 类型",
        2, 13,
    );
}

#[test]
fn propagate_in_non_result_fn_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val result<i32, string> r = ok(1)\n    val i32 v = r?\n    return v\n}\n",
        "? 需要所在函数返回 result 类型, 实际 i32",
        3, 17,
    );
}

// ---------------------------------------------------------------------------
// 负向矩阵 — 词法
// ---------------------------------------------------------------------------

#[test]
fn bad_escape_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val string s = 'bad \\q esc'\n    return 0\n}\n",
        "未知转义 '\\q' — 支持 \\n \\t \\r \\\\ \\' \\\" \\0 \\$",
        2,
        26,
    );
}
