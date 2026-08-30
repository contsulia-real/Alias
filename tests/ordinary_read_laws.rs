//! 稳定 Place 普通读取进入当前已知 owning slot 时的 DeepClone 法律。

use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source should be rejected")
}

#[test]
fn local_replacement_clones_before_overwriting_target() {
    let source = r#"
struct cell { var i32 value = 0 }
func i32 main = () -> {
    val cell source = cell(value = 1)
    var cell target = cell(value = 2)
    target = source
    target.value = 9
    return source.value * 10 + target.value
}
"#;
    assert_eq!(run(source).unwrap(), 19);
}

#[test]
fn field_replacement_clones_the_source_place() {
    let source = r#"
struct cell { var i32 value = 0 }
struct holder { var cell item = cell() }
func i32 main = () -> {
    val cell source = cell(value = 1)
    val holder target = holder()
    target.item = source
    target.item.value = 9
    return source.value * 10 + target.item.value
}
"#;
    assert_eq!(run(source).unwrap(), 19);
}

#[test]
fn aggregate_construction_clones_stable_place_inputs() {
    let source = r#"
struct cell { var i32 value = 0 }
struct holder { var cell item = cell() }
func i32 main = () -> {
    val cell source = cell(value = 1)
    val holder object = holder(item = source)
    val array<cell> items = [source]
    object.item.value = 7
    items[0].value = 9
    return source.value * 100 + object.item.value * 10 + items[0].value
}
"#;
    assert_eq!(run(source).unwrap(), 179);
}

#[test]
fn array_push_clones_a_stable_place_element() {
    let source = r#"
struct cell { var i32 value = 0 }
func i32 main = () -> {
    val cell source = cell(value = 1)
    val array<cell> items = []
    items.push(source)
    items[0].value = 9
    return source.value * 10 + items[0].value
}
"#;
    assert_eq!(run(source).unwrap(), 19);
}

#[test]
fn identity_conversion_cannot_hide_an_owning_place_read() {
    let source = r#"
struct cell { var i32 value = 0 }
func i32 main = () -> {
    val cell source = cell(value = 1)
    val cell copied = try_from source
    copied.value = 9
    return source.value * 10 + copied.value
}
"#;
    assert_eq!(run(source).unwrap(), 19);
}

#[test]
fn result_payload_construction_clones_the_source_place() {
    let source = r#"
struct cell { var i32 value = 0 }
func i32 main = () -> {
    val cell source = cell(value = 1)
    val result<cell, string> wrapped = ok(source)
    match wrapped {
        ok(value) -> {
            value.value = 9
            return source.value * 10 + value.value
        }
        err(message) -> return message.len()
    }
}
"#;
    assert_eq!(run(source).unwrap(), 19);
}

#[test]
fn field_default_is_cloned_for_each_construction() {
    let source = r#"
struct cell { var i32 value = 1 }
val cell default_cell = cell()
struct holder { var cell item = default_cell }
func i32 main = () -> {
    val holder first = holder()
    val holder second = holder()
    first.item.value = 9
    return second.item.value
}
"#;
    assert_eq!(run(source).unwrap(), 1);
}

#[test]
fn non_deep_cloneable_place_does_not_fall_back_to_reference_copy() {
    let error = fail(
        r#"
func i32 main = () -> {
    val array<i32> values = [1]
    val iterator<i32> source = values.iterator()
    val iterator<i32> copied = source
    return 0
}
"#,
    );
    assert!(error.msg.contains("不支持 clone"), "{}", error.msg);
}
