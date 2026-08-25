//! Phase 1 sema 法律测试 — 每项检查一条负向用例, 断言精确中文消息 + 行:列。
//!
//! 消息逐字节对齐两个来源:
//! - 运行时搬家项: interp.rs 原报错文本 (D4 律: 迁移消息字节精确)
//! - 新发明项: D3 一致性矩阵与 Q①③④ 收紧 (见 MIGRATION.md 各条目)
//!
//! 列号语义: token span col = max(可视列-1, 1) (lexer.rs span_here)。

use alias::{run, AliasError};

fn fail(src: &str) -> AliasError {
    match run("test.as", src) {
        Err(e) => e,
        Ok(_) => panic!("应当报错"),
    }
}

fn assert_law(src: &str, want_sub: &str, line: u32, col: u32) {
    let e = fail(src);
    assert!(
        e.msg.contains(want_sub),
        "消息应含「{want_sub}」, 实际: {}", e.msg
    );
    assert_eq!(
        (e.span.line, e.span.col),
        (line, col),
        "span 不符: 实际 {}:{} — {}",
        e.span.line, e.span.col, e.msg
    );
}

// ---------------------------------------------------------------------------
// 运行时搬家项 — 消息与 span 逐字节保留
// ---------------------------------------------------------------------------

#[test]
fn undefined_binding_read() {
    assert_law(
        "\nfunc i32 main = () -> {\n    return y\n}\n",
        "未定义的绑定 'y'",
        3, 11,
    );
}

#[test]
fn val_reassignment_moved_to_sema() {
    assert_law(
        "\nfunc i32 main = () -> {\n    val i32 a = 1;\n    a = 2;\n    return 0\n}\n",
        "'a' 是 val 绑定, 不可重新赋值",
        4, 4,
    );
}

#[test]
fn assign_target_undefined() {
    assert_law(
        "\nfunc i32 main = () -> {\n    q = 1;\n    return 0\n}\n",
        "赋值目标 'q' 未定义",
        3, 4,
    );
}

#[test]
fn incdec_non_ident_target() {
    assert_law(
        "\nfunc i32 main = () -> {\n    increase(1)\n    return 0\n}\n",
        "increase 的参数必须是可变绑定名",
        3, 12,
    );
}

#[test]
fn incdec_val_target() {
    assert_law(
        "\nfunc i32 main = () -> {\n    val i32 a = 1;\n    increase a\n    return 0\n}\n",
        "'a' 是 val 绑定, 不能 increase",
        4, 13,
    );
}

#[test]
fn incdec_non_i32_target() {
    assert_law(
        "\nfunc i32 main = () -> {\n    var bool b = true;\n    increase b\n    return 0\n}\n",
        "increase 需要 i32, 实际 bool",
        4, 13,
    );
}

#[test]
fn incdec_undefined_target() {
    assert_law(
        "\nfunc i32 main = () -> {\n    increase z\n    return 0\n}\n",
        "'z' 未定义",
        3, 13,
    );
}

#[test]
fn incdec_arity_not_one() {
    assert_law(
        "\nfunc i32 main = () -> {\n    increase()\n    return 0\n}\n",
        "increase 恰好接受 1 个参数",
        3, 12,
    );
}

#[test]
fn println_arity_not_one() {
    assert_law(
        "\nfunc i32 main = () -> {\n    println()\n    return 0\n}\n",
        "println 恰好接受 1 个参数",
        3, 11,
    );
}

#[test]
fn binary_operand_type_mismatch() {
    assert_law(
        "\nfunc i32 main = () -> {\n    return 1 + true\n}\n",
        "运算符 + 不适用于 i32 与 bool",
        3, 11,
    );
}

#[test]
fn neg_requires_i32() {
    assert_law(
        "\nfunc i32 main = () -> {\n    return -true\n}\n",
        "取负需要 i32, 实际 bool",
        3, 12,
    );
}

#[test]
fn loop_condition_requires_bool() {
    assert_law(
        "\nfunc i32 main = () -> {\n    while 1 {\n        return 7\n    }\n    return 3\n}\n",
        "while 条件需要 bool, 实际 i32",
        3, 4,
    );
}

#[test]
fn call_arity_mismatch() {
    assert_law(
        "\nfunc i32 add = (i32 a, i32 b) -> return a + b\nfunc i32 main = () -> {\n    return add(1)\n}\n",
        "期望 2 个参数, 实际 1 个",
        4, 14,
    );
}

#[test]
fn calling_non_function_value() {
    assert_law(
        "\nfunc i32 main = () -> {\n    return 5()\n}\n",
        "i32 不是可调用值",
        3, 12,
    );
}

// ---------------------------------------------------------------------------
// D3 新发明: 声明类型一致性矩阵
// ---------------------------------------------------------------------------

#[test]
fn declared_type_vs_initializer_mismatch() {
    assert_law(
        "\nfunc i32 main = () -> {\n    val string s = 1\n    return 0\n}\n",
        "绑定 's' 声明类型为 string, 实际 i32",
        3, 4,
    );
}

