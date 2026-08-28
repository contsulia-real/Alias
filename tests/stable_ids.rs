//! 稳定语义身份回归：BindingId / MethodId / field index / Pattern binding / HIR capture。

use alias::run;

fn ok(src: &str) -> i32 {
    run(src).unwrap_or_else(|e| panic!("应当通过, 实际: {e}"))
}

#[test]
fn closure_capture_survives_same_name_shadowing() {
    let src = "func i32 main = () -> {\n    val i32 x = 4\n    func i32 capture = () -> return x\n    if true {\n        val i32 x = 9\n        val i32 y = capture\n        if y != 4 { return 1 }\n        if x != 9 { return 2 }\n    }\n    return 0\n}\n";
    assert_eq!(ok(src), 0);
}

#[test]
fn assignment_target_is_stable_across_shadowing() {
    let src = "func i32 main = () -> {\n    var i32 x = 1\n    if true {\n        var i32 x = 2\n        x = 3\n    }\n    return x\n}\n";
    assert_eq!(ok(src), 1);
}

#[test]
fn for_binding_does_not_replace_outer_binding() {
    let src = "func i32 main = () -> {\n    val i32 x = 7\n    var i32 seen = 0\n    for i32 x in [1] {\n        seen = x\n    }\n    if seen != 1 { return 1 }\n    return x\n}\n";
    assert_eq!(ok(src), 7);
}

#[test]
fn pattern_binding_does_not_replace_outer_binding() {
    let src = "func i32 main = () -> {\n    val i32 x = 9\n    val i32 y = match 1 { x -> x }\n    if y != 1 { return 1 }\n    return x\n}\n";
    assert_eq!(ok(src), 9);
}

#[test]
fn field_access_uses_resolved_layout_index() {
    let src = "struct pair {\n    val i32 left = 1\n    val i32 right = 2\n}\nfunc i32 main = () -> {\n    val pair p = pair()\n    return p.right\n}\n";
    assert_eq!(ok(src), 2);
}

#[test]
fn same_method_name_on_different_receivers_keeps_distinct_method_ids() {
    let src = "struct left_box { val i32 v = 1 }\nstruct right_box { val i32 v = 2 }\npub func i32 left_box.read = () -> return self.v\npub func i32 right_box.read = () -> return self.v\nfunc i32 main = () -> {\n    val left_box a = left_box()\n    val right_box b = right_box()\n    return (a read) + (b read)\n}\n";
    assert_eq!(ok(src), 3);
}
