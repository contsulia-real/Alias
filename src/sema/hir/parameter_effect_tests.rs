use super::{
    validate_resolved_hir, ArgumentPass, BindingOwner, Body, DestroyNode, Expr, Item, Stmt,
};
use crate::sema::types::{ParamEffect, Ty};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

#[test]
fn iterator_receiver_pass_is_required_and_rechecked_at_the_final_gate() {
    let source = "func i32 main = () -> { val array<i32> values = [1]\nval iterator<i32> it = values.iterator()\nfor i32 item in it { println item }\nreturn 0 }";
    for missing in [true, false] {
        let mut program = checked(source);
        let main = top_binding(&mut program, "main");
        let Expr::FuncLit { body, .. } = &mut main.value else { panic!("main body") };
        let Body::Block(stmts) = body.as_mut() else { panic!("main block") };
        let Stmt::Binding(binding) = &mut stmts[1] else { panic!("iterator binding") };
        let Expr::MethodCall { receiver_pass, .. } = &mut binding.value else { panic!("iterator creation") };
        let Some(ArgumentPass::ReadBorrow { loan_id, source }) = receiver_pass.take().map(|pass| *pass) else { panic!("read loan") };
        if !missing {
            *receiver_pass = Some(Box::new(ArgumentPass::WriteBorrow { loan_id, source }));
        }
        assert!(validate_resolved_hir(&program).is_err());
    }
}

#[test]
fn builtin_temporary_receiver_destruction_is_frozen_and_rechecked() {
    let mut program = checked("func i32 main = () -> return ' value '.trim().len()");
    let main = top_binding(&mut program, "main");
    let Expr::FuncLit { body, .. } = &mut main.value else {
        panic!("main body")
    };
    let Body::Single(stmt) = body.as_mut() else {
        panic!("main single body")
    };
    let Stmt::Return {
        value:
            Some(Expr::MethodCall {
                recv,
                receiver_pass,
                ..
            }),
    } = stmt.as_mut()
    else {
        panic!("main return method")
    };
    let Some(ArgumentPass::BorrowTemporary { destroy_plan, .. }) = receiver_pass.as_deref_mut()
    else {
        panic!("builtin temporary receiver pass")
    };
    assert!(matches!(recv.as_ref(), Expr::MethodCall { .. }));
    assert_eq!(destroy_plan.nodes, vec![DestroyNode::String]);
    destroy_plan.nodes[0] = DestroyNode::Inline;

    let error = validate_resolved_hir(&program)
        .expect_err("builtin receiver destruction drift must fail closed");
    assert!(
        error.msg.contains("temporary argument destruction plan"),
        "{}",
        error.msg
    );
}

#[test]
fn iteration_source_pass_is_required_and_rechecked_at_the_final_gate() {
    let source = "func i32 main = () -> { val array<i32> values = [1]\nfor i32 item in values { println item }\nreturn 0 }";
    for missing in [true, false] {
        let mut program = checked(source);
        let main = top_binding(&mut program, "main");
        let Expr::FuncLit { body, .. } = &mut main.value else { panic!("main body") };
        let Body::Block(stmts) = body.as_mut() else { panic!("main block") };
        let Stmt::For { source_pass, .. } = &mut stmts[1] else { panic!("for") };
        let Some(ArgumentPass::ReadBorrow { loan_id, source }) = source_pass.take() else { panic!("read loan") };
        if !missing {
            *source_pass = Some(ArgumentPass::WriteBorrow { loan_id, source });
        }
        assert!(validate_resolved_hir(&program).is_err());
    }
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
    assert_eq!(
        params[0]
            .destroy_plan
            .as_deref()
            .expect("parameter destruction plan")
            .nodes,
        vec![DestroyNode::String]
    );

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
fn parameter_and_self_destruction_plans_are_fail_closed() {
    let mut program = checked(
        "func i32 string.size = () -> return self.len()\n\
func i32 length = (string value) -> return value.len()\n\
func i32 main = () -> return 'x'.size() + length('y')\n",
    );
    let method = top_binding(&mut program, "size");
    let BindingOwner::Method {
        self_destroy_plan, ..
    } = &method.owner
    else {
        panic!("size method owner")
    };
    assert_eq!(
        self_destroy_plan
            .as_deref()
            .expect("self destruction plan")
            .nodes,
        vec![DestroyNode::String]
    );

    let length = top_binding(&mut program, "length");
    let Expr::FuncLit { params, .. } = &mut length.value else {
        panic!("length function literal")
    };
    params[0]
        .destroy_plan
        .as_deref_mut()
        .expect("parameter destruction plan")
        .nodes[0] = DestroyNode::Inline;

    let error = validate_resolved_hir(&program).expect_err("parameter plan drift must fail closed");
    assert!(
        error.msg.contains("parameter destruction plan"),
        "{}",
        error.msg
    );

    let length = top_binding(&mut program, "length");
    let Expr::FuncLit { params, .. } = &mut length.value else {
        panic!("length function literal")
    };
    params[0]
        .destroy_plan
        .as_deref_mut()
        .expect("parameter destruction plan")
        .nodes[0] = DestroyNode::String;
    let method = top_binding(&mut program, "size");
    let BindingOwner::Method {
        self_destroy_plan, ..
    } = &mut method.owner
    else {
        panic!("size method owner")
    };
    self_destroy_plan
        .as_deref_mut()
        .expect("self destruction plan")
        .nodes[0] = DestroyNode::Inline;
    let error = validate_resolved_hir(&program).expect_err("self plan drift must fail closed");
    assert!(
        error.msg.contains("self parameter destruction plan"),
        "{}",
        error.msg
    );
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
fn borrowed_temporary_argument_destruction_is_frozen_and_rechecked() {
    let mut program = checked(
        "func i32 length = (string value) -> return value.len()\nfunc i32 main = () -> { return length('temporary') }\n",
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
    let Some(ArgumentPass::BorrowTemporary { destroy_plan, .. }) = &mut args[0].pass else {
        panic!("borrowed temporary pass")
    };
    assert_eq!(destroy_plan.nodes, vec![DestroyNode::String]);
    destroy_plan.nodes[0] = DestroyNode::Inline;

    let error = validate_resolved_hir(&program)
        .expect_err("temporary argument destruction drift must fail closed");
    assert!(
        error.msg.contains("temporary argument destruction plan"),
        "{}",
        error.msg
    );
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
