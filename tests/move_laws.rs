use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source should be rejected")
}

#[test]
fn for_element_has_a_fresh_owner_on_each_iteration_and_continue() {
    let source = r#"
struct cell { var i32 value }
func i32 main = () -> {
    val array<cell> values = [cell(value = 1), cell(value = 2), cell(value = 3)]
    var i32 total = 0
    for cell item in values {
        val cell taken = move item
        taken.value = taken.value + 10
        total = total + taken.value
        continue
    }
    return total + values[0].value + values[1].value + values[2].value
}
"#;
    assert_eq!(run(source).unwrap(), 42);
}

#[test]
fn for_element_move_does_not_authorize_another_use_in_the_same_iteration() {
    for tail in ["val cell second = move item", "val i32 observed = item.value"] {
        let source = format!(
            "struct cell {{ var i32 value = 1 }}\nfunc i32 main = () -> {{\nval array<cell> values = [cell()]\nfor cell item in values {{\nval cell taken = move item\n{tail}\n}}\nreturn 0\n}}"
        );
        let error = fail(&source);
        assert!(error.msg.contains("move"), "{}", error.msg);
    }
}

#[test]
fn move_transfers_through_array_constructor_push_struct_and_result() {
    let source = r#"
struct cell { var i32 value = 0 }
struct holder { val cell item }
func i32 main = () -> {
    val cell first = cell(value = 1)
    val array<cell> items = [move first]
    val cell second = cell(value = 2)
    items.push(move second)
    val cell removed = items.pop()
    val holder object = holder(item = move removed)
    val result<holder, string> wrapped = ok(move object)
    return match wrapped {
        ok(value) -> value.item.value * 10 + items[0].value
        err(error) -> 99
    }
}
"#;
    assert_eq!(run(source).unwrap(), 21);
}

#[test]
fn container_transfer_does_not_leave_the_source_readable() {
    for destination in [
        "val array<cell> items = [move source]",
        "val array<cell> items = []\nitems.push(move source)",
        "val holder object = holder(item = move source)",
        "val result<cell, string> wrapped = ok(move source)",
    ] {
        let source = format!("struct cell {{ var i32 value = 1 }}\nstruct holder {{ val cell item }}\nfunc i32 main = () -> {{\nval cell source = cell()\n{destination}\nreturn source.value\n}}\n");
        let error = fail(&source);
        assert!(error.msg.contains("move"), "{}", error.msg);
    }
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
fn scalar_parameter_move_remains_ordinary_value_passing() {
    let source = r#"
func i32 copy = (i32 value) -> return move value
func i32 main = () -> return copy(3)
"#;
    assert_eq!(run(source).unwrap(), 3);
}

#[test]
fn scalar_global_move_remains_ordinary_value_passing() {
    let source = r#"
val i32 global = 4
func i32 main = () -> return move global
"#;
    assert_eq!(run(source).unwrap(), 4);
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
    assert!(
        error.msg.contains("move source") && error.msg.contains("live loan"),
        "{}",
        error.msg
    );
}

#[test]
fn capture_loan_ends_after_the_closure_last_use() {
    let source = r#"
func i32 main = () -> {
    val string original = 'x'
    func i32 length = () -> return original.len()
    val i32 before_move = length()
    val string transferred = move original
    return before_move + transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn unused_closure_does_not_keep_its_capture_loan_live() {
    let source = r#"
func i32 main = () -> {
    val string original = 'x'
    func i32 length = () -> return original.len()
    val string transferred = move original
    return transferred.len()
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn scalar_move_is_a_read_under_a_capture_read_loan() {
    let source = r#"
func i32 main = () -> {
    val i32 original = 3
    func i32 read = () -> return original
    val i32 copied = move original
    return copied + read()
}
"#;
    assert_eq!(run(source).unwrap(), 6);
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
fn dynamic_parameter_move_infers_owned_effect() {
    let source = r#"
func string take = (string value) -> return move(value)
func i32 main = () -> return take('x').len()
"#;
    assert_eq!(run(source).unwrap(), 1);
}
