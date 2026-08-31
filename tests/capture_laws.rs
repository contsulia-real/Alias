use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source should be rejected")
}

#[test]
fn write_capture_uses_an_exclusive_loan() {
    let error = fail(
        r#"
func i32 main = () -> {
    var i32 owner = 1
    func unit change = () -> increase owner
    println owner
    change()
    return owner
}
"#,
    );
    assert!(
        error.msg.contains("owner access") && error.msg.contains("live loan"),
        "{}",
        error.msg
    );
}

#[test]
fn projected_mutating_receiver_upgrades_the_root_capture() {
    let error = fail(
        r#"
struct bag {
    val array<i32> items = []
}
func i32 main = () -> {
    val bag owner = bag()
    func unit append = () -> owner.items.push(1)
    println owner.items.len()
    append()
    return owner.items.len()
}
"#,
    );
    assert!(
        error.msg.contains("owner access") && error.msg.contains("live loan"),
        "{}",
        error.msg
    );
}

#[test]
fn owner_access_resumes_after_a_write_capture_last_use() {
    let source = r#"
func i32 main = () -> {
    var i32 owner = 1
    func unit change = () -> increase owner
    change()
    return owner
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn scalar_move_read_conflicts_with_a_future_write_capture_use() {
    let error = fail(
        r#"
func i32 main = () -> {
    var i32 owner = 1
    func unit change = () -> increase owner
    val i32 copied = move owner
    change()
    return copied
}
"#,
    );
    assert!(error.msg.contains("live WriteLoan"), "{}", error.msg);
}

#[test]
fn scalar_move_inside_a_closure_only_requires_a_read_capture() {
    let source = r#"
func i32 main = () -> {
    val i32 owner = 1
    func i32 copy = () -> return move owner
    val i32 before = owner
    return before + copy()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn immediately_called_capture_has_a_temporary_loan_holder() {
    let source = r#"
func i32 main = () -> {
    val string owner = 'x'
    val i32 length = (() -> return owner.len())()
    val string transferred = move owner
    return length + transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn ternary_capture_branches_share_the_immediate_call_holder() {
    let source = r#"
func i32 main = () -> {
    val string owner = 'x'
    val bool first = true
    val i32 length = (first ? (() -> return owner.len()) : (() -> return owner.len() + 1))()
    val string transferred = move owner
    return length + transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn match_capture_arms_share_the_immediate_call_holder() {
    let source = r#"
func i32 main = () -> {
    val string owner = 'x'
    val bool first = false
    val i32 length = (match first {
        true -> (() -> return owner.len() + 1),
        false -> (() -> return owner.len()),
    })()
    val string transferred = move owner
    return length + transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn nested_closure_keeps_inner_capture_loans_live() {
    let error = fail(
        r#"
func i32 main = () -> {
    val string owner = 'x'
    func i32 inner = () -> return owner.len()
    func i32 outer = () -> return inner()
    val string transferred = move owner
    return outer() + transferred.len()
}
"#,
    );
    assert!(error.msg.contains("live loan"), "{}", error.msg);
}

#[test]
fn nested_write_capture_propagates_write_permission_to_the_parent() {
    let error = fail(
        r#"
func i32 main = () -> {
    var i32 owner = 1
    func unit outer = () -> {
        func unit inner = () -> increase owner
        inner()
    }
    println owner
    outer()
    return owner
}
"#,
    );
    assert!(
        error.msg.contains("owner access") && error.msg.contains("live loan"),
        "{}",
        error.msg
    );
}

#[test]
fn borrowed_alias_capture_requires_a_stable_referent_loan() {
    let error = fail(
        r#"
func i32 main = () -> {
    val string owner = 'x'
    val string alias = borrow owner
    func i32 length = () -> return alias.len()
    return length()
}
"#,
    );
    assert!(
        error.msg.contains("referent loan generation"),
        "{}",
        error.msg
    );
}

#[test]
fn captured_argument_uses_the_callee_read_borrow_effect() {
    let source = r#"
func i32 length = (string value) -> return value.len()
func i32 main = () -> {
    val string owner = 'x'
    func i32 call_length = () -> return length(owner)
    return call_length()
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn captured_dynamic_return_requires_a_resolved_return_effect() {
    let error = fail(
        r#"
func i32 main = () -> {
    val string owner = 'x'
    func string expose = () -> return owner
    return expose().len()
}
"#,
    );
    assert!(error.msg.contains("return effect"), "{}", error.msg);
}

#[test]
fn captured_dynamic_clone_can_return_as_a_fresh_owner() {
    let source = r#"
func i32 main = () -> {
    val string owner = 'x'
    func string copy = () -> return clone owner
    val string copied = copy()
    val string transferred = move owner
    return copied.len() + transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}