#[test]
fn argument_type_vs_param_mismatch() {
    assert_law(
        "\nfunc i32 add = (i32 a, i32 b) -> return a + b\nfunc i32 main = () -> {\n    return add(1, true)\n}\n",
        "第 2 个实参需要 i32, 实际 bool",
        4, 18,
    );
}

#[test]
fn return_expr_vs_declared_mismatch() {
    assert_law(
        "\nfunc i32 f = () -> return true\nfunc i32 main = () -> {\n    return 0\n}\n",
        "return 需要 i32, 实际 bool",
        2, 26,
    );
}

#[test]
fn generic_type_shape_rejected() {
    assert_law(
        "\nfunc i32 main = () -> {\n    val array<string> a = ()\n    return 0\n}\n",
        "泛型类型 array<string> 尚未实现 (Phase 5+)",
        3, 4,
    );
}

#[test]
fn unknown_type_name_rejected() {
    assert_law(
        "\nfunc i32 main = () -> {\n    val foo x = 1\n    return 0\n}\n",
        "未知类型名 'foo'",
        3, 4,
    );
}

// ---------------------------------------------------------------------------
// 收紧裁决 tightened_* — Q① / Q③ / Q④ / Q②
// ---------------------------------------------------------------------------

/// Q① 裁决: `true < false` 曾静默求值 false (interp.rs:317), 现为编译错误。
#[test]
fn tightened_q1_ordered_comparison_on_bool() {
    assert_law(
        "\nfunc bool main = () -> {\n    return true < false\n}\n",
        "运算符 < 不适用于 bool 与 bool — 有序比较仅限 i32 与 string",
        3, 11,
    );
}

/// Q① 正向控制: EqEq 对 bool 仍合法。
#[test]
fn q1_bool_equality_still_legal() {
    let src = "\nfunc bool main = () -> {\n    return true == true\n}\n";
    assert_eq!(run("t.as", src).unwrap(), 0);
}

/// Q① 正向控制: string 有序比较合法 (运行时字典序语义不变)。
#[test]
fn q1_string_ordering_still_legal() {
    let src = "\nfunc bool main = () -> {\n    return 'a' < 'b'\n}\n";
    assert_eq!(run("t.as", src).unwrap(), 0);
}

/// Q③ 裁决(严格版终裁): 声明返回非 unit 的块体落空曾静默得 Unit
/// (interp.rs:363), 现为编译错误。末条语句必须是 return,
/// 循环收尾不再豁免 (见 spec-notes §三.1 与 MIGRATION.md Q③ 条目)。
#[test]
fn tightened_q3_block_fall_off_rejected() {
    assert_law(
        "\nfunc i32 f = () -> {\n    val i32 a = 1\n}\nfunc i32 main = () -> {\n    return 0\n}\n",
        "返回类型为 i32 的函数体必须以 return 语句收尾",
        2, 13,
    );
}

/// Q③ 严格版负向控制: 循环收尾的 i32 main 同样拒绝 (原驱动尾豁免已废)。
#[test]
fn tightened_q3_loop_tail_rejected() {
    assert_law(
        "\nfunc i32 main = () -> {\n    var i32 i = 0\n    for i < 2 {\n        increase i\n    }\n}\n",
        "返回类型为 i32 的函数体必须以 return 语句收尾",
        2, 16,
    );
}

/// Q④ 裁决: main 零参校验 (曾有参数的 main 在运行时被无参调用)。
#[test]
fn tightened_q4_main_no_params() {
    assert_law(
        "\nfunc i32 main = (i32 a) -> return a\n",
        "顶层 func main 不能声明参数",
        2, 1,
    );
}

/// Q④ 正向控制: string main 合法且静默退 0 (退出映射: string→0 不打印)。
#[test]
fn q4_string_main_exits_zero() {
    let src = "\nfunc string main = () -> return 'hi'\n";
    assert_eq!(run("t.as", src).unwrap(), 0);
}

/// Q⑤ 裁决: 缺 main 时 Display 省略位置前缀 — 锁定新输出形态。
#[test]
fn tightened_q5_missing_main_has_no_location_prefix() {
    let e = fail("val i32 x = 1\n");
    assert_eq!(e.msg, "找不到顶层 func main");
    assert!(
        !e.to_string().contains("错误 @"),
        "Q⑤: default Span 不得带位置前缀, 实际: {}", e
    );
}

/// Q② 裁决: 参数隐式 val — 编译期拒绝对参数赋值 (原为运行时错误)。
#[test]
fn tightened_q2_param_assignment_rejected() {
    assert_law(
        "\nfunc i32 f = (i32 p) -> {\n    p = 1;\n    return p\n}\nfunc i32 main = () -> {\n    return f(1)\n}\n",
        "'p' 是 val 绑定, 不可重新赋值",
        3, 4,
    );
}

// ---------------------------------------------------------------------------
// demo 语料审计 — sema 接线后已知良好夹具必须原样通过
// ---------------------------------------------------------------------------

#[test]
fn audit_count_to_ten_passes_sema() {
    let src = include_str!("../demos/count_to_ten.as");
    assert_eq!(run("count_to_ten.as", src).unwrap(), 0);
}
