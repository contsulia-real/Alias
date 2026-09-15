use super::{
    validate_resolved_hir, Body, Expr, Item, LoanId, PointerViewSource, RuntimeCheckRequirement,
    Stmt,
};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

#[test]
fn final_gate_rejects_reinterpret_alignment_fact_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n    val i64 owner = 7\n    val ptr<i32> view = reinterpret<i32>(refer(owner))\n    return 0\n}\n",
    );
    let main = program
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "main" => Some(binding),
            _ => None,
        })
        .unwrap();
    let Expr::FuncLit { body, .. } = &mut main.value else {
        panic!("main function")
    };
    let Body::Block(stmts) = body.as_mut() else {
        panic!("main block")
    };
    let view = stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == "view" => Some(&mut binding.value),
            _ => None,
        })
        .unwrap();
    let Expr::ReinterpretPointer {
        alignment_check, ..
    } = view
    else {
        panic!("reinterpret HIR")
    };
    *alignment_check = RuntimeCheckRequirement::Proven;
    let error = validate_resolved_hir(&program).expect_err("alignment fact drift must fail closed");
    assert!(error.msg.contains("alignment fact"), "{}", error.msg);
}

#[test]
fn final_gate_rejects_reinterpret_source_origin_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n    val i64 owner = 7\n    val ptr<i64> source = refer(owner)\n    val ptr<i32> view = reinterpret<i32>(source)\n    return 0\n}\n",
    );
    let main = program
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "main" => Some(binding),
            _ => None,
        })
        .unwrap();
    let Expr::FuncLit { body, .. } = &mut main.value else {
        panic!("main function")
    };
    let Body::Block(stmts) = body.as_mut() else {
        panic!("main block")
    };
    let view = stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == "view" => Some(&mut binding.value),
            _ => None,
        })
        .unwrap();
    let Expr::ReinterpretPointer { source_origin, .. } = view else {
        panic!("reinterpret HIR")
    };
    *source_origin = PointerViewSource::Loan(LoanId(u32::MAX));
    let error = validate_resolved_hir(&program).expect_err("source-origin drift must fail closed");
    assert!(error.msg.contains("source origin fact"), "{}", error.msg);
}
