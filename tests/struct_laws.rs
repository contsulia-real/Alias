//! struct 当前法律测试 — 正负矩阵，负向断言精确中文消息 + 行:列。
//!
//! 语义锚点 (用户批准设计, spec-notes 附录三):
//! - 引用语义: 实例 = 泄漏堆块, 变量持指针 — 别名/传参/闭包捕获共享实例
//! - 字段级可变性: var 字段可写与绑定自身 val/var 无关
//! - 单一命名空间: 结构体名与 func/绑定重名即编译错误
//! - 构造全命名: 缺字段/重复/未知/类型不符各有独立诊断
//!
//! 列号语义: token span col = max(可视列-1, 1) (lexer.rs span_here)。
//!
//! allow: SIZE_OK — 法律表为纯数据矩阵 (项目先例 sema_laws.rs 同注)。

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
// 负向矩阵 — 构造检查
// ---------------------------------------------------------------------------

#[test]
fn ctor_missing_required_field() {
    assert_law(
        "struct stat {\n    val i32 lines = 1\n    var i32 bytes\n}\nfunc i32 main = () -> {\n    val stat s = stat(lines = 2)\n    return 0\n}\n",
        "结构体 stat 构造缺少字段 'bytes'",
        6, 21,
    );
}

#[test]
fn ctor_unknown_field() {
    assert_law(
        "struct stat {\n    val i32 lines = 1\n    var i32 bytes\n}\nfunc i32 main = () -> {\n    val stat s = stat(lines = 2, bytes = 3, bogus = 4)\n    return 0\n}\n",
        "结构体 stat 没有字段 'bogus'",
        6, 44,
    );
}

#[test]
fn ctor_duplicate_field() {
    assert_law(
        "struct stat {\n    val i32 lines = 1\n    var i32 bytes\n}\nfunc i32 main = () -> {\n    val stat s = stat(lines = 2, lines = 3, bytes = 4)\n    return 0\n}\n",
        "结构体 stat 构造重复指定字段 'lines'",
        6, 33,
    );
}

#[test]
fn ctor_field_type_mismatch() {
    assert_law(
        "struct stat {\n    val i32 lines = 1\n    var i32 bytes\n}\nfunc i32 main = () -> {\n    val stat s = stat(lines = 'x', bytes = 2)\n    return 0\n}\n",
        "字段 'lines' 需要 i32, 实际 string",
        6, 30,
    );
}

#[test]
fn ctor_unlabeled_arg_rejected() {
    assert_law(
        "struct stat {\n    val i32 lines = 1\n    var i32 bytes\n}\nfunc i32 main = () -> {\n    val stat s = stat(5)\n    return 0\n}\n",
        "结构体 stat 构造必须使用命名实参",
        6, 22,
    );
}

#[test]
fn func_call_labeled_arg_rejected() {
    assert_law(
        "func i32 add = (i32 a, i32 b) -> return a + b\nfunc i32 main = () -> {\n    return add(1, b = 2)\n}\n",
        "函数调用不接受命名实参 'b'",
        3, 18,
    );
}

// ---------------------------------------------------------------------------
// 负向矩阵 — 字段访问 / 字段赋值
// ---------------------------------------------------------------------------

#[test]
fn val_field_assign_rejected() {
    assert_law(
        "struct stat {\n    val i32 lines = 1\n    var i32 bytes\n}\nfunc i32 main = () -> {\n    val stat s = stat(bytes = 2)\n    s.lines = 5\n    return 0\n}\n",
        "'lines' 是 val 字段, 不可赋值",
        7, 4,
    );
}

#[test]
fn unknown_field_assign_rejected() {
    assert_law(
        "struct stat {\n    val i32 lines = 1\n    var i32 bytes\n}\nfunc i32 main = () -> {\n    val stat s = stat(bytes = 2)\n    s.bogus = 5\n    return 0\n}\n",
        "结构体 stat 没有字段 'bogus'",
        7, 4,
    );
}

#[test]
fn unknown_field_read_rejected() {
    assert_law(
        "struct stat { val i32 lines = 1 }\nfunc i32 main = () -> {\n    val stat s = stat()\n    return s.bogus\n}\n",
        "结构体 stat 没有字段 'bogus'",
        4, 12,
    );
}

#[test]
fn field_access_on_non_struct_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val i32 x = 1\n    return x.foo\n}\n",
        "i32 没有字段 'foo'",
        3,
        12,
    );
}

// ---------------------------------------------------------------------------
// 负向矩阵 — 单一命名空间
// ---------------------------------------------------------------------------

#[test]
fn binding_clashes_with_struct() {
    assert_law(
        "struct foo {\n    val i32 x = 1\n}\nfunc i32 foo = () -> return 1\nfunc i32 main = () -> return 0\n",
        "'foo' 已定义为结构体, 不能再定义为绑定",
        4, 1,
    );
}

