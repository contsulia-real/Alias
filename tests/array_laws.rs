//! array<T> 当前法律测试 — 正负矩阵，负向断言精确中文消息 + 行:列。
//!
//! 当前语义锚点见 `docs/spec-notes.md`：
//! - 字面量 [e1, e2, ...] 元素类型一致；类型槽 array<T> 恰一参；
//! - owning binding 的稳定 Place 读取创建独立 array wrapper/backing；
//! - 下标读带越界守卫 (i<0 或 i>=len → span-ID 中止存根, exit 1)；
//!   下标赋值当前未支持并显式拒绝；
//! - 内建 len/push/pop/iterator 由编译器提供；push/pop 推进所属 wrapper 的结构版本，
//!   pop 空数组运行时中止。
//!
//! 列号语义按当前 lexer Span 算法冻结。运行时中止只能由已编译进程产生；
//! 这里走 CLI 子进程逐字节锁定用户可见 stderr 与退出码。

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
// 正向矩阵 — 字面量 / 下标 / 增长 / LIFO / ownership / 嵌套 / 结构体元素
// ---------------------------------------------------------------------------

/// 字面量构造 + 下标读往返。
#[test]
fn literal_index_roundtrip() {
    let src = "func i32 main = () -> {\n    val array<i32> xs = [3, 14, 15]\n    return xs[0] + xs[1] + xs[2]\n}\n";
    assert_eq!(run(src).unwrap(), 32);
}

/// push 越初始容量增长: 初始 len=cap=1, 推到 6 必经换缓冲复制路径,
/// 全部下标可读回。
#[test]
fn push_grows_across_capacity_boundary() {
    let src = "func i32 main = () -> {\n    var array<i32> a = [7]\n    a.push(1)\n    a.push(2)\n    a.push(3)\n    a.push(4)\n    a.push(5)\n    val i32 sum = a[0] + a[1] + a[2] + a[3] + a[4] + a[5]\n    while a.len() != 6 {\n        return 9\n    }\n    return sum\n}\n";
    assert_eq!(run(src).unwrap(), 22);
}

/// pop LIFO: 后进先出; 空字面量 [] 从 cap=0 起步增长 (cap 0→1→2 路径)。
#[test]
fn pop_lifo_order() {
    let src = "func i32 main = () -> {\n    var array<i32> a = []\n    a.push(1)\n    a.push(2)\n    a.push(3)\n    val i32 p1 = a.pop()\n    val i32 p2 = a.pop()\n    return p1 * 10 + p2 + a.len()\n}\n";
    // p1=3, p2=2, len=1 → 30+2+1
    assert_eq!(run(src).unwrap(), 33);
}

/// 嵌套数组: 外层元素仍是数组, 双重下标与内层 len 均可达。
#[test]
fn nested_arrays() {
    let src = "func i32 main = () -> {\n    val array<array<i32>> g = [[1, 2], [3, 4, 5]]\n    val i32 deep = g[1][2]\n    val i32 inner_len = g[0].len()\n    return deep * 10 + inner_len\n}\n";
    assert_eq!(run(src).unwrap(), 52);
}

/// 数组元素为结构体: 从下标 Place 读入 owning binding 时递归 clone 元素。
#[test]
fn array_of_struct_field_access() {
    let src = "struct cell {\n    var i32 v = 0\n}\nfunc i32 main = () -> {\n    val array<cell> cs = [cell(), cell(v = 5)]\n    cs[1].v = 50\n    val cell alias = cs[1]\n    alias.v = alias.v + 1\n    return cs[1].v + cs[0].v\n}\n";
    assert_eq!(run(src).unwrap(), 50);
}

/// array 的 owning binding 读取创建独立 wrapper/backing。
#[test]
fn binding_read_deep_clones_array() {
    let src = "func i32 main = () -> {\n    var array<i32> a = [1]\n    val array<i32> b = a\n    b.push(9)\n    a.push(8)\n    return a.len() * 10 + b.len() + a[1] + b[1]\n}\n";
    assert_eq!(run(src).unwrap(), 39);
}

/// val 绑定上的 push 合法 (变异在实例不在绑定); 字符串元素 +
/// 下标结果上的内建方法分派。
#[test]
fn string_elements_and_val_binding_push() {
    let src = "func i32 main = () -> {\n    val array<string> w = ['ab', 'c']\n    w.push('忠犬')\n    return w.len() * 100 + w[2].len()\n}\n";
    // 忠犬 = 6 字节
    assert_eq!(run(src).unwrap(), 306);
}

/// 闭包引用捕获数组绑定: 捕获后 push, 闭包内读到最新长度 (引用捕获)。
#[test]
fn closure_captures_array_by_reference() {
    let src = "func i32 main = () -> {\n    var array<i32> a = [1]\n    func i32 reader = () -> return a.len() * 10 + a[0]\n    a.push(5)\n    return reader()\n}\n";
    assert_eq!(run(src).unwrap(), 21);
}

