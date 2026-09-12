//! Pointer type-stage laws. Values and raw allocation remain closed until their semantic and
//! runtime phases land; these cases cover only the frozen type grammar and aggregate ABI wiring.

use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source must be rejected")
}

#[test]
fn pointer_and_nullable_pointer_are_valid_type_slots() {
    let source = "struct holder {\n    val ptr<i32> value\n}\nfunc unit inspect = (ptr<i32> value) -> return\nfunc unit inspect_maybe = (ptr<i32>? value) -> return\nfunc i32 main = () -> return 0\n";
    assert_eq!(run(source).unwrap(), 0);
}

#[test]
fn pointer_requires_one_complete_storable_pointee_type() {
    let arity = fail(
        "func unit inspect = (ptr<i32, string> value) -> return\nfunc i32 main = () -> return 0\n",
    );
    assert!(arity.msg.contains("ptr 需要 1 个类型参数"), "{}", arity.msg);

    let unit =
        fail("func unit inspect = (ptr<unit> value) -> return\nfunc i32 main = () -> return 0\n");
    assert!(unit.msg.contains("不是完整可存储类型"), "{}", unit.msg);
}

#[test]
fn nullable_suffix_is_currently_pointer_only() {
    let error =
        fail("func unit inspect = (i32? value) -> return\nfunc i32 main = () -> return 0\n");
    assert!(error.msg.contains("当前只支持 ptr<T>?"), "{}", error.msg);
}

#[test]
fn pointer_owner_operations_stay_closed_before_raw_lifecycle_lowering() {
    let error = fail(
        "func ptr<i32> relay = (ptr<i32> value) -> return move value\nfunc i32 main = () -> return 0\n",
    );
    assert!(
        error.msg.contains("ptr ownership transfer") && !error.msg.contains("内部"),
        "{}",
        error.msg
    );
}

#[test]
fn pointer_values_cannot_enter_unimplemented_scalar_display_lowering() {
    let error = fail(
        "func string show = (ptr<i32> value) -> return (string) value\nfunc i32 main = () -> return 0\n",
    );
    assert!(
        error.msg.contains("不存在 ptr<i32> → string 转换") && !error.msg.contains("内部"),
        "{}",
        error.msg
    );
}
