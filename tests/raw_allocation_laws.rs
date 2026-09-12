//! Public malloc/free ownership laws. The runtime path is exercised through the normal native
//! build/run pipeline; rejected cases prove the same resolved ownership facts remain fail-closed.

use alias::{run, AliasError};

fn fail(source: &str) -> AliasError {
    run(source).expect_err("source must be rejected")
}

#[test]
fn fresh_and_local_allocation_roots_can_be_explicitly_freed() {
    let direct = "func i32 main = () -> {\n    free(malloc<i32>())\n    return 0\n}\n";
    assert_eq!(run(direct).unwrap(), 0);

    let local = "func i32 main = () -> {\n    val ptr<i32>? value = malloc<i32>()\n    free(move value)\n    return 0\n}\n";
    assert_eq!(run(local).unwrap(), 0);

    let no_parens = "func i32 main = () -> {\n    val ptr<i32>? value = malloc<i32>()\n    free move(value)\n    return 0\n}\n";
    assert_eq!(run(no_parens).unwrap(), 0);
}

#[test]
fn allocation_root_can_cross_owned_call_and_return_boundaries() {
    let source = "func ptr<i32>? make = () -> return malloc<i32>()\nfunc unit release = (ptr<i32>? value) -> free(move value)\nfunc i32 main = () -> {\n    val ptr<i32>? returned = make()\n    free(move returned)\n    release(malloc<i32>())\n    return 0\n}\n";
    assert_eq!(run(source).unwrap(), 0);
}

#[test]
fn every_reachable_local_root_path_must_consume_the_owner() {
    let leaked =
        fail("func i32 main = () -> {\n    val ptr<i32>? value = malloc<i32>()\n    return 0\n}\n");
    assert!(
        leaked.msg.contains("必须显式 free 或 transfer") && !leaked.msg.contains("内部"),
        "{}",
        leaked.msg
    );

    let branch = fail(
        "func i32 main = () -> {\n    val ptr<i32>? value = malloc<i32>()\n    if true {\n        free(move value)\n    }\n    return 0\n}\n",
    );
    assert!(
        branch.msg.contains("必须显式 free 或 transfer"),
        "{}",
        branch.msg
    );

    let loop_backedge = fail(
        "func i32 main = () -> {\n    var bool repeat = true\n    while repeat {\n        val ptr<i32>? value = malloc<i32>()\n        repeat = false\n    }\n    return 0\n}\n",
    );
    assert!(
        loop_backedge.msg.contains("必须显式 free 或 transfer"),
        "{}",
        loop_backedge.msg
    );

    let all_paths = "func i32 main = () -> {\n    val ptr<i32>? value = malloc<i32>()\n    if true {\n        free(move value)\n    } else {\n        free(move value)\n    }\n    return 0\n}\n";
    assert_eq!(run(all_paths).unwrap(), 0);
}

#[test]
fn free_requires_an_owned_value_and_consumption_is_single_use() {
    let place = fail(
        "func i32 main = () -> {\n    val ptr<i32>? value = malloc<i32>()\n    free(value)\n    return 0\n}\n",
    );
    assert!(place.msg.contains("free 只能消费"), "{}", place.msg);

    let twice = fail(
        "func i32 main = () -> {\n    val ptr<i32>? value = malloc<i32>()\n    free(move value)\n    free(move value)\n    return 0\n}\n",
    );
    assert!(twice.msg.contains("已被 move"), "{}", twice.msg);
}

#[test]
fn raw_roots_are_not_implicitly_discarded_replaced_or_stored() {
    let discarded = fail("func i32 main = () -> {\n    malloc<i32>()\n    return 0\n}\n");
    assert!(
        discarded.msg.contains("不能作为表达式结果被隐式丢弃"),
        "{}",
        discarded.msg
    );

    let replaced = fail(
        "func i32 main = () -> {\n    var ptr<i32>? value = malloc<i32>()\n    value = malloc<i32>()\n    free(move value)\n    return 0\n}\n",
    );
    assert!(
        replaced
            .msg
            .contains("replacement 前必须显式 free 或 transfer"),
        "{}",
        replaced.msg
    );

    let stored = fail(
        "func i32 main = () -> {\n    val array<ptr<i32>?> values = [malloc<i32>()]\n    return 0\n}\n",
    );
    assert!(
        stored.msg.contains("暂不能 transfer 进字段、容器"),
        "{}",
        stored.msg
    );

    let global = fail("val ptr<i32>? value = malloc<i32>()\nfunc i32 main = () -> return 0\n");
    assert!(global.msg.contains("global storage"), "{}", global.msg);

    let reinitialized = "func i32 main = () -> {\n    var ptr<i32>? value = malloc<i32>()\n    free(move value)\n    value = malloc<i32>()\n    free(move value)\n    return 0\n}\n";
    assert_eq!(run(reinitialized).unwrap(), 0);
}

#[test]
fn explicit_malloc_count_stays_closed_without_an_invented_integer_contract() {
    let error = fail("func i32 main = () -> {\n    free(malloc<i32>(2))\n    return 0\n}\n");
    assert!(
        error.msg.contains("源码整数类型合同尚未冻结"),
        "{}",
        error.msg
    );
}
