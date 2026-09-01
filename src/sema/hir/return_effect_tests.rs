use super::{
    validate_resolved_hir, Body, CallResult, Expr, Item, ReturnPass, Stmt, StorageRelation,
};
use crate::sema::types::{ReturnBorrowSource, ReturnEffect, Ty};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

fn top_binding<'a>(program: &'a mut super::CheckedProgram, name: &str) -> &'a mut super::Binding {
    program
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == name => Some(binding.as_mut()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing top-level binding {name}"))
}

#[test]
fn borrowed_return_freezes_signature_call_source_and_alias_relation() {
    let mut program = checked(
        "struct box {\n    var i32 value = 1\n}\nfunc box expose = (box value) -> return value\nfunc i32 main = () -> {\n    val box owner = box()\n    val box alias = expose(owner)\n    return alias.value\n}\n",
    );
    let expose = top_binding(&mut program, "expose");
    let Ty::Func {
        return_effect: Some(effect),
        ..
    } = &expose.ty
    else {
        panic!("expose return effect")
    };
    assert_eq!(
        *effect,
        ReturnEffect::Borrowed(ReturnBorrowSource::Parameter(0))
    );
    let Expr::FuncLit { body, .. } = &expose.value else {
        panic!("expose function")
    };
    let Body::Single(stmt) = body.as_ref() else {
        panic!("expose single body")
    };
    let Stmt::Return { value: Some(value) } = stmt.as_ref() else {
        panic!("expose return")
    };
    assert!(matches!(
        value.info().return_pass.as_deref(),
        Some(ReturnPass::BorrowPlace { .. })
    ));

    let main = top_binding(&mut program, "main");
    let Expr::FuncLit { body, .. } = &main.value else {
        panic!("main function")
    };
    let Body::Block(stmts) = body.as_ref() else {
        panic!("main block")
    };
    let Stmt::Binding(alias) = &stmts[1] else {
        panic!("alias binding")
    };
    assert_eq!(alias.relation, Some(StorageRelation::Borrowed));
    let Expr::Call { result, .. } = &alias.value else {
        panic!("alias call")
    };
    assert!(matches!(
        result.as_deref(),
        Some(CallResult::Borrowed { kind: Some(_), .. })
    ));
}

#[test]
fn final_gate_rejects_return_effect_drift() {
    let mut program = checked(
        "func string expose = (string value) -> return value\nfunc i32 main = () -> {\n    val string owner = 'x'\n    val string alias = expose(owner)\n    return alias.len()\n}\n",
    );
    let expose = top_binding(&mut program, "expose");
    let Expr::FuncLit { body, .. } = &mut expose.value else {
        panic!("expose function")
    };
    let Body::Single(stmt) = body.as_mut() else {
        panic!("expose single body")
    };
    let Stmt::Return { value: Some(value) } = stmt.as_mut() else {
        panic!("expose return")
    };
    value.info_mut().return_pass = Some(Box::new(ReturnPass::OwnedValue));
    let error = validate_resolved_hir(&program).expect_err("return effect drift must fail closed");
    assert!(error.msg.contains("ReturnPass"), "{}", error.msg);
}

#[test]
fn final_gate_rejects_borrowed_result_permission_drift() {
    let mut program = checked(
        "val i32 shared = 1\nfunc i32 expose = () -> return borrow shared\nfunc i32 main = () -> {\n    var i32 alias = expose()\n    return alias\n}\n",
    );
    let main = top_binding(&mut program, "main");
    let Expr::FuncLit { body, .. } = &mut main.value else {
        panic!("main function")
    };
    let Body::Block(stmts) = body.as_mut() else {
        panic!("main block")
    };
    let Stmt::Binding(alias) = &mut stmts[0] else {
        panic!("alias binding")
    };
    let Expr::Call { result, .. } = &mut alias.value else {
        panic!("borrowed call result")
    };
    let Some(CallResult::Borrowed {
        source_writable, ..
    }) = result.as_deref_mut()
    else {
        panic!("borrowed call result")
    };
    *source_writable = true;
    let error = validate_resolved_hir(&program).expect_err("permission drift must fail closed");
    assert!(error.msg.contains("call result plan"), "{}", error.msg);
}
