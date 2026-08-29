use alias::run;

#[test]
fn clone_recursively_detaches_nested_structs() {
    let source = r#"
struct leaf {
    var i32 value = 1
}
struct box {
    val leaf item = leaf()
}
func i32 main = () -> {
    val box original = box()
    val box copied = clone(original)
    original.item.value = 9
    return copied.item.value
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn clone_detaches_array_backing_storage() {
    let source = r#"
func i32 main = () -> {
    val array<i32> original = [1, 2]
    val array<i32> copied = clone(original)
    original.push(3)
    return copied.len()
}
"#;
    assert_eq!(run(source).unwrap(), 2);
}

#[test]
fn clone_recursively_detaches_result_payload() {
    let source = r#"
struct leaf {
    var i32 value = 1
}
func i32 main = () -> {
    val leaf original = leaf()
    val result<leaf, i32> wrapped = ok(original)
    val result<leaf, i32> copied = clone(wrapped)
    original.value = 9
    return match copied {
        ok(value) -> value.value,
        err(_) -> 99,
    }
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn clone_empty_array_uses_target_element_type() {
    let source = r#"
func i32 main = () -> {
    val array<i32> copied = clone([])
    return copied.len()
}
"#;
    assert_eq!(run(source).unwrap(), 0);
}

#[test]
fn clone_supports_no_paren_intrinsic_syntax() {
    let source = r#"
func i32 main = () -> {
    val string original = 'x'
    val string copied = clone original
    if copied == 'x' {
        return 0
    }
    return 1
}
"#;
    assert_eq!(run(source).unwrap(), 0);
}
