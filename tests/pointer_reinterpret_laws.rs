//! Typed pointer-view reinterpretation, bounds narrowing, and source-loan laws.

use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source must be rejected")
}

fn cli_run(source: &str, file_name: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "alias-pointer-reinterpret-laws-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create test temp directory");
    let path = dir.join(file_name);
    std::fs::write(&path, source).expect("write pointer reinterpret source");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_alias"))
        .arg(&path)
        .output()
        .expect("run Alias CLI");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    output
}

#[test]
fn reinterpret_preserves_provenance_and_builds_a_target_stride_lattice() {
    let source = r#"
func i32 main = () -> {
    val i64 owner = 7
    val ptr<i64> original = refer(owner)
    val ptr<i32> words = reinterpret<i32>(original)
    val ptr<i32> end = words + 2
    if end - words != 2 { return 1 }
    val ptr<u8> bytes = reinterpret<u8>(original)
    val ptr<u8> byte_end = bytes + 8
    val ptr<u8> original_end = reinterpret<u8>(original + 1)
    if byte_end != original_end { return 2 }
    return 0
}
"#;
    assert_eq!(run(source).unwrap(), 0);
}

#[test]
fn reinterpret_excludes_an_incomplete_target_stride_tail() {
    let source = "func i32 main = () -> {\n    val i32 owner = 7\n    val ptr<i64> words = reinterpret<i64>(refer(owner))\n    val ptr<i64> invalid = words + 1\n    return 0\n}\n";
    let output = cli_run(source, "tail-bounds.as");
    assert_eq!(output.stdout, b"");
    assert_eq!(
        output.stderr,
        "错误 @ 4:27 — pointer view 越界\n".as_bytes()
    );
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn reinterpret_checks_target_alignment_at_runtime() {
    let source = "func i32 main = () -> {\n    val i64 owner = 7\n    val ptr<u8> bytes = reinterpret<u8>(refer(owner)) + 1\n    val ptr<i32> words = reinterpret<i32>(bytes)\n    return 0\n}\n";
    let output = cli_run(source, "alignment.as");
    assert_eq!(output.stdout, b"");
    assert_eq!(
        output.stderr,
        "错误 @ 4:36 — pointer address 未满足目标类型对齐\n".as_bytes()
    );
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn reinterpreted_view_keeps_the_original_owner_loan_live() {
    let error = fail(
        r#"
func i32 main = () -> {
    var i64 owner = 7
    val ptr<i32> words = reinterpret<i32>(refer(owner))
    owner = 8
    val ptr<i32> end = words + 1
    return 0
}
"#,
    );
    assert!(
        error.msg.contains("owner write") && error.msg.contains("live loan"),
        "{}",
        error.msg
    );
}

#[test]
fn reinterpret_rejects_owning_nullable_and_malformed_sources() {
    let owning = fail(
        r#"
func i32 main = () -> {
    val ptr<i32>? owner = malloc<i32>()
    val ptr<u8> view = reinterpret<u8>(owner)
    free(move(owner))
    return 0
}
"#,
    );
    assert!(owning.msg.contains("non-null ptr source"), "{}", owning.msg);

    let named = fail(
        r#"
func i32 main = () -> {
    val i32 owner = 7
    val ptr<u8> view = reinterpret<u8>(source = refer(owner))
    return 0
}
"#,
    );
    assert!(named.msg.contains("不接受命名实参"), "{}", named.msg);

    let incomplete = fail(
        r#"
func i32 main = () -> {
    val i32 owner = 7
    reinterpret<func>(refer(owner))
    return 0
}
"#,
    );
    assert!(
        incomplete.msg.contains("完整可存储类型"),
        "{}",
        incomplete.msg
    );
}
