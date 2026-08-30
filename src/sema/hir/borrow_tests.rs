use super::{
    validate_resolved_hir, Body, BorrowKind, Expr, ExprCategory, Item, OwnershipCapability, Place,
    Stmt, StorageRelation, ValueCategory,
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
fn borrow_freezes_place_relation_category_and_read_loan() {
    let mut program = checked(
        "func i32 main = () -> {\n    val string owner = 'x'\n    val string alias = borrow owner\n    return alias.len()\n}\n",
    );
    let alias = binding(&mut program, "alias");
    assert_eq!(alias.relation, Some(StorageRelation::Borrowed));
    assert_eq!(
        alias.value.category(),
        Some(ExprCategory::Value(ValueCategory::BorrowedValue))
    );
    assert_eq!(
        alias.value.ownership_capability(),
        Some(OwnershipCapability::None)
    );
    let Expr::Borrow {
        source,
        kind: Some(kind),
        ..
    } = &alias.value
    else {
        panic!("borrow must lower to dedicated HIR")
    };
    assert!(matches!(source.as_ref(), Place::Local { .. }));
    assert_eq!(*kind, BorrowKind::Read);
}

#[test]
fn actual_write_through_upgrades_the_loan_kind() {
    let mut program = checked(
        "func i32 main = () -> {\n    var i32 owner = 1\n    val i32 alias = borrow owner\n    increase alias\n    return owner\n}\n",
    );
    let alias = binding(&mut program, "alias");
    let Expr::Borrow {
        kind: Some(kind), ..
    } = &alias.value
    else {
        panic!("borrow HIR")
    };
    assert_eq!(*kind, BorrowKind::Write);
}

#[test]
fn final_gate_rejects_loan_kind_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n    var i32 owner = 1\n    val i32 alias = borrow owner\n    increase alias\n    return owner\n}\n",
    );
    let alias = binding(&mut program, "alias");
    let Expr::Borrow { kind, .. } = &mut alias.value else {
        panic!("borrow HIR")
    };
    *kind = Some(BorrowKind::Read);
    let error = validate_resolved_hir(&program).expect_err("loan kind drift must fail closed");
    assert!(
        error.msg.contains("loan kind") && error.msg.contains("漂移"),
        "{}",
        error.msg
    );
}

#[test]
fn final_gate_rejects_borrowed_relation_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n    val string owner = 'x'\n    val string alias = borrow owner\n    return alias.len()\n}\n",
    );
    binding(&mut program, "alias").relation = Some(StorageRelation::Owning);
    let error = validate_resolved_hir(&program).expect_err("relation drift must fail closed");
    assert!(error.msg.contains("storage relation"), "{}", error.msg);
}
