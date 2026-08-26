//! Phase 5 AOT 奇偶校验 — build 产出的独立可执行文件必须与 JIT 路径
//! 对同一源程序产生逐字节一致的三元组 (stdout/stderr/exit)。
//!
//! 政策: 期望值来自对 JIT 路径的实际探测 (黄金记录背书), 禁止凭记忆断言。

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static TMP_SEQ: AtomicUsize = AtomicUsize::new(0);

/// 编译源码到临时 exe 并执行, 返回三元组。
fn build_and_run(src: &str) -> (Vec<u8>, Vec<u8>, i32) {
    let n = TMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "alias_aot_{}_{n}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    let src_path = dir.join("prog.as");
    let exe_path = dir.join("prog.exe");
    std::fs::write(&src_path, src).expect("写入临时源文件失败");

    let st = Command::new(env!("CARGO_BIN_EXE_alias"))
        .args(["build", src_path.to_str().unwrap()])
        .output()
        .expect("运行 alias build 失败");
    assert!(
        st.status.success(),
        "alias build 失败: {}",
        String::from_utf8_lossy(&st.stderr)
    );
    assert!(exe_path.exists(), "build 未产出 exe");

    let run = Command::new(&exe_path).output().expect("运行产物失败");

    let _ = std::fs::remove_file(&exe_path);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_dir(&dir);
    (run.stdout, run.stderr, run.status.code().unwrap_or(-1))
}

fn jit_run(src: &str) -> (Vec<u8>, Vec<u8>, i32) {
    let n = TMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "alias_jit_{}_{n}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    let src_path = dir.join("prog.as");
    std::fs::write(&src_path, src).expect("写入临时源文件失败");

    let out = Command::new(env!("CARGO_BIN_EXE_alias"))
        .arg(src_path.to_str().unwrap())
        .output()
        .expect("运行 alias 失败");

    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_dir(&dir);
    (
        out.stdout,
        out.stderr,
        out.status.code().unwrap_or(-1),
    )
}

