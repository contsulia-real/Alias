use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source should be rejected")
}

#[test]
fn borrow_supports_parenthesized_and_no_paren_forms() {
    let source = r#"
func i32 main = () -> {
    val string first_owner = 'a'
    val string first = borrow(first_owner)
    val string second_owner = 'b'
    val string second = borrow second_owner
    return first.len() + second.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn read_loan_ends_at_last_alias_use() {
    let source = r#"
func i32 main = () -> {
    val string owner = 'x'
    val string alias = borrow owner
    println alias
    val string transferred = move owner
    return transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn move_conflicts_with_a_future_alias_use() {
    let error = fail(
        r#"
func i32 main = () -> {
    val string owner = 'x'
    val string alias = borrow owner
    val string transferred = move owner
    println alias
    return transferred.len()
}
"#,
    );
    assert!(error.msg.contains("live loan"), "{}", error.msg);
}

#[test]
fn overlapping_read_loans_can_coexist() {
    let source = r#"
func i32 main = () -> {
    val string owner = 'x'
    val string first = borrow owner
    val string second = borrow owner
    return first.len() + second.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn scalar_write_through_borrow_updates_the_referent() {
    let source = r#"
func i32 main = () -> {
    var i32 owner = 1
    val i32 alias = borrow owner
    increase alias
    return owner
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn write_loan_conflicts_with_an_overlapping_read_loan() {
    let error = fail(
        r#"
func i32 main = () -> {
    var i32 owner = 1
    val i32 writer = borrow owner
    val i32 reader = borrow owner
    increase writer
    println reader
    return owner
}
"#,
    );
    assert!(
        error.msg.contains("loan") && error.msg.contains("冲突"),
        "{}",
        error.msg
    );
}

#[test]
fn owner_write_conflicts_with_a_live_read_loan() {
    let error = fail(
        r#"
func i32 main = () -> {
    var i32 owner = 1
    val i32 alias = borrow owner
    owner = 2
    println alias
    return owner
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
fn borrowed_var_rebind_ends_the_previous_loan() {
    let source = r#"
func i32 main = () -> {
    var i32 first = 1
    var i32 second = 2
    var i32 alias = borrow first
    println alias
    alias = borrow second
    increase alias
    return second
}
"#;
    assert_eq!(run(source).unwrap(), 3);
}

#[test]
fn borrowed_struct_alias_writes_through_mutable_fields() {
    let source = r#"
struct box {
    var i32 value = 1
}
func i32 main = () -> {
    val box owner = box()
    val box alias = borrow owner
    alias.value = 7
    return owner.value
}
"#;
    assert_eq!(run(source).unwrap(), 7);
}

#[test]
fn immutable_scalar_referent_rejects_write_through() {
    let error = fail(
        r#"
func i32 main = () -> {
    val i32 owner = 1
    val i32 alias = borrow owner
    increase alias
    return owner
}
"#,
    );
    assert!(error.msg.contains("referent 未证明可写"), "{}", error.msg);
}

#[test]
fn borrowed_place_read_into_owning_slot_deep_clones() {
    let source = r#"
func i32 main = () -> {
    val string owner = 'x'
    val string alias = borrow owner
    val string copied = alias
    val string transferred = move owner
    return copied.len() + transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn a_future_borrowed_place_clone_keeps_the_loan_live() {
    let error = fail(
        r#"
func i32 main = () -> {
    val string owner = 'x'
    val string alias = borrow owner
    val string transferred = move owner
    val string copied = alias
    return transferred.len() + copied.len()
}
"#,
    );
    assert!(
        error.msg.contains("move source") && error.msg.contains("live loan"),
        "{}",
        error.msg
    );
}

#[test]
fn borrowed_value_cannot_be_stored_in_an_array() {
    let error = fail(
        r#"
func i32 main = () -> {
    val string owner = 'x'
    val array<string> values = [borrow owner]
    return values.len()
}
"#,
    );
    assert!(error.msg.contains("BorrowedValue"), "{}", error.msg);
}

#[test]
fn borrowed_value_cannot_bypass_referent_loan_forwarding() {
    let error = fail(
        r#"
func i32 length = (string value) -> return value.len()
func i32 main = () -> {
    val string owner = 'x'
    return length(borrow owner)
}
"#,
    );
    assert!(
        error.msg.contains("referent-loan forwarding"),
        "{}",
        error.msg
    );
}

#[test]
fn disjoint_field_loans_do_not_conflict() {
    let source = r#"
struct pair {
    var i32 first = 1
    var i32 second = 2
}
func i32 main = () -> {
    val pair owner = pair()
    val i32 writer = borrow owner.first
    val i32 reader = borrow owner.second
    increase writer
    return owner.first + reader
}
"#;
    assert_eq!(run(source).unwrap(), 4);
}

#[test]
fn owner_access_to_a_disjoint_field_does_not_conflict() {
    let source = r#"
struct pair {
    var i32 left = 1
    var i32 right = 2
}
func i32 main = () -> {
    val pair p = pair()
    val i32 left = borrow(p.left)
    p.right = p.right + 4
    println left
    return p.right
}
"#;
    assert_eq!(run(source).unwrap(), 6);
}

#[test]
fn owner_read_of_a_disjoint_field_coexists_with_a_write_loan() {
    let source = r#"
struct pair {
    var i32 left = 1
    var i32 right = 2
}
func i32 main = () -> {
    val pair p = pair()
    val i32 left = borrow(p.left)
    val i32 right_copy = p.right
    increase left
    return right_copy + p.left
}
"#;
    assert_eq!(run(source).unwrap(), 4);
}

#[test]
fn borrowed_scalar_place_cannot_use_move_spelling() {
    let error = fail(
        r#"
func i32 main = () -> {
    val i32 owner = 7
    val i32 alias = borrow owner
    return move alias
}
"#,
    );
    assert!(
        error.msg.contains("borrowed Place") && error.msg.contains("不能 move"),
        "{}",
        error.msg
    );
}

#[test]
fn branch_local_loan_ends_before_the_join_continues() {
    let source = r#"
func i32 main = () -> {
    val string owner = 'x'
    if true {
        val string alias = borrow owner
        println alias
    }
    val string transferred = move owner
    return transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn loop_back_edge_keeps_a_future_alias_use_live() {
    let error = fail(
        r#"
func i32 main = () -> {
    val string owner = 'x'
    val string alias = borrow owner
    while true {
        println alias
        val string transferred = move owner
        println transferred
    }
    return 0
}
"#,
    );
    assert!(error.msg.contains("live loan"), "{}", error.msg);
}
