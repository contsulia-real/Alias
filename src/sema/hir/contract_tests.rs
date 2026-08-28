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

    let error = super::validate::validate_resolved_hir(&program)
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

    let error = super::validate::validate_resolved_hir(&program)
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

    let error = super::validate::validate_resolved_hir(&program)
        .expect_err("field result type drift must fail the final HIR gate");
    assert!(
        error
            .msg
            .contains("字段表达式类型与已解析字段声明不一致"),
        "实际: {}",
        error.msg
    );
}
