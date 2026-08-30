use super::{
    validate_resolved_hir, Body, Expr, ExprCategory, Item, OwnershipCapability, Place, Stmt,
    StorageRelation, ValueCategory,
};
use crate::sema::types::{IntW, Ty};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

fn moved_binding(program: &mut super::CheckedProgram) -> &mut super::Binding {
    let main = program
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "main" => Some(binding),
            _ => None,
        })
        .expect("main binding");
    let Expr::FuncLit { body, .. } = &mut main.value else {
        panic!("main value must be FuncLit")
    };
    let Body::Block(stmts) = body.as_mut() else {
        panic!("main body must be block")
    };
    stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == "transferred" => Some(binding),
            _ => None,
        })
        .expect("transferred binding")
}

#[test]
fn move_freezes_source_place_and_owned_result_facts() {
    let mut program = checked(
        "func i32 main = () -> {\n    val string source = 'x'\n    val string transferred = move(source)\n    return transferred.len()\n}\n",
    );
    let transferred = moved_binding(&mut program);
    assert_eq!(transferred.relation, Some(StorageRelation::Owning));
    assert_eq!(
        transferred.value.category(),
        Some(ExprCategory::Value(ValueCategory::OwnedTemporary))
    );
    assert_eq!(
        transferred.value.ownership_capability(),
        Some(OwnershipCapability::Available)
    );
    let Expr::Move { source, .. } = &transferred.value else {
        panic!("move must lower to a dedicated HIR operation")
    };
    assert!(matches!(source.as_ref(), Place::Local { .. }));
}

#[test]
fn final_gate_rejects_move_source_type_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n    val string source = 'x'\n    val string transferred = move(source)\n    return transferred.len()\n}\n",
    );
    let transferred = moved_binding(&mut program);
    let Expr::Move { source, .. } = &mut transferred.value else {
        panic!("move HIR")
    };
    match source.as_mut() {
        Place::Local { info, .. } => info.ty = Ty::Int(IntW::W32),
        _ => panic!("local source"),
    }
    let error = validate_resolved_hir(&program).expect_err("type drift must fail closed");
    assert!(
        error.msg.contains("Move source/result 类型不一致")
            || error
                .msg
                .contains("Place Local 类型与 BindingId 声明类型不一致"),
        "{}",
        error.msg
    );
}
