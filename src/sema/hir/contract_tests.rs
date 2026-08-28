use super::{Body, CallTarget, Expr, Item, Stmt};
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
fn resolved_hir_rejects_constructor_arg_type_drift() {
    let mut program = checked(
        "struct point { val i32 x }\nfunc i32 main = () -> {\n    val point p = point(x = 9)\n    return p.x\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let arg = stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == "p" => match &mut binding.value {
                Expr::Call {
                    args,
                    target: CallTarget::StructConstructor { .. },
                    ..
                } => args.first_mut(),
                _ => None,
            },
            _ => None,
        })
        .expect("fixture must contain constructor arg");
    let Expr::Int(_, _, info) = &mut arg.value else {
        panic!("fixture constructor arg must be an integer literal")
    };
    info.ty = Ty::Str;

    let error = super::validate_resolved_hir(&program)
        .expect_err("constructor arg type drift must fail the final HIR gate");
    assert!(
        error
            .msg
            .contains("构造器实参类型与已解析字段类型不一致"),
        "实际: {}",
        error.msg
    );
}

#[test]
fn resolved_hir_rejects_missing_required_constructor_field() {
    let mut program = checked(
        "struct point { val i32 x val i32 y }\nfunc i32 main = () -> {\n    val point p = point(x = 1, y = 2)\n    return p.x\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let (args, indices) = stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == "p" => match &mut binding.value {
                Expr::Call {
                    args,
                    target:
                        CallTarget::StructConstructor {
                            arg_field_indices, ..
                        },
                    ..
                } => Some((args, arg_field_indices)),
                _ => None,
            },
            _ => None,
        })
        .expect("fixture must contain constructor");
    args.pop();
    indices.pop();

    let error = super::validate_resolved_hir(&program)
        .expect_err("missing required field must fail the final HIR gate");
    assert!(
        error.msg.contains("结构体构造 target 遗漏无默认值字段"),
        "实际: {}",
        error.msg
    );
}

#[test]
fn resolved_hir_rejects_field_result_type_drift() {
    let mut program = checked(
        "struct point { val i32 x = 7 }\nfunc i32 main = () -> {\n    val point p = point()\n    return p.x\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let info = stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Return {
                value: Some(Expr::Field { info, .. }),
            } => Some(info),
            _ => None,
        })
        .expect("fixture must contain field access");
    info.ty = Ty::Str;

    let error = super::validate_resolved_hir(&program)
        .expect_err("field result type drift must fail the final HIR gate");
    assert!(
        error
            .msg
            .contains("字段表达式类型与已解析字段声明不一致"),
        "实际: {}",
        error.msg
    );
}

#[test]
fn final_hir_gate_rejects_duplicate_binding_id() {
    let mut program = checked(
        "val i32 left = 1\nval i32 right = 2\nfunc i32 main = () -> return left + right\n",
    );
    let left_id = program
        .items
        .iter()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "left" => Some(binding.binding_id),
            _ => None,
        })
        .expect("left binding");
    let right = program
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "right" => Some(binding),
            _ => None,
        })
        .expect("right binding");
    right.binding_id = left_id;

    let error = super::validate_resolved_hir(&program)
        .expect_err("duplicate BindingId must fail the final HIR gate");
    assert!(error.msg.contains("BindingId 重复"), "实际: {}", error.msg);
}

#[test]
fn final_hir_gate_rejects_ident_type_drift_from_binding_id() {
    let mut program = checked("val i32 value = 7\nfunc i32 main = () -> return value\n");
    let Body::Single(stmt) = main_body(&mut program) else {
        panic!("fixture main must use single-statement body")
    };
    let Stmt::Return {
        value: Some(Expr::Ident(_, Some(_), _, info)),
    } = stmt.as_mut()
    else {
        panic!("fixture must return resolved ident")
    };
    info.ty = Ty::Str;

    let error = super::validate_resolved_hir(&program)
        .expect_err("Ident type drift must fail the final HIR gate");
    assert!(
        error
            .msg
            .contains("Ident 静态类型与 BindingId 声明类型不一致"),
        "实际: {}",
        error.msg
    );
}

#[test]
fn final_hir_gate_rejects_assign_target_type_drift() {
    let mut program = checked(
        "val string text = 'x'\nfunc i32 main = () -> {\n    var i32 n = 0\n    n = 1\n    return n\n}\n",
    );
    let text_id = program
        .items
        .iter()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "text" => Some(binding.binding_id),
            _ => None,
        })
        .expect("text binding");
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let target_id = stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Assign { target_id, .. } => Some(target_id),
            _ => None,
        })
        .expect("fixture must contain assignment");
    *target_id = text_id;

    let error = super::validate_resolved_hir(&program)
        .expect_err("Assign target type drift must fail the final HIR gate");
    assert!(
        error
            .msg
            .contains("Assign RHS 类型与 BindingId 声明类型不一致"),
        "实际: {}",
        error.msg
    );
}
