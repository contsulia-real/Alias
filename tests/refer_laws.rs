//! Public refer laws for the address-taken local/global storage slice.

use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source must be rejected")
}

#[test]
fn local_storage_has_one_live_descriptor_for_repeated_refer() {
    let local = "func i32 main = () -> {\n    val i32 value = 7\n    val ptr<i32> first = refer(value)\n    val ptr<i32> second = refer value\n    return value\n}\n";
    assert_eq!(run(local).unwrap(), 7);
}

#[test]
fn global_storage_uses_a_slab_lifetime_descriptor() {
    let source = "val i32 value = 7\nfunc i32 main = () -> {\n    val ptr<i32> view = refer(value)\n    return value\n}\n";
    assert_eq!(run(source).unwrap(), 7);
}

#[test]
fn refer_requires_a_whole_addressable_owning_place() {
    let parameter = fail(
        "func i32 inspect = (i32 value) -> {\n    val ptr<i32> view = refer(value)\n    return value\n}\nfunc i32 main = () -> return inspect(7)\n",
    );
    assert!(parameter.msg.contains("owning"), "{}", parameter.msg);

    let field = fail(
        "struct pair { val i32 value = 7 }\nfunc i32 main = () -> {\n    val pair item = pair()\n    val ptr<i32> view = refer(item.value)\n    return item.value\n}\n",
    );
    assert!(field.msg.contains("subplace descriptor"), "{}", field.msg);
}

#[test]
fn refer_pointer_view_cannot_escape_or_enter_persistent_storage() {
    let returned = fail(
        "func ptr<i32> expose = (i32 value) -> return refer(value)\nfunc i32 main = () -> return 0\n",
    );
    assert!(
        returned.msg.contains("borrowed return ABI"),
        "{}",
        returned.msg
    );

    let stored = fail(
        "func i32 main = () -> {\n    val i32 value = 7\n    val array<ptr<i32>> views = [refer(value)]\n    return value\n}\n",
    );
    assert!(stored.msg.contains("BorrowedValue"), "{}", stored.msg);
}