/// 插值洞内的下标表达式同通道。
#[test]
fn index_inside_interpolation_hole() {
    let src = "func i32 main = () -> {\n    val array<i32> xs = [4, 9]\n    while '${xs[1]}' != '9' {\n        return 1\n    }\n    return xs[0]\n}\n";
    assert_eq!(run(src).unwrap(), 4);
}

// ---------------------------------------------------------------------------
// 负向矩阵 — 每项检查一条, 断言精确中文消息 + 行:列
// ---------------------------------------------------------------------------

/// 字面量元素类型不一致 (bool 混入 i32)。
#[test]
fn element_type_mismatch_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val array<i32> a = [1, true]\n    return 0\n}\n",
        "数组元素类型不一致: i32 与 bool",
        2,
        27,
    );
}

/// 三元素混合字面量 — 首个违规元素即报。
#[test]
fn mixed_literal_rejected_at_first_offender() {
    assert_law(
        "func i32 main = () -> {\n    val array<i32> a = [1, 'x', 2]\n    return 0\n}\n",
        "数组元素类型不一致: i32 与 string",
        2,
        27,
    );
}

/// 非数组主语的下标访问。
#[test]
fn index_on_non_array_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val i32 x = 5\n    return x[0]\n}\n",
        "下标访问需要 array 类型, 实际 i32",
        3,
        11,
    );
}

/// 非 i32 下标。
#[test]
fn non_i32_index_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val array<i32> a = [1]\n    return a[true]\n}\n",
        "下标需要 i32, 实际 bool",
        3,
        13,
    );
}

/// push 实参类型须等于元素类型。
#[test]
fn push_wrong_element_type_rejected() {
    assert_law(
        "func i32 main = () -> {\n    var array<i32> a = [1]\n    a.push(true)\n    return 0\n}\n",
        "第 1 个实参需要 i32, 实际 bool",
        3,
        11,
    );
}

/// 下标赋值当前未支持并显式拒绝。
#[test]
fn index_assignment_rejected() {
    assert_law(
        "func i32 main = () -> {\n    var array<i32> a = [1]\n    a[0] = 2\n    return 0\n}\n",
        "下标赋值尚未支持",
        3,
        9,
    );
}

/// 数组上查无此方法。
#[test]
fn unknown_method_on_array_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val array<i32> a = [1]\n    return a.nope()\n}\n",
        "类型 array<i32> 上没有方法 'nope'",
        3,
        12,
    );
}

/// array<> 零参 — 语法层拒绝 (类型参数至少一个)。
#[test]
fn array_zero_params_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val array<> a = 1\n    return 0\n}\n",
        "期望类型名",
        2,
        14,
    );
}

/// array<i32, string> 两参 — sema 元数拒绝。
#[test]
fn array_two_params_rejected() {
    assert_law(
        "func i32 main = () -> {\n    val array<i32, string> a = 1\n    return 0\n}\n",
        "array 需要 1 个类型参数, 实际 2 个",
        2,
        4,
    );
}

// ---------------------------------------------------------------------------
// 运行时中止 — span-ID 存根 exit 1, 仅子进程可观测
// ---------------------------------------------------------------------------

fn cli_run(src: &str) -> (Vec<u8>, Vec<u8>, Option<i32>) {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    // 序列号只用于同一测试进程内生成不重复文件名，不发布任何跨线程状态；
    // 因此不需要 happens-before 关系，Relaxed 足够。
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("alias-arr-laws-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    let p = dir.join(format!("case-{n}.as"));
    std::fs::write(&p, src).expect("写入临时源文件失败");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_alias"))
        .arg(&p)
        .output()
        .expect("启动 alias 二进制失败");
    let _ = std::fs::remove_file(&p);
    (out.stdout, out.stderr, out.status.code())
}

/// 越界读中止: span 取 '[' 记账点, 消息/退出码冻结。
#[test]
fn bounds_abort_via_cli() {
    let src = "func i32 main = () -> {\n    val array<i32> a = [1, 2, 3]\n    println a[5]\n    return 0\n}\n";
    let (so, se, code) = cli_run(src);
    assert_eq!(so, b"");
    assert_eq!(se, "错误 @ 3:13 — 下标越界\n".as_bytes());
    assert_eq!(code, Some(1));
}

/// 负下标同样越界 (i < 0 守卫分支)。
#[test]
fn negative_index_abort_via_cli() {
    let src = "func i32 main = () -> {\n    val array<i32> a = [1]\n    val i32 i = 0 - 1\n    println a[i]\n    return 0\n}\n";
    let (_so, se, code) = cli_run(src);
    assert_eq!(se, "错误 @ 4:13 — 下标越界\n".as_bytes());
    assert_eq!(code, Some(1));
}

/// pop 空数组中止: span 取方法调用 '.' 记账点。
#[test]
fn pop_empty_abort_via_cli() {
    let src = "func i32 main = () -> {\n    val array<i32> a = []\n    return a.pop()\n}\n";
    let (so, se, code) = cli_run(src);
    assert_eq!(so, b"");
    assert_eq!(se, "错误 @ 3:12 — pop 空数组\n".as_bytes());
    assert_eq!(code, Some(1));
}
