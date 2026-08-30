use super::{Body, Expr, Item, Stmt};
use crate::sema::types::Ty;

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

#[test]
fn final_hir_gate_rejects_binary_result_type_drift() {
    let mut program = checked("func i32 main = () -> return 1 + 2\n");
    let Body::Single(stmt) = main_body(&mut program) else {
        panic!("fixture main must use single-statement body")
    };
    let Stmt::Return {
        value: Some(Expr::Binary { info, .. }),
    } = stmt.as_mut()
    else {
        panic!("fixture must return binary expression")
    };
    info.ty = Ty::Str;
    info.category = Some(super::ExprCategory::Value(super::ValueCategory::General));
    info.ownership_capability = None;

    let error = super::validate_resolved_hir(&program)
        .expect_err("binary result drift must fail the final HIR gate");
    assert!(
        error
            .msg
            .contains("Binary HIR 结果类型与 canonical operator contract 不一致"),
        "实际: {}",
        error.msg
    );
}

#[test]
fn final_hir_gate_rejects_index_result_type_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n    val array<i32> values = [7]\n    return values[0]\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let info = stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Return {
                value: Some(Expr::Index { info, .. }),
            } => Some(info),
            _ => None,
        })
        .expect("fixture must contain index expression");
    info.ty = Ty::Str;

    let error = super::validate_resolved_hir(&program)
        .expect_err("index result drift must fail the final HIR gate");
    assert!(
        error.msg.contains("Index HIR 下标/结果类型不一致"),
        "实际: {}",
        error.msg
    );
}

#[test]
fn final_hir_gate_rejects_impossible_resolved_conversion() {
    let mut program = checked(
        "func i32 main = () -> {\n    val u32 source = 7\n    val i32 value = from source\n    return value\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let info = stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == "value" => match &mut binding.value {
                Expr::Convert { info, .. } => Some(info),
                _ => None,
            },
            _ => None,
        })
        .expect("fixture must contain resolved conversion");
    info.ty = Ty::Bool;

    let error = super::validate_resolved_hir(&program)
        .expect_err("impossible resolved conversion must fail the final HIR gate");
    assert!(
        error
            .msg
            .contains("resolved Convert 不符合 canonical conversion contract"),
        "实际: {}",
        error.msg
    );
}