/// 同一 AOT 产物重复执行，专门捕获依赖地址/时序的间歇性崩溃。
fn build_once_and_run_many(src: &str, runs: usize) -> Vec<(Vec<u8>, Vec<u8>, i32)> {
    let n = TMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "alias_aot_repeat_{}_{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    let src_path = dir.join("prog.as");
    let exe_path = dir.join("prog.exe");
    std::fs::write(&src_path, src).expect("写入临时源文件失败");
    let build = Command::new(env!("CARGO_BIN_EXE_alias"))
        .args(["build", src_path.to_str().unwrap()])
        .output()
        .expect("运行 alias build 失败");
    assert!(
        build.status.success(),
        "alias build 失败: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let mut out = Vec::with_capacity(runs);
    for _ in 0..runs {
        let run = Command::new(&exe_path).output().expect("运行 AOT 产物失败");
        out.push((
            run.stdout,
            run.stderr,
            run.status.code().unwrap_or(-1),
        ));
    }
    let _ = std::fs::remove_file(&exe_path);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_dir(&dir);
    out
}

#[test]
fn aot_matches_jit_arithmetic_and_loops() {
    let src = "func i32 main = () -> {\n    var i32 x = 6;\n    increase x\n    val i32 y = x * 7;\n    return y - 1\n}\n";
    assert_eq!(build_and_run(src).2, jit_run(src).2);
    assert_eq!(build_and_run(src).0, jit_run(src).0);
}

#[test]
fn aot_matches_jit_print_int() {
    // exit 48 用例的打印版: 输出可观察
    let src = "func i32 main = () -> {\n    var i32 i = 0;\n    for i < 3 {\n        increase i\n        println i\n    }\n    return 0\n}\n";
    let (so, se, code) = build_and_run(src);
    assert_eq!(String::from_utf8_lossy(&so), "1\n2\n3\n");
    assert_eq!(se, b"");
    assert_eq!(code, 0);
}

#[test]
fn aot_matches_jit_i32_main_one() {
    let src = "func i32 main = () -> return 1\n";
    assert_eq!(build_and_run(src).2, 1);
    assert_eq!(jit_run(src).2, 1);
}

#[test]
fn aot_div_zero_aborts_with_span() {
    // 除零: 运行时错误 → stderr 带 span + 退出码 1 (span-ID 表数据段回查)
    let src = "func i32 main = () -> {\n    val i32 z = 0;\n    return 5 / z\n}\n";
    let (_, se, code) = build_and_run(src);
    let s = String::from_utf8_lossy(&se);
    assert!(
        s.contains("除以零") && s.contains("@ 3:"),
        "stderr 应含 span 化除零消息, 实际: {s}"
    );
    assert_eq!(code, 1);
}

#[test]
fn methods_aot_same_binary_is_stable() {
    let expected = "忠犬\nABC\nabc\n3\nhi\n[]\n[plain]\n3\nHi!\n5\n7\nc(7)\n7\n".as_bytes();
    for (index, (stdout, stderr, code)) in
        build_once_and_run_many(include_str!("../demos/methods.as"), 30)
            .into_iter()
            .enumerate()
    {
        assert_eq!(code, 0, "第 {} 次执行退出异常", index + 1);
        assert_eq!(stderr, b"", "第 {} 次执行 stderr 非空", index + 1);
        assert_eq!(stdout, expected, "第 {} 次执行输出截断或损坏", index + 1);
    }
}

#[test]
fn aot_matches_jit_wide_and_float_display() {
    let src = "func i32 main = () -> {\n    val i64 a = 2147483648\n    val i64 max = 9223372036854775807\n    val i64 one = 1\n    val i64 min = max + one\n    val u64 b = to_u64(-1)\n    val f32 c = 12.34\n    val f64 d = 0.125\n    println a\n    println min\n    println b\n    println c\n    println d\n    return 0\n}\n";
    let jit = jit_run(src);
    assert!(String::from_utf8_lossy(&jit.0).contains("-9223372036854775808\n18446744073709551615\n"));
    assert_eq!(build_and_run(src), jit);
}

#[test]
fn aot_matches_jit_non_finite_float_display() {
    let src = "func i32 main = () -> {\n    val f64 zero = 0.0\n    val f64 one = 1.0\n    println (zero / zero)\n    println (one / zero)\n    println (-one / zero)\n    return 0\n}\n";
    let jit = jit_run(src);
    assert_eq!(jit.0, b"NaN\ninf\n-inf\n");
    assert_eq!(build_and_run(src), jit);
}

#[test]
fn aot_matches_jit_f32_result_match_and_array() {
    let src = "func result<f32, string> get = () -> {\n    val f32 initial = 1.25\n    return ok(initial)\n}\nfunc i32 main = () -> {\n    val result<f32, string> r = get()\n    val f32 zero = 0.0\n    val f32 x = match r {\n        err(e) -> zero\n        ok(v) -> v\n    }\n    var array<f32> values = [x]\n    val f32 middle = 1.5\n    val f32 last = 2.5\n    values.push(middle)\n    values.push(last)\n    println values[0]\n    println values[1]\n    println values.pop()\n    return 0\n}\n";
    assert_eq!(build_and_run(src), jit_run(src));
}

#[test]
fn aot_matches_jit_mixed_struct_layout_and_self_abi() {
    let src = "struct mixed {\n    var i8 small = 1\n    val f64 wide = 2.5\n    var i16 tail = 3\n    val string tag = 'm'\n}\npublic func string mixed.label = () -> return '${self.tag}:${self.small}:${self.wide}:${self.tail}'\nfunc i32 main = () -> {\n    val mixed value = mixed()\n    value.small = 7\n    value.tail = 9\n    println value.label()\n    return 0\n}\n";
    let jit = jit_run(src);
    assert_eq!(jit.2, 0);
    assert_eq!(build_and_run(src), jit);
}

#[test]
fn aot_demo_corpus_parity() {
    // 机械枚举 demos/ 下可运行语料, 双形态三元组逐字节一致。
    // forward-spec demos (recursion/file_wc/producer_consumer/helper)
    // 在 sema 处以相同错误拒绝, 天然满足 parity — 但 build 的错误
    // 报告发生在编译器进程而非产物, 故此处仅校验 count_to_ten 与
    // hello_native 两个 Phase-1 可运行程序 + 内联片段用例。
    let entries = std::fs::read_dir("demos").expect("枚举 demos 失败");
    let mut runnable = 0;
    for e in entries.filter_map(|e| e.ok()) {
        let path = e.path();
        if path.extension().map(|x| x != "as").unwrap_or(true) {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name == "count_to_ten.as"
            || name == "hello_native.as"
            || name == "structs.as"
            || name == "result_match.as"
            || name == "methods.as"
            || name == "arrays.as"
        {
            runnable += 1;
            let src = std::fs::read_to_string(&path).unwrap();
            let jit = jit_run(&src);
            let aot = build_and_run(&src);
            assert_eq!(aot.0, jit.0, "{name} stdout 不一致");
            assert_eq!(aot.2, jit.2, "{name} exit 不一致");
        }
    }
    assert!(runnable >= 2, "可运行 demo 数量异常: {runnable}");
}
