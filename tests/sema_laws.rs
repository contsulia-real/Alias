//! sema 当前法律测试 — 每项检查一条当前静态语义，负向用例断言中文消息 + 行:列。
//!
//! 测试名称与说明只描述当前语言行为，不保留历史来源和旧实现差异。
//! 列号语义按当前 lexer 的 `span_here` 算法冻结：`max(可视列-1, 1)`。

use alias::{run, AliasError};

fn fail(src: &str) -> AliasError {
    match run(src) {
        Err(e) => e,
        Ok(_) => panic!("应当报错"),
    }
}

#[test]
fn duplicate_binding_in_same_lexical_scope_is_rejected() {
    let error =
        fail("func i32 main = () -> {\n    val i32 x = 1\n    val i32 x = 2\n    return 0\n}\n");
    assert!(error.msg.contains("同一词法作用域不能重复声明绑定 'x'"));
}

#[test]
fn duplicate_parameter_name_is_rejected() {
    let error = fail("func i32 bad = (i32 x, i32 x) -> return x\nfunc i32 main = () -> return 0\n");
    assert!(error.msg.contains("同一参数列表不能重复参数名 'x'"));
}

#[test]
fn duplicate_top_level_binding_or_function_name_is_rejected() {
    let error =
        fail("val i32 item = 1\nfunc i32 item = () -> return 2\nfunc i32 main = () -> return 0\n");
    assert!(error.msg.contains("同一顶层作用域不能重复声明绑定 'item'"));
}