#[test]
fn struct_clashes_with_binding() {
    assert_law(
        "val i32 n = 1\nstruct n {\n    val i32 x = 1\n}\nfunc i32 main = () -> return 0\n",
        "'n' 已定义为绑定, 不能再定义为结构体",
        2,
        1,
    );
}

#[test]
fn duplicate_struct_rejected() {
    assert_law(
        "struct stat {\n    val i32 x = 1\n}\nstruct stat {\n    val i32 y = 1\n}\nfunc i32 main = () -> return 0\n",
        "'stat' 已定义为结构体, 不能重复定义",
        4, 1,
    );
}

#[test]
fn default_type_mismatch_rejected() {
    assert_law(
        "struct bad {\n    val i32 x = 'no'\n}\nfunc i32 main = () -> return 0\n",
        "字段 'x' 声明类型为 i32, 实际 string",
        2,
        16,
    );
}

#[test]
fn unknown_struct_in_type_slot() {
    // 既有法律不变: 类型槽按冻结类型集 + 结构体表收紧
    assert_law(
        "func i32 main = () -> {\n    val nosuch s = 1\n    return 0\n}\n",
        "未知类型名 'nosuch'",
        2,
        4,
    );
}

#[test]
fn forward_struct_reference_rejected() {
    // 声明前不可见 — 与绑定同序 (insert-after-eval 镜像)
    assert_law(
        "func i32 main = () -> {\n    val later s = 1\n    return 0\n}\nstruct later {\n    val i32 x = 1\n}\n",
        "未知类型名 'later'",
        2, 4,
    );
}

// ---------------------------------------------------------------------------
// 正向矩阵 — 引用语义 / 默认值 / 嵌套 / 闭包捕获
// ---------------------------------------------------------------------------

/// 引用别名: 两个名字一个实例 — 经 b 改, a 可见 (值语义下返回 0)。
#[test]
fn reference_aliasing_two_names_one_instance() {
    let src = "struct box {\n    var i32 v = 0\n}\nfunc i32 main = () -> {\n    val box a = box()\n    val box b = a\n    b.v = 42\n    return a.v\n}\n";
    assert_eq!(run(src).unwrap(), 42);
}

/// 全默认构造 + 嵌套读取。
#[test]
fn all_default_construction_and_read() {
    let src = "struct point {\n    val i32 x = 3\n    val i32 y = 4\n}\nfunc i32 main = () -> {\n    val point p = point()\n    return p.x * p.y\n}\n";
    assert_eq!(run(src).unwrap(), 12);
}

/// 乱序命名构造 + 显式覆盖默认 + var 字段变异 (val 绑定上)。
#[test]
fn out_of_order_ctor_and_var_field_mutation() {
    let src = "struct inner {\n    val i32 k = 7\n}\nstruct outer {\n    val inner i = inner(k = 9)\n    var i32 m\n}\nfunc i32 main = () -> {\n    val outer o = outer(m = 1)\n    o.m = o.i.k + 30\n    return o.m\n}\n";
    assert_eq!(run(src).unwrap(), 39);
}

/// 传参即共享: 函数内字段改动调用方可见 (值语义下返回 0)。
#[test]
fn param_passing_shares_instance() {
    let src = "struct bag {\n    var i32 n = 0\n}\nfunc i32 touch = (bag b) -> {\n    b.n = b.n + 5\n    return b.n\n}\nfunc i32 main = () -> {\n    val bag x = bag()\n    val i32 r = touch(x)\n    return x.n\n}\n";
    assert_eq!(run(src).unwrap(), 5);
}

/// 闭包引用捕获结构体: 每次调用读到累积最新值 (second - first = 30;
/// 若捕获为副本则两次都从 0 起, 差为 0)。别名再证同一实例。
#[test]
fn closure_capture_reads_latest_field_value() {
    let src = "struct cell {\n    var i32 v = 0\n}\nfunc i32 main = () -> {\n    val cell c = cell()\n    func i32 bump = () -> {\n        c.v = c.v + 30\n        return c.v\n    }\n    val i32 first = bump()\n    val i32 second = bump()\n    val cell alias = c\n    alias.v = 100\n    return second - first + c.v - 100\n}\n";
    assert_eq!(run(src).unwrap(), 30);
}

/// func 绑定类型槽 = 结构体名; 返回结构体的函数。
#[test]
fn struct_returning_function() {
    let src = "struct pair {\n    val i32 a = 0\n    val i32 b = 0\n}\nfunc pair mk = (i32 v) -> return pair(a = v, b = v * 2)\nfunc i32 main = () -> {\n    val pair p = mk(21)\n    return p.a + p.b\n}\n";
    assert_eq!(run(src).unwrap(), 63);
}
