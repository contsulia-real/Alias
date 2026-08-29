use super::{
    Body, BuiltinCall, CallTarget, DeepClonePlan, Expr, ExprCategory, Item, OwnershipCapability,
    Stmt, StorageRelation, ValueCategory,
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

fn clone_plan(expr: &Expr) -> &DeepClonePlan {
    let Expr::Call {
        target: CallTarget::Builtin(BuiltinCall::DeepClone(plan)),
        ..
    } = expr
    else {
        panic!("expected resolved clone call")
    };
    plan
}

#[test]
fn clone_freezes_recursive_plan_and_value_ownership() {
    let mut program = checked(
        "struct leaf { val string name = 'x' }\n\
struct box { val leaf item = leaf() }\n\
func i32 main = () -> {\n\
    val i32 n = 1\n\
    val i32 n2 = clone(n)\n\
    val string s = 'a'\n\
    val string s2 = clone(s)\n\
    val box b = box()\n\
    val box b2 = clone(b)\n\
    val array<string> a = ['a', 'b']\n\
    val array<string> a2 = clone(a)\n\
    val result<string, i32> r = ok('x')\n\
    val result<string, i32> r2 = clone(r)\n\
    val array<i32> empty = clone([])\n\
    return n2\n\
}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };

    let n2 = binding(stmts, "n2");
    assert_eq!(clone_plan(&n2.value), &DeepClonePlan::Inline);
    assert_eq!(
        n2.value.category(),
        Some(ExprCategory::Value(ValueCategory::InlineValue))
    );
    assert_eq!(
        n2.value.ownership_capability(),
        Some(OwnershipCapability::None)
    );
    assert_eq!(n2.relation, Some(StorageRelation::Owning));

    let s2 = binding(stmts, "s2");
    assert_eq!(clone_plan(&s2.value), &DeepClonePlan::String);
    assert_eq!(
        s2.value.category(),
        Some(ExprCategory::Value(ValueCategory::OwnedTemporary))
    );
    assert_eq!(
        s2.value.ownership_capability(),
        Some(OwnershipCapability::Available)
    );
    assert_eq!(s2.relation, Some(StorageRelation::Owning));

    assert_eq!(
        clone_plan(&binding(stmts, "b2").value),
        &DeepClonePlan::Struct {
            name: "box".into(),
            fields: vec![DeepClonePlan::Struct {
                name: "leaf".into(),
                fields: vec![DeepClonePlan::String],
            }],
        }
    );
    assert_eq!(
        clone_plan(&binding(stmts, "a2").value),
        &DeepClonePlan::Array(Box::new(DeepClonePlan::String))
    );
    assert_eq!(
        clone_plan(&binding(stmts, "r2").value),
        &DeepClonePlan::Result {
            ok: Box::new(DeepClonePlan::String),
            err: Box::new(DeepClonePlan::Inline),
        }
    );
    assert_eq!(
        clone_plan(&binding(stmts, "empty").value),
        &DeepClonePlan::Array(Box::new(DeepClonePlan::Inline)),
        "clone source 必须继承 array<i32> 目标类型，不能留下 Unknown"
    );
}

#[test]
fn clone_rejects_non_deep_cloneable_function_and_iterator() {
    let function_error = {
        let tokens = crate::lexer::lex(
            "func i32 f = () -> return 1\nfunc i32 main = () -> { val func bad = clone(f) return 0 }\n",
        )
        .unwrap();
        let program = crate::parser::parse(tokens).unwrap();
        crate::sema::check(program).expect_err("function clone must fail")
    };
    assert!(
        function_error.msg.contains("不支持 clone"),
        "实际: {}",
        function_error.msg
    );

    let iterator_error = {
        let tokens = crate::lexer::lex(
            "func i32 main = () -> { val array<i32> a = [1] val iterator<i32> it = a.iterator() val iterator<i32> bad = clone(it) return 0 }\n",
        )
        .unwrap();
        let program = crate::parser::parse(tokens).unwrap();
        crate::sema::check(program).expect_err("iterator clone must fail")
    };
    assert!(
        iterator_error.msg.contains("不支持 clone"),
        "实际: {}",
        iterator_error.msg
    );
}

#[test]
fn final_hir_gate_rejects_deep_clone_plan_drift() {
    let mut program = checked(
        "func i32 main = () -> { val string s = 'a' val string c = clone(s) return 0 }\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let Expr::Call {
        target: CallTarget::Builtin(BuiltinCall::DeepClone(plan)),
        ..
    } = &mut binding(stmts, "c").value
    else {
        panic!("c must be resolved clone")
    };
    *plan = DeepClonePlan::Array(Box::new(DeepClonePlan::Inline));

    let error = super::validate_resolved_hir(&program)
        .expect_err("mutated deep-clone plan must fail final HIR gate");
    assert!(
        error.msg.contains("DeepClone plan 与 resolved 静态类型不一致"),
        "实际: {}",
        error.msg
    );
}