#[test]
fn main_must_be_unique() {
    let error = fail("func i32 main = () -> return 0\nfunc i32 main = () -> return 1\n");
    assert!(error.msg.contains("同一顶层作用域不能重复声明绑定 'main'"));
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
// 绑定、调用与基础运算法律
// ---------------------------------------------------------------------------

#[test]
fn undefined_binding_read() {
    assert_law(
        "\nfunc i32 main = () -> {\n    return y\n}\n",
        "未定义的绑定 'y'",
        3,
        11,
    );
}

#[test]
fn val_reassignment_rejected() {
    assert_law(
        "\nfunc i32 main = () -> {\n    val i32 a = 1;\n    a = 2;\n    return 0\n}\n",
        "'a' 是 val 绑定, 不可重新赋值",
        4,
        4,
    );
}

#[test]
fn assign_target_undefined() {
    assert_law(
        "\nfunc i32 main = () -> {\n    q = 1;\n    return 0\n}\n",
        "赋值目标 'q' 未定义",
        3,
        4,
    );
}

#[test]
fn incdec_non_ident_target() {
    assert_law(
        "\nfunc i32 main = () -> {\n    increase(1)\n    return 0\n}\n",
        "increase 的参数必须是可变绑定名",
        3,
        12,
    );
}

#[test]
fn incdec_val_target() {
    assert_law(
        "\nfunc i32 main = () -> {\n    val i32 a = 1;\n    increase a\n    return 0\n}\n",
        "'a' 是 val 绑定, 不能 increase",
        4,
        13,
    );
}

#[test]
fn incdec_non_numeric_target() {
    assert_law(
        "\nfunc i32 main = () -> {\n    var bool b = true;\n    increase b\n    return 0\n}\n",
        "increase 需要数值类型, 实际 bool",
        4,
        13,
    );
}

#[test]
fn incdec_undefined_target() {
    assert_law(
        "\nfunc i32 main = () -> {\n    increase z\n    return 0\n}\n",
        "'z' 未定义",
        3,
        13,
    );
}

#[test]
fn incdec_arity_not_one() {
    assert_law(
        "\nfunc i32 main = () -> {\n    increase()\n    return 0\n}\n",
        "increase 恰好接受 1 个参数",
        3,
        12,
    );
}

#[test]
fn println_arity_not_one() {
    assert_law(
        "\nfunc i32 main = () -> {\n    println()\n    return 0\n}\n",
        "println 恰好接受 1 个参数",
        3,
        11,
    );
}

#[test]
fn binary_operand_type_mismatch() {
    assert_law(
        "\nfunc i32 main = () -> {\n    return 1 + true\n}\n",
        "运算符 + 不适用于 i32 与 bool",
        3,
        11,
    );
}

#[test]
fn neg_requires_signed_integer_or_float() {
    assert_law(
        "\nfunc i32 main = () -> {\n    return -true\n}\n",
        "取负需要有符号整数或浮点",
        3,
        12,
    );
}

#[test]
fn loop_condition_requires_bool() {
    assert_law(
        "\nfunc i32 main = () -> {\n    while 1 {\n        return 7\n    }\n    return 3\n}\n",
        "while 条件需要 bool, 实际 i32",
        3,
        4,
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
        3,
        12,
    );
}

// ---------------------------------------------------------------------------
// 声明与槽位类型一致性
// ---------------------------------------------------------------------------

#[test]
fn declared_type_vs_initializer_mismatch() {
    assert_law(
        "\nfunc i32 main = () -> {\n    val string s = 1\n    return 0\n}\n",
        "绑定 's' 声明类型为 string: 需要 string, 实际 i32",
        3,
        19,
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
        "return 需要 i32: 需要 i32, 实际 bool",
        2,
        26,
    );
}

#[test]
fn generic_type_shape_rejected() {
    // sender<string> 代表当前未实现的一般泛型。
    assert_law(
        "\nfunc i32 main = () -> {\n    val sender<string> a = 1\n    return 0\n}\n",
        "泛型类型 sender<string> 尚未实现",
        3,
        4,
    );
}

#[test]
fn unknown_type_name_rejected() {
    assert_law(
        "\nfunc i32 main = () -> {\n    val foo x = 1\n    return 0\n}\n",
        "未知类型名 'foo'",
        3,
        4,
    );
}

// ---------------------------------------------------------------------------
// 比较、返回路径与 main 约束
// ---------------------------------------------------------------------------

/// bool 只支持相等/不等比较，不支持有序比较。
#[test]
fn ordered_comparison_on_bool_rejected() {
    assert_law(
        "\nfunc bool main = () -> {\n    return true < false\n}\n",
        "运算符 < 不适用于 bool 与 bool",
        3,
        11,
    );
}

#[test]
fn bool_equality_is_legal() {
    let src = "\nfunc i32 main = () -> {\n    val bool comparison = true == true\n    while comparison == false { return 1 }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn string_ordering_is_legal() {
    let src = "\nfunc i32 main = () -> {\n    val bool comparison = 'a' < 'b'\n    while comparison == false { return 1 }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

/// 非 unit 函数的所有可达路径都必须显式 return；块体落空非法。
#[test]
fn non_unit_block_fall_off_rejected() {
    assert_law(
        "\nfunc i32 f = () -> {\n    val i32 a = 1\n}\nfunc i32 main = () -> {\n    return 0\n}\n",
        "返回类型为 i32 的函数所有可达路径都必须显式 return",
        2,
        13,
    );
}

/// 循环本身不用于证明非 unit 函数必返回。
#[test]
fn loop_tail_does_not_prove_non_unit_return() {
    assert_law(
        "\nfunc i32 main = () -> {\n    var i32 i = 0\n    while i < 2 {\n        increase i\n    }\n}\n",
        "返回类型为 i32 的函数所有可达路径都必须显式 return",
        2, 16,
    );
}

#[test]
fn main_rejects_parameters() {
    assert_law(
        "\nfunc i32 main = (i32 a) -> return a\n",
        "顶层 func main 不能声明参数",
        2,
        1,
    );
}

#[test]
fn non_i32_main_rejected() {
    for (ty, value, actual) in [("bool", "true", "bool"), ("string", "'hi'", "string")] {
        let src = format!("\nfunc {ty} main = () -> return {value}\n");
        let e = fail(&src);
        assert_eq!(
            e.msg,
            format!("顶层 func main 返回类型必须是 i32, 实际 {actual}")
        );
        assert_eq!((e.span.line, e.span.col), (2, 1));
    }
    let e = fail("\nfunc unit main = () -> return\n");
    assert_eq!(e.msg, "顶层 func main 返回类型必须是 i32, 实际 unit");
    assert_eq!((e.span.line, e.span.col), (2, 1));
}

/// 缺少 main 属于无具体源码位置的诊断，因此 Display 不带位置前缀。
#[test]
fn missing_main_has_no_location_prefix() {
    let e = fail("val i32 x = 1\n");
    assert_eq!(e.msg, "找不到顶层 func main");
    assert!(
        !e.to_string().contains("错误 @"),
        "default Span 不得带位置前缀, 实际: {}",
        e
    );
}

/// 参数是不可重新绑定的隐式 val。
#[test]
fn parameter_assignment_rejected() {
    assert_law(
        "\nfunc i32 f = (i32 p) -> {\n    p = 1;\n    return p\n}\nfunc i32 main = () -> {\n    return f(1)\n}\n",
        "'p' 是 val 绑定, 不可重新赋值",
        3, 4,
    );
}
