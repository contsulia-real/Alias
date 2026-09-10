use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source should be rejected")
}

#[test]
fn iterator_source_loan_cannot_be_stored_in_an_array_element() {
    for storage in [
        "val array<iterator<i32>> stored = [values.iterator()]",
        "val array<iterator<i32>> stored = []\nstored.push(values.iterator())",
        "val iterator<i32> it = values.iterator()\nval array<iterator<i32>> stored = [move it]",
    ] {
        let source = format!("func i32 main = () -> {{\nval array<i32> values = [7]\n{storage}\nvalues.push(9)\nfor i32 item in stored[0] {{ println item }}\nreturn 0\n}}");
        let error = fail(&source);
        assert!(error.msg.contains("stored borrow"), "{}", error.msg);
    }
}

#[test]
fn iterator_source_loan_cannot_escape_through_other_owning_storage() {
    for source in [
        "val array<i32> values = [7]\nval iterator<i32> stored = values.iterator()\nfunc i32 main = () -> return 0",
        "func i32 main = () -> { val array<i32> values = [7]\nval result<iterator<i32>, i32> stored = ok(values.iterator())\nreturn 0 }",
        "struct holder { val iterator<i32> cursor }\nfunc i32 main = () -> { val array<i32> values = [7]\nval holder stored = holder(cursor = values.iterator())\nreturn 0 }",
        "val array<i32> values = [7]\nstruct holder { val iterator<i32> cursor = values.iterator() }\nfunc i32 main = () -> return 0",
    ] {
        let error = fail(source);
        assert!(error.msg.contains("stored borrow"), "{}\n{source}", error.msg);
    }
}

