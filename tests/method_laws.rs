//! 扩展方法当前法律测试 — 正负矩阵，负向断言精确中文消息 + 行:列。
//!
//! 当前语义锚点：
//! - 方法定义文法: pub? func <Ret> <RecvType>.<name> = (params) -> 体
//! - self 是隐式 val 绑定 (不在参数表), 类型 = 接收者类型
//! - 方法名按接收者类型划分命名空间; 内建 len/upper/lower/trim 不可覆盖
//! - 调用点静态分派: 接收者推断类型 → 方法表; 返回类型流入推断
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
// 正向矩阵 — 分派 / self / 引用语义 / 链式 / 内建 / 命名空间
// ---------------------------------------------------------------------------

/// 基本分派: 用户扩展方法在字符串上生效, 实参经 self 拼接。
#[test]
fn basic_dispatch_on_string() {
    let src = "func string string.append = (string tail) -> return '${self}${tail}'\nfunc i32 main = () -> {\n    val string s = '忠'\n    return s.append('犬').len()\n}\n";
    // 忠(3 字节) + 犬(3 字节) = 6
    assert_eq!(run(src).unwrap(), 6);
}

/// self 只读用法: 无参方法读 self 本体与字段。
#[test]
fn self_read_in_method_body() {
    let src = "struct box {\n    var i32 v = 7\n}\nfunc i32 box.get = () -> return self.v\nfunc i32 main = () -> {\n    val box b = box()\n    return b.get()\n}\n";
    assert_eq!(run(src).unwrap(), 7);
}

/// 结构体方法经 self 写 var 字段；由 owning binding 读取形成的两个实例独立。
#[test]
fn struct_method_mutates_only_the_selected_owner() {
    let src = "struct counter {\n    var i32 n = 0\n}\nfunc i32 counter.bump = (i32 by) -> {\n    self.n = self.n + by\n    return self.n\n}\nfunc i32 main = () -> {\n    val counter c = counter()\n    val counter alias = c\n    val i32 first = c.bump(5)\n    val i32 second = alias.bump(2)\n    return first * 10 + second + c.n\n}\n";
    // first=5, second=2 (独立副本), c.n=5 → 50+2+5
    assert_eq!(run(src).unwrap(), 57);
}

/// 链式调用: 返回类型逐级流入静态投影 — upper→lower→len 全程值流动。
#[test]
fn method_chaining_flows() {
    let src = "func i32 main = () -> {\n    return 'aBc'.upper().lower().len()\n}\n";
    assert_eq!(run(src).unwrap(), 3);
}

/// 内建四件套往返: len 字节长 / ASCII 大小写 / trim 双边界。
#[test]
fn builtin_string_methods_roundtrip() {
    let src = "func i32 main = () -> {\n    val i32 a = 'héllo'.upper().len()\n    val bool b = 'MiXeD'.lower() == 'mixed'\n    val bool c = '  hi '.trim() == 'hi'\n    while b == false {\n        return 9\n    }\n    while c == false {\n        return 8\n    }\n    return a\n}\n";
    // héllo = 6 字节 (é 双字节); upper 不改非 ASCII 字母 → 长度不变
    assert_eq!(run(src).unwrap(), 6);
}

