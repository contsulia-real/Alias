//! 唯一原生编译管线测试 — build 持久产物与 run 临时产物必须
//! 对同一源程序产生逐字节一致的三元组 (stdout/stderr/exit)。
//!
//! 政策: 期望值来自冻结黄金记录，禁止凭记忆断言。

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static TMP_SEQ: AtomicUsize = AtomicUsize::new(0);

/// 编译源码到临时 exe 并执行, 返回三元组。
fn build_and_run(src: &str) -> (Vec<u8>, Vec<u8>, i32) {
    let n = TMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "alias_native_build_{}_{n}_{}",
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

fn run_command(src: &str) -> (Vec<u8>, Vec<u8>, i32) {
    let n = TMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "alias_native_run_{}_{n}_{}",
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
    (out.stdout, out.stderr, out.status.code().unwrap_or(-1))
}

fn nested_closure_program(depth: usize) -> String {
    fn body(level: usize, depth: usize) -> String {
        let indent = "    ".repeat(level + 1);
        let mut out = format!("{indent}val i32 x{level} = {}\n", level + 1);
        if level + 1 == depth {
            let sum = std::iter::once("root".to_string())
                .chain((0..depth).map(|i| format!("x{i}")))
                .collect::<Vec<_>>()
                .join(" + ");
            out.push_str(&format!("{indent}return {sum}\n"));
        } else {
            out.push_str(&format!(
                "{indent}func i32 f{} = () -> {{\n{}{indent}}}\n",
                level + 1,
                body(level + 1, depth)
            ));
            out.push_str(&format!("{indent}return f{}()\n", level + 1));
        }
        out
    }
    format!(
        "func i32 main = () -> {{\n    val i32 root = 7\n    func i32 f0 = () -> {{\n{}    }}\n    return f0()\n}}\n",
        body(0, depth)
    )
}

/// 同一原生产物重复执行，专门捕获依赖地址/时序的间歇性崩溃。
fn build_once_and_run_many(src: &str, runs: usize) -> Vec<(Vec<u8>, Vec<u8>, i32)> {
    let n = TMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("alias_native_repeat_{}_{n}", std::process::id()));
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
        let run = Command::new(&exe_path).output().expect("运行原生产物失败");
        out.push((run.stdout, run.stderr, run.status.code().unwrap_or(-1)));
    }
    let _ = std::fs::remove_file(&exe_path);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_dir(&dir);
    out
}

/// 架构门禁：run 必须经过链接器。把链接器路径指向不存在文件后，命令必须
/// 在链接阶段失败；若未来有人塞回进程内执行捷径，本测试会立即暴露。
#[test]
fn run_requires_the_native_link_step() {
    let n = TMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("alias_native_link_gate_{}_{n}", std::process::id()));
    std::fs::create_dir(&dir).expect("创建临时目录失败");
    let source = dir.join("program.as");
    let missing_linker = dir.join("missing-rust-lld.exe");
    std::fs::write(&source, "func i32 main = () -> return 0\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_alias"))
        .args(["run", source.to_str().unwrap()])
        .env("ALIAS_RUST_LLD", &missing_linker)
        .output()
        .expect("启动 Alias 编译器失败");

    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_dir(dir);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("无法执行链接器"),
        "run 没有在链接阶段失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn run_matches_build_artifact_for_arithmetic_and_loops() {
    let src = "func i32 main = () -> {\n    var i32 x = 6;\n    increase x\n    val i32 y = x * 7;\n    return y - 1\n}\n";
    assert_eq!(build_and_run(src).2, run_command(src).2);
    assert_eq!(build_and_run(src).0, run_command(src).0);
}

#[test]
fn build_artifact_prints_integers() {
    // exit 48 用例的打印版: 输出可观察
    let src = "func i32 main = () -> {\n    var i32 i = 0;\n    while i < 3 {\n        increase i\n        println i\n    }\n    return 0\n}\n";
    let (so, se, code) = build_and_run(src);
    assert_eq!(String::from_utf8_lossy(&so), "1\n2\n3\n");
    assert_eq!(se, b"");
    assert_eq!(code, 0);
}

#[test]
fn run_matches_build_artifact_for_i32_main_one() {
    let src = "func i32 main = () -> return 1\n";
    assert_eq!(build_and_run(src).2, 1);
    assert_eq!(run_command(src).2, 1);
}

