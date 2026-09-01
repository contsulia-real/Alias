use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source should be rejected")
}

#[test]
fn owned_temporary_return_creates_an_owning_caller_value() {
    let source = r#"
struct box {
    var i32 value = 1
}
func box make = () -> return box()
func i32 main = () -> {
    val box owner = make()
    owner.value = 7
    return owner.value
}
"#;
    assert_eq!(run(source).unwrap(), 7);
}

#[test]
fn owned_local_return_uses_implicit_return_transfer() {
    let source = r#"
func array<i32> make = () -> {
    val array<i32> local = [1]
    return local
}
func i32 main = () -> {
    val array<i32> owner = make()
    owner.push(2)
    return owner.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn borrowed_parameter_return_forwards_the_caller_place() {
    let source = r#"
struct box {
    var i32 value = 1
}
func box expose = (box value) -> return value
func i32 main = () -> {
    val box owner = box()
    val box alias = expose(owner)
    alias.value = 9
    return owner.value
}
"#;
    assert_eq!(run(source).unwrap(), 9);
}

#[test]
fn explicit_borrow_return_uses_the_same_source_contract() {
    let source = r#"
struct box {
    var i32 value = 1
}
func box expose = (box value) -> return borrow value
func i32 main = () -> {
    val box owner = box()
    val box alias = expose(owner)
    alias.value = 6
    return owner.value
}
"#;
    assert_eq!(run(source).unwrap(), 6);
}

#[test]
fn returned_loan_ends_at_the_alias_last_use() {
    let source = r#"
func string expose = (string value) -> return value
func i32 main = () -> {
    val string owner = 'x'
    val string alias = expose(owner)
    println alias
    val string transferred = move owner
    return transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn borrowed_return_requires_one_unique_source() {
    let error = fail(
        r#"
struct box {
    var i32 value = 1
}
func box choose = (box left, box right, bool take_left) -> {
    if take_left {
        return left
    }
    return right
}
func i32 main = () -> return 0
"#,
    );
    assert!(
        error.msg.contains("return effect") || error.msg.contains("borrowed source"),
        "{}",
        error.msg
    );
}

#[test]
fn borrowed_return_rejects_a_temporary_argument() {
    let error = fail(
        r#"
func string expose = (string value) -> return value
func i32 main = () -> return expose('x').len()
"#,
    );
    assert!(
        error.msg.contains("temporary argument") && error.msg.contains("borrowed return"),
        "{}",
        error.msg
    );
}

#[test]
fn borrowed_global_return_keeps_global_identity() {
    let source = r#"
struct box {
    var i32 value = 1
}
val box shared = box()
func box expose = () -> return shared
func i32 main = () -> {
    val box alias = expose()
    alias.value = 11
    return shared.value
}
"#;
    assert_eq!(run(source).unwrap(), 11);
}

#[test]
fn borrowed_self_return_maps_to_the_receiver_place() {
    let source = r#"
struct box {
    var i32 value = 1
}
func box box.expose = () -> return self
func i32 main = () -> {
    val box owner = box()
    val box alias = owner.expose()
    alias.value = 13
    return owner.value
}
"#;
    assert_eq!(run(source).unwrap(), 13);
}

#[test]
fn recursive_owned_return_effect_converges_from_a_fresh_base() {
    let source = r#"
func string make = (i32 remaining) -> {
    if remaining == 0 {
        return 'x'
    }
    return make(remaining - 1)
}
func i32 main = () -> {
    val string owner = make(2)
    return owner.len()
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn function_value_merge_requires_the_same_borrow_source() {
    let error = fail(
        r#"
struct box {
    var i32 value = 1
}
func box first = (box left, box right) -> return left
func box second = (box left, box right) -> return right
func i32 main = () -> {
    val bool choose_first = true
    val box left = box()
    val box right = box()
    val box alias = (choose_first ? first : second)(left, right)
    return alias.value
}
"#,
    );
    assert!(
        error.msg.contains("return effect") && error.msg.contains("borrow source"),
        "{}",
        error.msg
    );
}

#[test]
fn owner_write_conflicts_with_a_live_returned_loan() {
    let error = fail(
        r#"
struct box {
    var i32 value = 1
}
func box expose = (box value) -> return value
func i32 main = () -> {
    val box owner = box()
    val box alias = expose(owner)
    owner.value = 2
    println alias.value
    return owner.value
}
"#,
    );
    assert!(
        error.msg.contains("live loan") || error.msg.contains("loan 冲突"),
        "{}",
        error.msg
    );
}

#[test]
fn borrowed_global_scalar_return_is_not_collapsed_to_inline_copy() {
    let source = r#"
var i32 shared = 1
func i32 expose = () -> return borrow shared
func i32 main = () -> {
    var i32 alias = expose()
    increase alias
    return shared
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn borrowed_return_does_not_manufacture_write_permission() {
    let error = fail(
        r#"
val i32 shared = 1
func i32 expose = () -> return borrow shared
func i32 main = () -> {
    var i32 alias = expose()
    increase alias
    return shared
}
"#,
    );
    assert!(error.msg.contains("referent 未证明可写"), "{}", error.msg);
}

#[test]
fn inline_parameter_cannot_back_a_borrowed_return() {
    let error = fail(
        r#"
func i32 expose = (i32 value) -> return borrow value
func i32 main = () -> return 0
"#,
    );
    assert!(
        error.msg.contains("InlineValue") && error.msg.contains("borrowed return"),
        "{}",
        error.msg
    );
}

#[test]
fn main_cannot_return_a_borrowed_exit_code() {
    let error = fail(
        r#"
var i32 shared = 1
func i32 main = () -> return borrow shared
"#,
    );
    assert!(
        error.msg.contains("main") && error.msg.contains("Inline i32"),
        "{}",
        error.msg
    );
}
