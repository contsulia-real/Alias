use super::{
    Body, BuiltinCall, CallTarget, Expr, ExprCategory, Item, OwnershipCapability,
    ShallowClonePlan, Stmt, StorageRelation, ValueCategory,
};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

fn main_body(program: &mut super::CheckedProgram) -> &mut Body {
    let main = program
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "main" => Some(binding),
            _ => None,
        })
        .expect("main binding");
    let Expr::FuncLit { body, .. } = &mut main.value else {
        panic!("main value must be function literal")
    };
    body.as_mut()
}

fn binding<'a>(stmts: &'a mut [Stmt], name: &str) -> &'a mut super::Binding {
    stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == name => Some(binding),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing binding {name}"))
}

fn shallow_plan(expr: &Expr) -> &ShallowClonePlan {
    let Expr::Call {
        target: CallTarget::Builtin(BuiltinCall::ShallowClone(plan)),
        ..
    } = expr
    else {
        panic!("expected resolved shallow call")
    };
    plan
}

fn check_error(source: &str, what: &str) -> crate::AliasError {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).expect_err(what)
}

#[test]
fn shallow_freezes_recursive_plan_and_value_ownership() {
    let mut program = checked(
        "struct leaf { var i32 value = 1 }\n\
struct box { val leaf item = leaf() }\n\
func i32 main = () -> {\n\
    val box original = box()\n\
    val box copied = shallow(original)\n\
    val result<leaf, i32> wrapped = ok(leaf())\n\
    val result<leaf, i32> copied_result = shallow(wrapped)\n\
    return copied.item.value\n\
}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };

    let copied = binding(stmts, "copied");
    assert_eq!(
        shallow_plan(&copied.value),
        &ShallowClonePlan::Struct {
            name: "box".into(),
            fields: vec![ShallowClonePlan::Struct {
                name: "leaf".into(),
                fields: vec![ShallowClonePlan::Inline],
            }],
        }
    );
    assert_eq!(
        copied.value.category(),
        Some(ExprCategory::Value(ValueCategory::OwnedTemporary))
    );
    assert_eq!(
        copied.value.ownership_capability(),
        Some(OwnershipCapability::Available)
    );
    assert_eq!(copied.relation, Some(StorageRelation::Owning));

    assert_eq!(
        shallow_plan(&binding(stmts, "copied_result").value),
        &ShallowClonePlan::Result {
            ok: Box::new(ShallowClonePlan::Struct {
                name: "leaf".into(),
                fields: vec![ShallowClonePlan::Inline],
            }),
            err: Box::new(ShallowClonePlan::Inline),
        }
    );
}

#[test]
fn shallow_rejects_scalar_and_dynamic_ownership_children() {
    let scalar_error = check_error(
        "func i32 main = () -> {\n\
    val i32 bad = shallow(1)\n\
    return 0\n\
}\n",
        "scalar shallow root must fail",
    );
    assert!(
        scalar_error.msg.contains("不提供 shallow"),
        "实际: {}",
        scalar_error.msg
    );

    let string_error = check_error(
        "func i32 main = () -> {\n\
    val string bad = shallow('x')\n\
    return 0\n\
}\n",
        "string shallow must fail",
    );
    assert!(
        string_error.msg.contains("不支持 shallow"),
        "实际: {}",
        string_error.msg
    );

    let nested_owner_error = check_error(
        "struct bad_box { val string name = 'x' }\n\
func i32 main = () -> {\n\
    val bad_box original = bad_box()\n\
    val bad_box bad = shallow(original)\n\
    return 0\n\
}\n",
        "struct containing string must not shallow",
    );
    assert!(
        nested_owner_error.msg.contains("不支持 shallow"),
        "实际: {}",
        nested_owner_error.msg
    );

    let array_error = check_error(
        "func i32 main = () -> {\n\
    val array<i32> values = [1]\n\
    shallow(values)\n\
    return 0\n\
}\n",
        "array shallow must fail",
    );
    assert!(
        array_error.msg.contains("不支持 shallow"),
        "实际: {}",
        array_error.msg
    );

    let iterator_error = check_error(
        "func i32 main = () -> {\n\
    val array<i32> values = [1]\n\
    val iterator<i32> it = values.iterator()\n\
    shallow(it)\n\
    return 0\n\
}\n",
        "iterator shallow must fail",
    );
    assert!(
        iterator_error.msg.contains("不支持 shallow"),
        "实际: {}",
        iterator_error.msg
    );

    let function_error = check_error(
        "struct leaf { var i32 value = 1 }\n\
func leaf make = () -> return leaf()\n\
func i32 main = () -> {\n\
    shallow(make)\n\
    return 0\n\
}\n",
        "function shallow must fail without implicit zero call",
    );
    assert!(
        function_error.msg.contains("不支持 shallow"),
        "零参函数名必须按函数值拒绝 shallow，实际: {}",
        function_error.msg
    );
}

#[test]
fn final_hir_gate_rejects_shallow_plan_drift() {
    let mut program = checked(
        "struct leaf { var i32 value = 1 }\n\
func i32 main = () -> {\n\
    val leaf a = leaf()\n\
    val leaf c = shallow(a)\n\
    return 0\n\
}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let Expr::Call {
        target: CallTarget::Builtin(BuiltinCall::ShallowClone(plan)),
        ..
    } = &mut binding(stmts, "c").value
    else {
        panic!("c must be resolved shallow")
    };
    *plan = ShallowClonePlan::Result {
        ok: Box::new(ShallowClonePlan::Inline),
        err: Box::new(ShallowClonePlan::Inline),
    };

    let error = super::validate_resolved_hir(&program)
        .expect_err("mutated shallow plan must fail final HIR gate");
    assert!(
        error
            .msg
            .contains("ShallowClone plan 与 resolved 静态类型不一致"),
        "实际: {}",
        error.msg
    );
}
