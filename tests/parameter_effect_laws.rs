//! Function parameter-effect laws: inference, caller argument planning, and fail-closed escape.

use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source should be rejected")
}

#[test]
fn read_borrow_argument_preserves_the_stable_owner() {
    let source = r#"
func i32 length = (string value) -> return value.len()
func i32 main = () -> {
    val string owner = 'x'
    val i32 before = length(owner)
    val string transferred = move owner
    return before + transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn write_borrow_argument_mutates_the_caller_place() {
    let source = r#"
struct cell {
    var i32 value = 0
}
func unit set = (cell target) -> target.value = 7
func i32 main = () -> {
    val cell owner = cell()
    set(owner)
    return owner.value
}
"#;
    assert_eq!(run(source).unwrap(), 7);
}

#[test]
fn owned_parameter_accepts_an_explicit_transfer() {
    let source = r#"
func i32 consume = (string value) -> return move(value).len()
func i32 main = () -> {
    val string owner = 'xyz'
    return consume(move owner)
}
"#;
    assert_eq!(run(source).unwrap(), 3);
}

#[test]
fn owned_parameter_rejects_a_stable_owner_argument() {
    let error = fail(
        r#"
func i32 consume = (string value) -> return move(value).len()
func i32 main = () -> {
    val string owner = 'x'
    return consume(owner)
}
"#,
    );
    assert!(
        error.msg.contains("Owned parameter") && error.msg.contains("ownership-producing value"),
        "{}",
        error.msg
    );
}

#[test]
fn recursive_calls_converge_to_owned_parameter_effect() {
    let source = r#"
func i32 consume = (string value, i32 remaining) -> {
    if remaining == 0 {
        return move(value).len()
    }
    return consume(move(value), remaining - 1)
}
func i32 main = () -> return consume('x', 2)
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn borrowed_parameter_cannot_escape_before_return_effects_exist() {
    let error = fail(
        r#"
func string expose = (string value) -> return value
func i32 main = () -> return expose('x').len()
"#,
    );
    assert!(
        error.msg.contains("return source") && error.msg.contains("调用 loan"),
        "{}",
        error.msg
    );
}

#[test]
fn write_borrow_method_receiver_uses_the_same_argument_planner() {
    let source = r#"
struct counter {
    var i32 value = 0
}
func unit counter.set = (i32 value) -> self.value = value
func i32 main = () -> {
    val counter owner = counter()
    owner.set(9)
    return owner.value
}
"#;
    assert_eq!(run(source).unwrap(), 9);
}

#[test]
fn overlapping_read_and_write_call_loans_are_rejected() {
    let error = fail(
        r#"
struct cell {
    var i32 value = 0
}
func i32 observe_and_set = (cell observed, cell changed) -> {
    changed.value = 1
    return observed.value
}
func i32 main = () -> {
    val cell owner = cell()
    return observe_and_set(owner, owner)
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
fn function_value_branches_require_exact_parameter_effects() {
    let error = fail(
        r#"
struct cell {
    var i32 value = 0
}
func i32 observe = (cell target) -> return target.value
func i32 change = (cell target) -> {
    target.value = 1
    return target.value
}
func i32 main = () -> {
    val bool choose_read = true
    val cell owner = cell()
    return (choose_read ? observe : change)(owner)
}
"#,
    );
    assert!(
        error.msg.contains("函数值分支") && error.msg.contains("parameter effects"),
        "{}",
        error.msg
    );
}

#[test]
fn function_value_effects_are_compared_after_fixed_point_convergence() {
    let source = r#"
struct cell {
    var i32 value = 0
}
func i32 change = (cell target) -> {
    target.value = 1
    return target.value
}
func i32 forward = (cell target) -> return change(target)
func i32 main = () -> {
    val bool direct = true
    val cell owner = cell()
    return (direct ? change : forward)(owner)
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}
