use alias::{run, AliasError};

#[test]
fn iterator_return_preserves_one_incoming_source_through_local_moves() {
    let source = r#"
func iterator<i32> forward = (iterator<i32> it, bool flag) -> {
    val iterator<i32> moved = move it
    if flag { return move moved }
    return moved
}
func i32 main = () -> {
    val array<i32> values = [7]
    var i32 total = 0
    for bool flag in [true, false] {
        val iterator<i32> it = forward(values.iterator(), flag)
        for i32 item in it { total = total + item }
    }
    return total
}
"#;
    assert_eq!(run(source).unwrap(), 14);
}

#[test]
fn iterator_return_distinguishes_incoming_owned_parameter_sources() {
    let source = r#"
func iterator<i32> choose = (iterator<i32> left, iterator<i32> right, bool flag) -> {
    if flag { return move left }
    return move right
}
func i32 main = () -> return 0
"#;
    let error = fail(source);
    assert!(error.msg.contains("源数组 loan 来源不唯一"), "{}", error.msg);
}

#[test]
fn iterator_return_rejects_different_reaching_array_sources() {
    for body in [
        "if flag { return left.iterator() }\nreturn right.iterator()",
        "var iterator<i32> it = left.iterator()\nif flag { it = right.iterator() }\nreturn it",
    ] {
        let source = format!("func iterator<i32> choose = (array<i32> left, array<i32> right, bool flag) -> {{\n{body}\n}}\nfunc i32 main = () -> return 0");
        let error = fail(&source);
        assert!(error.msg.contains("源数组 loan 来源不唯一"), "{}", error.msg);
    }
}

#[test]
fn iterator_return_accepts_the_same_constant_projection_across_exits() {
    let source = r#"
func iterator<i32> choose = (array<array<i32>> values, bool flag) -> {
    if flag { return values[0].iterator() }
    return values[0].iterator()
}
func i32 main = () -> {
    val array<array<i32>> values = [[7]]
    val iterator<i32> it = choose(values, true)
    var i32 total = 0
    for i32 item in it { total = total + item }
    return total
}
"#;
    assert_eq!(run(source).unwrap(), 7);
}

#[test]
fn returned_iterator_does_not_hold_unrelated_arguments_or_outlive_its_last_use() {
    for (declarations, call) in [
        ("func iterator<i32> make = (array<i32> values, array<i32> other) -> { val i32 size = other.len()\nreturn values.iterator() }", "make(values, other)"),
        ("func iterator<i32> forward = (iterator<i32> it, array<i32> other) -> { val i32 size = other.len()\nreturn move it }", "forward(values.iterator(), other)"),
        ("func iterator<i32> make = (array<i32> values, array<i32> other) -> { val i32 size = other.len()\nreturn values.iterator() }\nfunc iterator<i32> forward = (array<i32> values, array<i32> other) -> return make(values, other)", "(true ? make : forward)(values, other)"),
    ] {
        let source = format!(r#"
{declarations}
func i32 main = () -> {{
    val array<i32> values = [1, 2]
    val array<i32> other = []
    val iterator<i32> it = {call}
    other.push(9)
    var i32 total = 0
    for i32 item in it {{ total = total + item }}
    values.push(3)
    return total + values.len() + other.len()
}}
"#);
        assert_eq!(run(&source).unwrap(), 7);
    }
}

#[test]
fn returned_iterator_keeps_its_array_loan_in_the_caller() {
    let mut missed = Vec::new();
    for (name, declarations, call) in [
        ("parameter source", "func iterator<i32> make = (array<i32> values) -> return values.iterator()", "make(values)"),
        ("forwarded return", "func iterator<i32> make = (array<i32> values) -> return values.iterator()\nfunc iterator<i32> forward = (array<i32> values) -> return make(values)", "forward(values)"),
        ("owned iterator argument", "func iterator<i32> forward = (iterator<i32> it) -> return move it", "forward(values.iterator())"),
        ("recursive return", "func iterator<i32> walk = (array<i32> values, i32 depth) -> { if depth == 0 { return values.iterator() }\nreturn walk(values, depth - 1) }", "walk(values, 2)"),
    ] {
        let source = format!("{declarations}\nfunc i32 main = () -> {{\nval array<i32> values = [1, 2]\nval iterator<i32> it = {call}\nvalues.push(3)\nfor i32 item in it {{ println item }}\nreturn 0\n}}");
        // A native fail-fast exit is not the static rejection required by the source-loan law.
        match run(&source) {
            Err(error) if error.msg.contains("loan") || error.msg.contains("Loan") => {}
            result => missed.push(format!("{name}: {result:?}")),
        }
    }
    assert!(missed.is_empty(), "missing caller source loans:\n{}", missed.join("\n"));
}

#[test]
fn iterator_return_cannot_outlive_its_local_array_source() {
    for returned in [
        "return values.iterator()",
        "val iterator<i32> it = values.iterator()\nreturn it",
        "val iterator<i32> it = values.iterator()\nreturn move it",
        "val iterator<i32> it = values.iterator()\nval iterator<i32> next = move it\nreturn next",
    ] {
        let source = format!("func iterator<i32> make = () -> {{\nval array<i32> values = [1]\n{returned}\n}}\nfunc i32 main = () -> {{ val iterator<i32> it = make()\nfor i32 item in it {{ println item }}\nreturn 0 }}");
        let error = fail(&source);
        assert!(error.msg.contains("源数组 loan") && error.msg.contains("local owner"), "{}", error.msg);
    }
}

#[test]
fn function_value_merge_rejects_different_owned_iterator_sources() {
    let error = fail(
        "func iterator<i32> left = (array<i32> a, array<i32> b) -> { val i32 size = b.len()\nreturn a.iterator() }\nfunc iterator<i32> right = (array<i32> a, array<i32> b) -> { val i32 size = a.len()\nreturn b.iterator() }\nfunc i32 main = () -> { val array<i32> a = [1]\nval array<i32> b = [2]\nval iterator<i32> it = (true ? left : right)(a, b)\nfor i32 item in it { println item }\nreturn 0 }",
    );
    assert!(error.msg.contains("return effect / borrow source"), "{}", error.msg);
}

#[test]
fn iterator_return_uses_the_reaching_source_after_replacement() {
    let source = r#"
val array<i32> shared = [7]
func iterator<i32> make = () -> {
    val array<i32> local = [1]
    var iterator<i32> it = local.iterator()
    it = shared.iterator()
    return it
}
func i32 main = () -> {
    val iterator<i32> it = make()
    var i32 total = 0
    for i32 item in it { total = total + item }
    return total
}
"#;
    assert_eq!(run(source).unwrap(), 7);
}

#[test]
fn returns_nested_inside_a_return_operand_keep_their_own_passes() {
    for source in [
        "func i32 main = () -> return match true { true -> { return 7 } false -> 9 }",
        "func i32 main = () -> return match true { true -> { return 7 } false -> { return 9 } }",
        "func i32 main = () -> return match true {\ntrue -> match false {\ntrue -> { return 9 }\nfalse -> { return 7 }\n}\nfalse -> 9\n}",
        "func i32 main = () -> { match true {\ntrue -> return match false {\ntrue -> { return 9 }\nfalse -> 7\n}\nfalse -> return 0\n} }",
    ] {
        assert_eq!(run(source).unwrap_or_else(|error| panic!("{error}\n{source}")), 7);
    }
    let owned = "func string make = () -> { val string local = 'abc'\nreturn match true { true -> { return local } false -> 'x' } }\nfunc i32 main = () -> { val string value = make()\nreturn value.len() }";
    assert_eq!(run(owned).unwrap(), 3);
}

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
