use super::{validate_resolved_hir, Body, Expr, ExprCategory, Item, Stmt, ValueCategory};
use crate::sema::types::{IntW, Ty};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

fn main_stmts(program: &mut super::CheckedProgram) -> &mut Vec<Stmt> {
    let main = program
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "main" => Some(binding),
            _ => None,
        })
        .expect("main binding");
    let Expr::FuncLit { body, .. } = &mut main.value else {
        panic!("main must be a function")
    };
    let Body::Block(stmts) = body.as_mut() else {
        panic!("fixture main must have a block")
    };
    stmts
}

#[test]
fn final_gate_rejects_raw_allocation_pointee_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n    val ptr<i32>? value = malloc<i32>()\n    free(move value)\n    return 0\n}\n",
    );
    let Stmt::Binding(binding) = &mut main_stmts(&mut program)[0] else {
        panic!("first statement must bind the allocation")
    };
    let Expr::RawAllocate { element_ty, .. } = &mut binding.value else {
        panic!("binding must contain RawAllocate")
    };
    *element_ty = Ty::Int(IntW::W64);

    let error = validate_resolved_hir(&program).expect_err("pointee drift must fail closed");
    assert!(
        error.msg.contains("RawAllocate pointee/count contract"),
        "{}",
        error.msg
    );
}

#[test]
fn final_gate_rejects_free_operand_category_drift() {
    let mut program =
        checked("func i32 main = () -> {\n    free(malloc<i32>())\n    return 0\n}\n");
    let Stmt::Expr { expr, .. } = &mut main_stmts(&mut program)[0] else {
        panic!("first statement must be free")
    };
    let Expr::FreeRawAllocation { pointer, .. } = expr else {
        panic!("statement must contain FreeRawAllocation")
    };
    pointer.info_mut().category = Some(ExprCategory::Value(ValueCategory::General));

    let error =
        validate_resolved_hir(&program).expect_err("free capability drift must fail closed");
    assert!(error.msg.contains("Expr category"), "{}", error.msg);
}
