use alias::{run, AliasError};

fn fail(src: &str) -> AliasError {
    run("this-law.as", src).expect_err("该程序应在编译期失败")
}

#[test]
fn this_recurses_without_coupling_to_the_declared_name() {
    let src = r#"
func i32 factorial_with_any_name = (i32 n) -> {
    if n <= 1 { return 1 }
    return n * this(n - 1)
}
func i32 main = () -> return factorial_with_any_name(5)
"#;
    assert_eq!(run("this-recursion.as", src).unwrap(), 120);
}

#[test]
fn nested_functions_each_bind_this_to_themselves() {
    let src = r#"
func i32 outer = (i32 n) -> {
    func i32 inner = (i32 current) -> {
        if current == 0 { return 0 }
        return 1 + this(current - 1)
    }
    return inner(n)
}
func i32 main = () -> return outer(7)
"#;
    assert_eq!(run("nested-this.as", src).unwrap(), 7);
}

#[test]
fn this_outside_a_function_body_is_rejected() {
    let error = fail("val i32 invalid = this\nfunc i32 main = () -> return 0\n");
    assert_eq!(
        error.msg,
        "绑定 'invalid' 声明类型为 i32: this 只能出现在 func 体内"
    );
}
