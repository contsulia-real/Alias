//! Provenance-aware pointer equality laws.

use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source must be rejected")
}

#[test]
fn equality_uses_provenance_and_address_but_not_view_storage_identity() {
    let source = r#"
val i32 global = 7
func i32 main = () -> {
    val i32 local = 7
    val ptr<i32> first = refer(local)
    val ptr<i32> repeated = refer local
    val ptr<i32> other = refer(global)
    if first != repeated { return 1 }
    if first == other { return 2 }
    return 0
}
"#;
    assert_eq!(run(source).unwrap(), 0);
}

#[test]
fn nullable_allocation_pointer_equality_preserves_all_owner_lanes() {
    let source = r#"
func i32 main = () -> {
    val ptr<i32>? owner = malloc<i32>()
    val bool equal = owner == owner
    free(move(owner))
    if !equal { return 1 }
    return 0
}
"#;
    assert_eq!(run(source).unwrap(), 0);
}

#[test]
fn a_later_pointer_comparison_keeps_the_source_loan_live() {
    let error = fail(
        r#"
func i32 main = () -> {
    var i32 owner = 1
    val ptr<i32> before = refer(owner)
    owner = 2
    val ptr<i32> after = refer(owner)
    if before == after { return 1 }
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
fn equality_requires_the_same_pointer_type() {
    let error = fail(
        r#"
func i32 main = () -> {
    val i32 narrow = 1
    val i64 wide = 1
    val ptr<i32> left = refer(narrow)
    val ptr<i64> right = refer(wide)
    if left == right { return 1 }
    return 0
}
"#,
    );
    assert!(error.msg.contains("不适用于"), "{}", error.msg);
}
