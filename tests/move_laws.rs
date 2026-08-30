use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source should be rejected")
}

#[test]
fn move_transfers_a_dynamic_local_owner() {
    let source = r#"
func i32 main = () -> {
    val string original = 'x'
    val string transferred = move(original)
    return transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn move_supports_no_paren_intrinsic_syntax() {
    let source = r#"
func i32 main = () -> {
    val string original = 'x'
    val string transferred = move original
    return transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn scalar_move_remains_ordinary_value_passing() {
    let source = r#"
func i32 main = () -> {
    val i32 original = 3
    val i32 copied = move(original)
    return original + copied
}
"#;
    assert_eq!(run(source).unwrap(), 6);
}

#[test]
fn moved_dynamic_local_cannot_be_read() {
    let error = fail(
        r#"
func i32 main = () -> {
    val string original = 'x'
    val string transferred = move(original)
    println original
    return transferred.len()
}
"#,
    );
    assert!(error.msg.contains("值已被 move"), "{}", error.msg);
}

#[test]
fn moved_var_can_be_reinitialized_from_a_fresh_owner() {
    let source = r#"
func i32 main = () -> {
    var string original = 'x'
    val string transferred = move(original)
    original = 'again'
    return original.len() + transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 6);
}

#[test]
fn prior_owning_read_clones_and_preserves_unique_ownership() {
    let source = r#"
func i32 main = () -> {
    val string original = 'x'
    val string copied = original
    val string transferred = move(original)
    return copied.len() + transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn closure_capture_prevents_moving_the_captured_owner() {
    let error = fail(
        r#"
func i32 main = () -> {
    val string original = 'x'
    func i32 length = () -> return original.len()
    val string transferred = move(original)
    return length() + transferred.len()
}
"#,
    );
    assert!(error.msg.contains("closure 捕获"), "{}", error.msg);
}

#[test]
fn move_replacement_must_be_proven_disjoint() {
    let error = fail(
        r#"
func i32 main = () -> {
    var string value = 'x'
    value = move(value)
    return 0
}
"#,
    );
    assert!(
        error.msg.contains("replacement target") && error.msg.contains("互不重叠"),
        "{}",
        error.msg
    );
}

#[test]
fn move_on_one_branch_invalidates_the_join() {
    let error = fail(
        r#"
func i32 main = () -> {
    val string original = 'x'
    if true {
        val string transferred = move(original)
        println transferred
    }
    println original
    return 0
}
"#,
    );
    assert!(error.msg.contains("值已被 move"), "{}", error.msg);
}

#[test]
fn loop_back_edge_rejects_a_second_move() {
    let error = fail(
        r#"
func i32 main = () -> {
    val string original = 'x'
    while true {
        val string transferred = move(original)
        println transferred
    }
    return 0
}
"#,
    );
    assert!(
        error.msg.contains("ownership capability 已被 move"),
        "{}",
        error.msg
    );
}

#[test]
fn ordinary_field_move_out_is_rejected() {
    let error = fail(
        r#"
struct box {
    val string item = 'x'
}
func i32 main = () -> {
    val box owner = box()
    val string item = move(owner.item)
    return item.len()
}
"#,
    );
    assert!(error.msg.contains("不能被 move-out"), "{}", error.msg);
}

#[test]
fn dynamic_parameter_move_waits_for_parameter_effects() {
    let error = fail(
        r#"
func string take = (string value) -> return move(value)
func i32 main = () -> return take('x').len()
"#,
    );
    assert!(
        error.msg.contains("当前函数内的 owning local"),
        "{}",
        error.msg
    );
}