#[test]
fn iterator_replacement_switches_source_loans_without_retaining_old_generations() {
    for replacement in [
        "it = right.iterator()",
        "val iterator<i32> next = right.iterator()\nit = move next",
        "if round == 0 { it = right.iterator() } else { it = right.iterator() }",
    ] {
        let source = format!(r#"
func i32 main = () -> {{
    val array<i32> left = [1]
    val array<i32> right = [2]
    var iterator<i32> it = left.iterator()
    var i32 total = 0
    var i32 round = 0
    while round < 2 {{
        it = left.iterator()
        for i32 item in it {{ total = total + item }}
        {replacement}
        left.push(3)
        for i32 item in it {{ total = total + item }}
        round = round + 1
    }}
    right.push(4)
    return total
}}
"#);
        assert_eq!(run(&source).unwrap(), 9);
    }
}

#[test]
fn iterator_replacement_protects_the_new_source_and_live_aliases() {
    for replacement in ["it = right.iterator()", "val iterator<i32> next = right.iterator()\nit = move next"] {
        let source = format!("func i32 main = () -> {{\nval array<i32> left = [1]\nval array<i32> right = [2]\nvar iterator<i32> it = left.iterator()\n{replacement}\nright.push(3)\nfor i32 item in it {{ println item }}\nreturn 0\n}}");
        let error = fail(&source);
        assert!(error.msg.contains("loan") || error.msg.contains("Loan"), "{}", error.msg);
    }
    let error = fail(r#"
func i32 main = () -> {
    val array<i32> left = [1]
    val array<i32> right = [2]
    var iterator<i32> it = left.iterator()
    for i32 item in it { it = right.iterator() }
    return 0
}
"#);
    assert!(error.msg.contains("loan") || error.msg.contains("Loan"), "{}", error.msg);
}

#[test]
fn conditional_iterator_replacement_keeps_both_possible_sources_live() {
    for source in ["left", "right"] {
        let program = format!(r#"
func i32 main = () -> {{
    val array<i32> left = [1]
    val array<i32> right = [2]
    var iterator<i32> it = left.iterator()
    if left.len() == 1 {{ it = right.iterator() }}
    {source}.push(3)
    for i32 item in it {{ println item }}
    return 0
}}
"#);
        let error = fail(&program);
        assert!(error.msg.contains("loan") || error.msg.contains("Loan"), "{}", error.msg);
    }
}

#[test]
fn explicit_iterator_retains_the_original_array_loan_after_move() {
    for transfer in ["", "val iterator<i32> moved = move it\n"] {
        let iterator = if transfer.is_empty() { "it" } else { "moved" };
        for modification in ["items.push(3)", "items = [3]", "val array<i32> taken = move items"] {
            let source = format!("func i32 main = () -> {{\nvar array<i32> items = [1, 2]\nval iterator<i32> it = items.iterator()\n{transfer}{modification}\nfor i32 item in {iterator} {{ println item }}\nreturn 0\n}}");
            // A runtime invalidation error cannot satisfy the static loan diagnostic below.
            let error = fail(&source);
            assert!(error.msg.contains("loan") || error.msg.contains("Loan"), "{}", error.msg);
        }
    }
}

#[test]
fn moved_iterator_source_loan_ends_after_its_last_consumption() {
    let source = r#"
func i32 main = () -> {
    val array<i32> items = [1, 2]
    val iterator<i32> first = items.iterator()
    val iterator<i32> second = move first
    var i32 total = 0
    for i32 item in second { total = total + item }
    items.push(3)
    return total + items.len()
}
"#;
    assert_eq!(run(source).unwrap(), 6);
}

#[test]
fn direct_array_iteration_keeps_its_source_read_loan_over_backedges() {
    for modification in ["items.push(3)", "val i32 removed = items.pop()", "items = [3]", "val array<i32> taken = move items"] {
        let source = format!("func i32 main = () -> {{ var array<i32> items = [1, 2]\nfor i32 item in items {{ {modification}\n}}\nreturn 0 }}");
        let error = fail(&source);
        assert!(error.msg.contains("loan") || error.msg.contains("Loan"), "{}", error.msg);
    }
}

#[test]
fn direct_array_iteration_releases_its_loan_and_allows_disjoint_writes() {
    let source = r#"
struct pair { val array<i32> left = [1, 2] val array<i32> right = [] }
func i32 main = () -> {
    val pair values = pair()
    var i32 total = 0
    for i32 item in values.left {
        values.right.push(item)
        total = total + item
        continue
    }
    values.left.push(3)
    return total + values.left.len() + values.right.len()
}
"#;
    assert_eq!(run(source).unwrap(), 8);
}

#[test]
fn pattern_binding_is_an_independent_owner_for_local_loans() {
    for (subject, arms) in [
        ("'abc'", "item -> { val string alias = borrow item\nval i32 size = alias.len()\nval string taken = move item\nreturn size + taken.len() }"),
        ("wrapped", "ok(item) -> { val string alias = borrow item\nval i32 size = alias.len()\nval string taken = move item\nreturn size + taken.len() }\nerr(error) -> 99"),
        ("3", "item -> { val i32 alias = borrow item\nreturn alias + item }"),
    ] {
        let source = format!("func i32 main = () -> {{\nval result<string, i32> wrapped = ok('abc')\nmatch {subject} {{ {arms} }}\nreturn 0\n}}");
        assert_eq!(run(&source).unwrap(), 6);
    }
}

#[test]
fn pattern_binding_move_conflicts_with_its_live_loan() {
    let error = fail(r#"
func i32 main = () -> {
    match 'abc' {
        item -> {
            val string alias = borrow item
            val string taken = move item
            return alias.len()
        }
    }
    return 0
}
"#);
    assert!(error.msg.contains("live loan"), "{}", error.msg);
}

#[test]
fn for_element_loan_ends_before_transfer_and_the_next_iteration() {
    let source = r#"
func i32 main = () -> {
    var i32 total = 0
    for string item in ['a', 'bb', 'ccc'] {
        val string alias = borrow item
        total = total + alias.len()
        val string taken = move item
        total = total + taken.len()
    }
    return total
}
"#;
    assert_eq!(run(source).unwrap(), 12);
}

#[test]
fn for_element_cannot_move_while_its_loan_is_live() {
    let error = fail(r#"
func i32 main = () -> {
    for string item in ['a'] {
        val string alias = borrow item
        val string taken = move item
        return alias.len()
    }
    return 0
}
"#);
    assert!(error.msg.contains("live loan"), "{}", error.msg);
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
