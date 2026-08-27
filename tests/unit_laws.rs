//! unit 法律：unit 仅是函数“无返回值”的签名标记，不属于值类型域。

use alias::{run, AliasError};

fn fail(src: &str) -> AliasError {
    match run("unit-law.as", src) {
        Err(error) => error,
        Ok(_) => panic!("应当报错"),
    }
}

#[test]
fn unit_functions_fall_through_or_use_bare_return() {
    let src = r#"
func unit write = (string text) -> {
    println text
}
func unit stop = () -> return
func unit string.write = () -> println self
func i32 main = () -> {
    write('ok')
    stop()
    'method'.write()
    return 0
}
"#;
    assert_eq!(run("unit-law.as", src).unwrap(), 0);
}

#[test]
fn unit_is_forbidden_in_every_value_type_slot() {
    let cases = [
        "val unit value = 1\nfunc i32 main = () -> return 0\n",
        "func i32 take = (unit value) -> return 0\nfunc i32 main = () -> return 0\n",
        "struct bad { val unit value }\nfunc i32 main = () -> return 0\n",
        "val array<unit> values = []\nfunc i32 main = () -> return 0\n",
        "val result<i32, unit> value = ok(1)\nfunc i32 main = () -> return 0\n",
        "func i32 unit.bad = () -> return 0\nfunc i32 main = () -> return 0\n",
        "func i32 main = () -> { val i32 value = (unit) 1 return value }\n",
    ];
    for src in cases {
        let error = fail(src);
        assert!(error.msg.contains("unit 只能"), "意外诊断: {}", error.msg);
    }
}

#[test]
fn no_return_expression_is_forbidden_in_value_positions() {
    let prefix = "func unit noop = () -> return\n";
    let cases = [
        "func i32 main = () -> { val i32 value = noop(); return value }\n",
        "func i32 main = () -> { var i32 value = 1; value = noop(); return value }\n",
        "func i32 take = (i32 value) -> return value\nfunc i32 main = () -> return take(noop())\n",
        "func i32 main = () -> { println noop(); return 0 }\n",
        "func i32 main = () -> { println '${noop()}'; return 0 }\n",
        "func i32 main = () -> { val array<i32> values = [noop()]; return 0 }\n",
        "func i32 main = () -> { val result<i32, string> value = ok(noop()); return 0 }\n",
        "func i32 main = () -> { val string value = from(noop()); return 0 }\n",
        "func i32 main = () -> { val string value = typeof(noop()); return 0 }\n",
        "func i32 main = () -> return noop()\n",
    ];
    for body in cases {
        let error = fail(&format!("{prefix}{body}"));
        assert!(
            error.msg.contains("无返回值表达式不能用于值位置"),
            "意外诊断: {}",
            error.msg
        );
    }
}

#[test]
fn unit_function_return_cannot_carry_a_value() {
    let error = fail("func unit bad = () -> return 1\nfunc i32 main = () -> return 0\n");
    assert_eq!(error.msg, "unit 函数的 return 不能携带值");
}

#[test]
fn empty_parentheses_are_not_a_value() {
    let error = fail("func i32 main = () -> { val i32 value = () return value }\n");
    assert_eq!(error.msg, "() 不是值；unit 只表示函数不返回值");
}