#[test]
fn build_artifact_div_zero_aborts_with_span() {
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
fn methods_same_native_binary_is_stable() {
    let expected = "忠犬\nABC\nabc\n3\nhi\n[]\n[plain]\n3\nHi!\n5\n7\nc(7)\n7\n".as_bytes();
    for (index, (stdout, stderr, code)) in
        build_once_and_run_many(include_str!("../demos/methods.as"), 100)
            .into_iter()
            .enumerate()
    {
        assert_eq!(code, 0, "第 {} 次执行退出异常", index + 1);
        assert_eq!(stderr, b"", "第 {} 次执行 stderr 非空", index + 1);
        assert_eq!(stdout, expected, "第 {} 次执行输出截断或损坏", index + 1);
    }
}

#[test]
fn run_matches_build_artifact_for_wide_and_float_display() {
    let src = "func i32 main = () -> {\n    val i64 a = 2147483648\n    val i64 max = 9223372036854775807\n    val i64 one = 1\n    val i64 min = -max - one\n    val u64 b = to_u64(-1)\n    val f32 c = 12.34\n    val f64 d = 0.125\n    println a\n    println min\n    println b\n    println c\n    println d\n    return 0\n}\n";
    let run = run_command(src);
    assert!(
        String::from_utf8_lossy(&run.0).contains("-9223372036854775808\n18446744073709551615\n")
    );
    assert_eq!(build_and_run(src), run);
}

#[test]
fn run_matches_build_artifact_for_non_finite_float_display() {
    let src = "func i32 main = () -> {\n    val f64 zero = 0.0\n    val f64 one = 1.0\n    println (zero / zero)\n    println (one / zero)\n    println (-one / zero)\n    return 0\n}\n";
    let run = run_command(src);
    assert_eq!(run.0, b"NaN\ninf\n-inf\n");
    assert_eq!(build_and_run(src), run);
}

#[test]
fn run_matches_build_artifact_for_f32_result_match_and_array() {
    let src = "func result<f32, string> get = () -> {\n    val f32 initial = 1.25\n    return ok(initial)\n}\nfunc i32 main = () -> {\n    val result<f32, string> r = get()\n    val f32 zero = 0.0\n    val f32 x = match r {\n        err(e) -> zero\n        ok(v) -> v\n    }\n    var array<f32> values = [x]\n    val f32 middle = 1.5\n    val f32 last = 2.5\n    values.push(middle)\n    values.push(last)\n    println values[0]\n    println values[1]\n    println values.pop()\n    return 0\n}\n";
    assert_eq!(build_and_run(src), run_command(src));
}

#[test]
fn run_matches_build_artifact_for_mixed_struct_layout_and_self_abi() {
    let src = "struct mixed {\n    var i8 small = 1\n    val f64 wide = 2.5\n    var i16 tail = 3\n    val string tag = 'm'\n}\npub func string mixed.label = () -> return '${self.tag}:${self.small}:${self.wide}:${self.tail}'\nfunc i32 main = () -> {\n    val mixed value = mixed()\n    value.small = 7\n    value.tail = 9\n    println value.label()\n    return 0\n}\n";
    let run = run_command(src);
    assert_eq!(run.2, 0);
    assert_eq!(build_and_run(src), run);
}

#[test]
fn run_matches_build_artifact_for_layout_permutation_matrix() {
    let src = r#"
struct a { val i8 x = 7 val f64 y = 2.5 val i16 z = 9 val string s = 'a' }
struct b { val string s = 'b' val i8 x = 8 val f32 y = 1.25 val i64 z = 99 }
struct c { val f64 x = 3.5 val bool ok = true val u8 y = 255 val string s = 'c' val i16 z = -4 }
pub func string a.show = () -> return '${self.s}:${self.x}:${self.y}:${self.z}'
pub func string b.show = () -> return '${self.s}:${self.x}:${self.y}:${self.z}'
pub func string c.show = () -> return '${self.s}:${self.x}:${self.ok}:${self.y}:${self.z}'
func i32 main = () -> {
    val a av = a()
    val b bv = b()
    val c cv = c()
    println av.show()
    println bv.show()
    println cv.show()
    return 0
}
"#;
    let run = run_command(src);
    assert_eq!(run.2, 0);
    assert_eq!(build_and_run(src), run);
}

#[test]
fn run_matches_build_artifact_for_integer_overflow() {
    let src = "func i32 main = () -> {\n    val u32 x = 4294967295\n    val u32 one = 1\n    println (x + one)\n    return 0\n}\n";
    let expected = (
        Vec::new(),
        "错误 @ 4:13 — 整数溢出\n".as_bytes().to_vec(),
        1,
    );
    assert_eq!(run_command(src), expected);
    assert_eq!(build_and_run(src), expected);
}

#[test]
fn run_matches_build_artifact_for_deep_transitive_closure_chain() {
    let depth = 16;
    let src = nested_closure_program(depth);
    let run = run_command(&src);
    assert_eq!(run.2, 7 + (1..=depth as i32).sum::<i32>());
    assert_eq!(build_and_run(&src), run);
}

#[test]
fn run_and_build_demo_corpus_match() {
    // 机械枚举 demos/ 下可运行语料，两条 CLI 工作流三元组逐字节一致。
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
            let run = run_command(&src);
            let built = build_and_run(&src);
            assert_eq!(built.0, run.0, "{name} stdout 不一致");
            assert_eq!(built.2, run.2, "{name} exit 不一致");
        }
    }
    assert!(runnable >= 2, "可运行 demo 数量异常: {runnable}");
}
