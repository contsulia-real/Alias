use super::{validate_resolved_hir, ArgumentPass, Body, Expr, Item, Stmt};
use crate::sema::types::{ParamEffect, Ty};

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

fn set_only_effect(ty: &mut Ty, effect: ParamEffect) {
    let Ty::Func { param_effects, .. } = ty else {
        panic!("expected function type")
    };
    *param_effects = Some(vec![effect]);
}

#[test]
fn parameter_effects_freeze_signature_param_and_caller_pass() {
    let mut program = checked(
        "func i32 length = (string value) -> return value.len()\nfunc i32 main = () -> {\n    val string owner = 'x'\n    return length(owner)\n}\n",
    );
    let length = top_binding(&mut program, "length");
    let Ty::Func {
        param_effects: Some(effects),
        ..
    } = &length.ty
    else {
        panic!("length signature effects")
    };
    assert_eq!(effects.as_slice(), [ParamEffect::ReadBorrow]);
    let Expr::FuncLit { params, .. } = &length.value else {
        panic!("length function literal")
    };
    assert_eq!(params[0].effect, Some(ParamEffect::ReadBorrow));

    let main = top_binding(&mut program, "main");
    let Expr::FuncLit { body, .. } = &main.value else {
        panic!("main function literal")
    };
    let Body::Block(stmts) = body.as_ref() else {
        panic!("main block")
    };
    let Some(Stmt::Return {
        value: Some(Expr::Call { args, .. }),
    }) = stmts.last()
    else {
        panic!("main return call")
    };
    assert!(matches!(
        args[0].pass,
        Some(ArgumentPass::ReadBorrow { .. })
    ));
}

#[test]
fn final_gate_rejects_argument_pass_drift() {
    let mut program = checked(
        "func i32 length = (string value) -> return value.len()\nfunc i32 main = () -> {\n    val string owner = 'x'\n    return length(owner)\n}\n",
    );
    let main = top_binding(&mut program, "main");
    let Expr::FuncLit { body, .. } = &mut main.value else {
        panic!("main function literal")
    };
    let Body::Block(stmts) = body.as_mut() else {
        panic!("main block")
    };
    let Some(Stmt::Return {
        value: Some(Expr::Call { args, .. }),
    }) = stmts.last_mut()
    else {
        panic!("main return call")
    };
    args[0].pass = Some(ArgumentPass::Owned);
    let error = validate_resolved_hir(&program).expect_err("argument pass drift must fail closed");
    assert!(error.msg.contains("argument pass"), "{}", error.msg);
}

#[test]
fn final_gate_recomputes_parameter_effects_from_the_body() {
    let mut program = checked(
        "func i32 length = (string value) -> return value.len()\nfunc i32 main = () -> return 0\n",
    );
    let length = top_binding(&mut program, "length");
    set_only_effect(&mut length.ty, ParamEffect::Owned);
    set_only_effect(&mut length.value.info_mut().ty, ParamEffect::Owned);
    let Expr::FuncLit { params, .. } = &mut length.value else {
        panic!("length function literal")
    };
    params[0].effect = Some(ParamEffect::Owned);

    let error = validate_resolved_hir(&program).expect_err("effect/body drift must fail closed");
    assert!(
        error.msg.contains("parameter effects") && error.msg.contains("fixed-point"),
        "{}",
        error.msg
    );
}
