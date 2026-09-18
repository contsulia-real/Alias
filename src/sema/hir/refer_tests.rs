use super::{
    validate_resolved_hir, Body, Expr, ExprCategory, Item, Place, Stmt, StorageRelation,
    ValueCategory,
};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

#[test]
fn refer_subplace_freezes_projection_and_root_descriptor_identity() {
    let program = checked(
        "struct pair { val i32 value = 7 }\nfunc i32 main = () -> {\n    val pair item = pair()\n    val ptr<i32> view = refer(item.value)\n    return item.value\n}\n",
    );
    let main = program
        .items
        .iter()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "main" => Some(binding),
            _ => None,
        })
        .unwrap();
    let Expr::FuncLit { body, .. } = &main.value else {
        panic!("main function")
    };
    let Body::Block(stmts) = body.as_ref() else {
        panic!("main block")
    };
    let view = stmts
        .iter()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == "view" => Some(binding),
            _ => None,
        })
        .unwrap();
    let Expr::Refer { source, .. } = &view.value else {
        panic!("refer HIR")
    };
    assert!(matches!(source.as_ref(), Place::Field { .. }));
    assert!(program
        .address_taken_roots
        .contains(&source.descriptor_root().unwrap()));
}

#[test]
fn refer_freezes_one_address_taken_root_and_borrowed_result() {
    let program = checked(
        "func i32 main = () -> {\n    val i32 value = 7\n    val ptr<i32> first = refer(value)\n    val ptr<i32> second = refer value\n    return value\n}\n",
    );
    assert_eq!(program.address_taken_roots.len(), 1);
    let main = program
        .items
        .iter()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "main" => Some(binding),
            _ => None,
        })
        .unwrap();
    let Expr::FuncLit { body, .. } = &main.value else {
        panic!("main function")
    };
    let Body::Block(stmts) = body.as_ref() else {
        panic!("main block")
    };
    let views = stmts.iter().filter_map(|stmt| match stmt {
        Stmt::Binding(binding) if binding.name == "first" || binding.name == "second" => {
            Some(binding)
        }
        _ => None,
    });
    for view in views {
        assert_eq!(view.relation, Some(StorageRelation::Borrowed));
        assert_eq!(
            view.value.category(),
            Some(ExprCategory::Value(ValueCategory::BorrowedValue))
        );
        let Expr::Refer { source, .. } = &view.value else {
            panic!("refer HIR")
        };
        assert!(program
            .address_taken_roots
            .contains(&source.descriptor_root().unwrap()));
    }
}

#[test]
fn final_gate_rejects_address_taken_root_set_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n    val i32 value = 7\n    val ptr<i32> view = refer(value)\n    return value\n}\n",
    );
    program.address_taken_roots.clear();
    let error = validate_resolved_hir(&program).expect_err("address-taken drift must fail closed");
    assert!(error.msg.contains("address-taken"), "{}", error.msg);
}
