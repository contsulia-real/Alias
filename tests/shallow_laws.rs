use alias::run;

#[test]
fn shallow_detaches_shallowable_struct_roots() {
    let source = r#"
struct leaf {
    var i32 value = 1
}
struct box {
    val leaf item = leaf()
}
func i32 main = () -> {
    val box original = box()
    val box copied = shallow(original)
    original.item.value = 9
    return copied.item.value
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn shallow_detaches_active_result_payload_root() {
    let source = r#"
struct leaf {
    var i32 value = 1
}
func i32 main = () -> {
    val leaf original = leaf()
    val result<leaf, i32> wrapped = ok(original)
    val result<leaf, i32> copied = shallow(wrapped)
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
fn shallow_supports_no_paren_intrinsic_syntax() {
    let source = r#"
struct leaf {
    var i32 value = 1
}
func i32 main = () -> {
    val leaf original = leaf()
    val leaf copied = shallow original
    original.value = 7
    return copied.value
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}
