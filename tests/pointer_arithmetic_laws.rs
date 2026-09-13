//! Checked pointer-view arithmetic and its source-loan laws.

use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source must be rejected")
}

fn cli_run(source: &str, file_name: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "alias-pointer-arithmetic-laws-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create test temp directory");
    let path = dir.join(file_name);
    std::fs::write(&path, source).expect("write pointer arithmetic source");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_alias"))
        .arg(&path)
        .output()
        .expect("run Alias CLI");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    output
}

#[test]
fn borrowed_pointer_offsets_preserve_provenance_bounds_and_element_stride() {
    let source = r#"
func i32 main = () -> {
    val i64 owner = 7
    val ptr<i64> start = refer(owner)
    val ptr<i64> end = start + 1
    val i64 distance = end - start
    if distance != 1 { return 1 }
    val ptr<i64> back = end - 1
    if back != start { return 2 }
    return 0
}
"#;
    assert_eq!(run(source).unwrap(), 0);
}

#[test]
fn borrowed_pointer_var_rebind_carries_the_original_source_loan() {
    let source = r#"
func i32 main = () -> {
    val i32 owner = 7
    var ptr<i32> cursor = refer(owner)
    cursor = cursor + 1
    cursor = cursor - 1
    val ptr<i32> origin = refer(owner)
    if cursor != origin { return 1 }
    return 0
}
"#;
    assert_eq!(run(source).unwrap(), 0);
}

#[test]
fn derived_pointer_keeps_the_root_loan_live_until_last_use() {
    let error = fail(
        r#"
func i32 main = () -> {
    var i32 owner = 7
    val ptr<i32> start = refer(owner)
    val ptr<i32> end = start + 1
    owner = 8
    if end > start { return 1 }
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
fn pointer_offset_outside_the_view_aborts_at_the_operator() {
    let source = "func i32 main = () -> {\n    val i32 owner = 7\n    val ptr<i32> start = refer(owner)\n    val ptr<i32> invalid = start + 2\n    return 0\n}\n";
    let output = cli_run(source, "bounds.as");
    assert_eq!(output.stdout, b"");
    assert_eq!(
        output.stderr,
        "错误 @ 4:27 — pointer view 越界\n".as_bytes()
    );
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn pointer_offset_stride_overflow_aborts_before_bounds_evaluation() {
    let source = "func i32 main = () -> {\n    val i64 owner = 7\n    val ptr<i64> start = refer(owner)\n    val ptr<i64> invalid = start + 18446744073709551615\n    return 0\n}\n";
    let output = cli_run(source, "overflow.as");
    assert_eq!(output.stdout, b"");
    assert_eq!(
        output.stderr,
        "错误 @ 4:27 — pointer arithmetic 溢出\n".as_bytes()
    );
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn pointer_arithmetic_rejects_nullable_owners_and_unbound_views() {
    let nullable = fail(
        r#"
func i32 main = () -> {
    val ptr<i32>? owner = malloc<i32>()
    val ptr<i32>? next = owner + 1
    free(move(owner))
    return 0
}
"#,
    );
    assert!(nullable.msg.contains("不适用于"), "{}", nullable.msg);

    let direct = fail(
        r#"
func i32 main = () -> {
    val i32 owner = 7
    val ptr<i32> next = refer(owner) + 1
    return 0
}
"#,
    );
    assert!(
        direct.msg.contains("stable borrowed pointer local"),
        "{}",
        direct.msg
    );
}
