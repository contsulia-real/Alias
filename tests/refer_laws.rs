//! Public refer laws for local/global roots and their projected subplaces.

use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source must be rejected")
}

fn cli_run(source: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("alias-refer-laws-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create test temp directory");
    let path = dir.join("distinct-roots.as");
    std::fs::write(&path, source).expect("write refer source");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_alias"))
        .arg(&path)
        .output()
        .expect("run Alias CLI");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
    output
}

#[test]
fn local_storage_has_one_live_descriptor_for_repeated_refer() {
    let local = "func i32 main = () -> {\n    val i32 value = 7\n    val ptr<i32> first = refer(value)\n    val ptr<i32> second = refer value\n    return value\n}\n";
    assert_eq!(run(local).unwrap(), 7);
}

#[test]
fn global_storage_uses_a_slab_lifetime_descriptor() {
    let source = "val i32 value = 7\nfunc i32 main = () -> {\n    val ptr<i32> view = refer(value)\n    return value\n}\n";
    assert_eq!(run(source).unwrap(), 7);
}

#[test]
fn refer_requires_an_addressable_owning_root() {
    let parameter = fail(
        "func i32 inspect = (i32 value) -> {\n    val ptr<i32> view = refer(value)\n    return value\n}\nfunc i32 main = () -> return inspect(7)\n",
    );
    assert!(parameter.msg.contains("owning"), "{}", parameter.msg);
}

#[test]
fn field_subviews_share_root_provenance_and_use_projected_addresses() {
    let source = r#"
struct pair {
    var i32 left = 7
    var i32 right = 9
}
func i32 main = () -> {
    var pair item = pair()
    val ptr<i32> left = refer(item.left)
    item.right = 11
    val ptr<i32> right = refer(item.right)
    if right - left != 1 { return 1 }
    return item.right
}
"#;
    assert_eq!(run(source).unwrap(), 11);
}

#[test]
fn whole_cell_and_heap_field_have_distinct_provenance() {
    let source = r#"
struct pair { val i32 left = 7 }
func i32 main = () -> {
    val pair item = pair()
    val ptr<u8> cell = reinterpret<u8>(refer(item))
    val ptr<u8> field = reinterpret<u8>(refer(item.left))
    val i64 invalid = field - cell
    return 0
}
"#;
    let output = cli_run(source);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("pointer provenance 不兼容"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn nested_heap_subplace_waits_for_its_own_descriptor_lifecycle() {
    let error = fail(
        "struct inner { val i32 value = 7 }\nstruct outer { val inner child = inner() }\nfunc i32 main = () -> {\n    val outer item = outer()\n    val ptr<i32> view = refer(item.child.value)\n    return 0\n}\n",
    );
    assert!(error.msg.contains("nested subplace"), "{}", error.msg);
}

#[test]
fn array_element_subviews_share_root_provenance_and_stride() {
    let source = r#"
func i32 main = () -> {
    val array<i32> values = [7, 9]
    val ptr<i32> first = refer(values[0])
    val ptr<i32> second = refer(values[1])
    if second - first != 1 { return 1 }
    return 0
}
"#;
    assert_eq!(run(source).unwrap(), 0);
}

#[test]
fn projected_refer_loan_rejects_only_overlapping_owner_write() {
    let overlapping = fail(
        r#"
struct pair {
    var i32 left = 7
    var i32 right = 9
}
func i32 main = () -> {
    var pair item = pair()
    val ptr<i32> view = refer(item.left)
    item.left = 8
    val ptr<i32> end = view + 1
    return 0
}
"#,
    );
    assert!(
        overlapping.msg.contains("owner write") && overlapping.msg.contains("live loan"),
        "{}",
        overlapping.msg
    );
}

#[test]
fn array_subview_blocks_backing_relocation_while_live() {
    let error = fail(
        r#"
func i32 main = () -> {
    var array<i32> values = [7]
    val ptr<i32> view = refer(values[0])
    values.push(9)
    val ptr<i32> end = view + 1
    return 0
}
"#,
    );
    assert!(error.msg.contains("loan"), "{}", error.msg);
}

#[test]
fn refer_pointer_view_cannot_escape_or_enter_persistent_storage() {
    let returned = fail(
        "func ptr<i32> expose = (i32 value) -> return refer(value)\nfunc i32 main = () -> return 0\n",
    );
    assert!(
        returned.msg.contains("borrowed return ABI"),
        "{}",
        returned.msg
    );

    let stored = fail(
        "func i32 main = () -> {\n    val i32 value = 7\n    val array<ptr<i32>> views = [refer(value)]\n    return value\n}\n",
    );
    assert!(stored.msg.contains("BorrowedValue"), "{}", stored.msg);
}