/// trim 边界: 全空白 → 空串; 无空白 → 原样; 仅单侧空白。
#[test]
fn builtin_trim_edge_cases() {
    let src = "func i32 main = () -> {\n    val bool all_ws = '[${' \\t\\r\\n '.trim()}]' == '[]'\n    val bool none = 'plain'.trim() == 'plain'\n    val bool left = '  x'.trim() == 'x'\n    val bool right = 'x  '.trim() == 'x'\n    while all_ws == false {\n        return 1\n    }\n    while none == false {\n        return 2\n    }\n    while left == false {\n        return 3\n    }\n    while right == false {\n        return 4\n    }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

/// 同名方法跨类型共存: string.twice 与 counter.twice 互不干扰;
/// 裸函数 twice 亦独立 (三重命名空间)。
#[test]
fn same_method_name_across_types() {
    let src = "struct counter {\n    var i32 n = 1\n}\nfunc string string.twice = () -> return '${self}${self}'\nfunc i32 counter.twice = () -> {\n    self.n = self.n * 2\n    return self.n\n}\nfunc i32 twice = (i32 v) -> return v * 2\nfunc i32 main = () -> {\n    val counter c = counter()\n    val i32 a = c.twice()\n    val i32 bare = twice(a)\n    while 'ab'.twice() != 'abab' {\n        return 1\n    }\n    return bare\n}\n";
    // c.n: 1→2; bare = 4
    assert_eq!(run(src).unwrap(), 4);
}

/// pub 是顶层声明修饰语；同一编译单元内的方法可调用性不因 pub 改变。
#[test]
fn pub_methods_callable_in_same_unit() {
    let src = "pub func string string.shout = () -> return '${self}!'\nfunc i32 main = () -> {\n    while 'hey'.shout() != 'hey!' {\n        return 1\n    }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

/// 非 pub 方法在同一编译单元内同样可调。
#[test]
fn private_methods_callable_in_same_unit() {
    let src = "func string string.whisper = () -> return '${self}'\nfunc i32 main = () -> {\n    while 'ss'.whisper() != 'ss' {\n        return 1\n    }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

/// 方法体内插值洞可用 self ($self 与 ${self} 同通道)。
#[test]
fn self_inside_interpolation_hole() {
    let src = "func string string.wrap = () -> return '[$self]'\nfunc i32 main = () -> {\n    while 'x'.wrap() != '[x]' {\n        return 1\n    }\n    return 0\n}\n";
    assert_eq!(run(src).unwrap(), 0);
}

/// 方法返回类型流入推断: 结果直接参与算术与比较。
#[test]
fn method_ret_type_flows_into_inference() {
    let src = "func i32 i32.square = () -> return self * self\nfunc i32 main = () -> {\n    val i32 v = 6.square() + 1\n    return v\n}\n";
    assert_eq!(run(src).unwrap(), 37);
}

// ---------------------------------------------------------------------------
// 负向矩阵 — 每项检查一条, 断言精确中文消息 + 行:列
// ---------------------------------------------------------------------------

/// 未知方法: 接收者类型上查无此名。
#[test]
fn unknown_method_rejected() {
    assert_law(
        "func i32 main = () -> {\n    return 'x'.nope()\n}\n",
        "类型 string 上没有方法 'nope'",
        2,
        14,
    );
}

/// 结构体有字段无方法 — 字段与方法命名空间独立, 不得互串。
#[test]
fn field_name_is_not_method() {
    assert_law(
        "struct box {\n    val i32 v = 1\n}\nfunc i32 main = () -> {\n    val box b = box()\n    return b.v()\n}\n",
        "类型 box 上没有方法 'v'",
        6, 12,
    );
}

/// 方法定义在未知类型上。
#[test]
fn method_on_unknown_type_rejected() {
    assert_law(
        "func i32 ghost.m = () -> return 0\nfunc i32 main = () -> return 0\n",
        "未知类型名 'ghost'",
        1,
        1,
    );
}

/// unit 是无返回值标记，不属于接收者的值类型域。
#[test]
fn unit_receiver_rejected() {
    assert_law(
        "func i32 unit.m = () -> return 0\nfunc i32 main = () -> return 0\n",
        "unit 只能作为函数返回类型",
        1,
        1,
    );
}

/// 元数不符 (self 不计入实参)。
#[test]
fn method_arity_mismatch_rejected() {
    assert_law(
        "func string string.cat = (string a, string b) -> return '${self}${a}${b}'\nfunc i32 main = () -> {\n    return 'x'.cat('y')\n}\n",
        "期望 2 个参数, 实际 1 个",
        3, 14,
    );
}

/// 实参类型不符。
#[test]
fn method_arg_type_mismatch_rejected() {
    assert_law(
        "func string string.append = (string t) -> return '${self}${t}'\nfunc i32 main = () -> {\n    return 'x'.append(1)\n}\n",
        "第 1 个实参需要 string, 实际 i32",
        3, 22,
    );
}

/// self 是隐式 val，不能重新绑定。
#[test]
fn self_rebinding_rejected() {
    assert_law(
        "func string string.set = (string v) -> {\n    self = v\n    return self\n}\nfunc i32 main = () -> {\n    return 'x'.set('y').len()\n}\n",
        "'self' 是 val 绑定, 不可重新赋值",
        2, 4,
    );
}

/// increase 同样不能修改隐式 val self。
#[test]
fn self_increase_rejected() {
    assert_law(
        "struct c {\n    var i32 n = 0\n}\nfunc i32 c.bad = () -> {\n    increase self\n    return self.n\n}\nfunc i32 main = () -> {\n    val c x = c()\n    return x.bad()\n}\n",
        "'self' 是 val 绑定, 不能 increase",
        5, 13,
    );
}

/// 内建方法不可覆盖。
#[test]
fn builtin_override_rejected() {
    assert_law(
        "func i32 string.len = () -> return 0\nfunc i32 main = () -> return 0\n",
        "内建方法不可覆盖: string.len",
        1,
        1,
    );
}

/// 同型同名重复定义。
#[test]
fn duplicate_method_same_type_rejected() {
    assert_law(
        "func string string.append = (string t) -> return t\nfunc string string.append = (string t) -> return t\nfunc i32 main = () -> return 0\n",
        "类型 string 上已定义方法 'append'",
        2, 1,
    );
}

/// 跨类型同名不算重复；正向用例 same_method_name_across_types 锁定该规则。
/// 方法按裸函数名调用 — 方法不入绑定命名空间。
#[test]
fn method_called_as_bare_function_rejected() {
    assert_law(
        "func string string.shout = () -> return 'x'\nfunc i32 main = () -> {\n    return shout()\n}\n",
        "未定义的绑定 'shout'",
        3, 11,
    );
}

/// self 只存在于方法体内；其它作用域按普通未定义名字处理。
#[test]
fn self_outside_method_rejected() {
    assert_law(
        "func i32 main = () -> {\n    return self\n}\n",
        "未定义的绑定 'self'",
        2,
        11,
    );
}

/// 方法调用不接受命名实参。
#[test]
fn method_labeled_arg_rejected() {
    assert_law(
        "func string string.append = (string t) -> return t\nfunc i32 main = () -> {\n    return 'x'.append(t = 'y')\n}\n",
        "方法调用不接受命名实参 't'",
        3, 22,
    );
}

/// 方法定义只能在顶层。
#[test]
fn nested_method_def_rejected() {
    assert_law(
        "func i32 main = () -> {\n    func string string.f = () -> return ''\n    return 0\n}\n",
        "方法定义只能出现在顶层",
        2,
        4,
    );
}

/// 方法体必须是函数字面量。
#[test]
fn method_body_must_be_funclit() {
    assert_law(
        "func string string.f = 42\nfunc i32 main = () -> return 0\n",
        "方法 string.f 的体必须是函数字面量",
        1,
        1,
    );
}
