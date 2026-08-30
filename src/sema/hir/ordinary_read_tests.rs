use super::{
    validate_resolved_hir, Body, DeepClonePlan, Expr, ExprCategory, Item, OwnershipCapability,
    Place, Stmt, StorageRelation, ValueCategory,
};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

fn binding<'a>(program: &'a mut super::CheckedProgram, name: &str) -> &'a mut super::Binding {
    let main = program
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "main" => Some(binding),
            _ => None,
        })
        .expect("main binding");
    let Expr::FuncLit { body, .. } = &mut main.value else {
        panic!("main value must be a function literal")
    };
    let Body::Block(stmts) = body.as_mut() else {
        panic!("main body must be a block")
    };
    stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == name => Some(binding),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing binding {name}"))
}

#[test]
fn owning_slot_read_freezes_place_plan_and_ownership_facts() {
    let mut program = checked(
        "struct leaf { val string text = 'x' }\n\
struct box { val leaf item = leaf() }\n\
func i32 main = () -> {\n\
    val box source = box()\n\
    val box copied = source\n\
    val i32 scalar = 1\n\
    val i32 scalar_copy = scalar\n\
    return scalar_copy\n\
}\n",
    );

    let copied = binding(&mut program, "copied");
    assert_eq!(copied.relation, Some(StorageRelation::Owning));
    assert_eq!(
        copied.value.category(),
        Some(ExprCategory::Value(ValueCategory::OwnedTemporary))
    );
    assert_eq!(
        copied.value.ownership_capability(),
        Some(OwnershipCapability::Available)
    );
    let Expr::ReadPlace { source, plan, .. } = &copied.value else {
        panic!("owning dynamic binding must lower to ReadPlace")
    };
    assert!(matches!(source.as_ref(), Place::Local { .. }));
    assert_eq!(
        plan,
        &DeepClonePlan::Struct {
            name: "box".into(),
            fields: vec![DeepClonePlan::Struct {
                name: "leaf".into(),
                fields: vec![DeepClonePlan::String],
            }],
        }
    );

    let scalar = binding(&mut program, "scalar_copy");
    assert_eq!(scalar.relation, Some(StorageRelation::Owning));
    assert_eq!(
        scalar.value.category(),
        Some(ExprCategory::Value(ValueCategory::InlineValue))
    );
    assert_eq!(
        scalar.value.ownership_capability(),
        Some(OwnershipCapability::None)
    );
    assert!(matches!(
        scalar.value,
        Expr::ReadPlace {
            plan: DeepClonePlan::Inline,
            ..
        }
    ));
}

#[test]
fn final_gate_rejects_ordinary_read_plan_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n\
    val string source = 'x'\n\
    val string copied = source\n\
    return copied.len()\n\
}\n",
    );
    let copied = binding(&mut program, "copied");
    let Expr::ReadPlace { plan, .. } = &mut copied.value else {
        panic!("copied binding must be ReadPlace")
    };
    *plan = DeepClonePlan::Array(Box::new(DeepClonePlan::Inline));

    let error = validate_resolved_hir(&program)
        .expect_err("mutated ordinary-read plan must fail final HIR gate");
    assert!(
        error
            .msg
            .contains("ReadPlace DeepClone plan 与 resolved Place 类型不一致"),
        "{}",
        error.msg
    );
}
