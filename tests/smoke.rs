//! `alias::run` library 入口的跨层冒烟测试。
//!
//! 具体语言法律与精确 stdout/stderr/exit 由 `*_laws` 和 `golden` 拥有；
//! 本文件只确认 library API 能贯通成功执行与静态错误路径。

use alias::run;

#[test]
fn run_api_executes_compiled_program() {
    let src = "
func i32 main = () -> {
    var i32 x = 6;
    increase x
    val i32 y = x * 7;
    return y - 1
}
";
    assert_eq!(run(src).expect("合法程序应经 run API 执行"), 48);
}

#[test]
fn run_api_executes_closure_and_loop_path() {
    let src = "
func i32 main = () -> {
    var i32 n = 0;
    func bool lt3 = (i32 cap) -> return n < cap
    while lt3(3) {
        increase n
    }
    return n
}
";
    assert_eq!(run(src).expect("闭包/循环主链应经 run API 执行"), 3);
}

#[test]
fn run_api_returns_static_error() {
    let src = "
func i32 main = () -> {
    var x = 1;
    return 0
}
";
    let error = run(src).expect_err("缺失类型槽必须由 run API 返回静态错误");
    assert!(error.msg.contains("类型槽"), "实际错误: {}", error.msg);
}
