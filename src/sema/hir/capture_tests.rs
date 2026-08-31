use super::{validate_resolved_hir, Body, BorrowKind, Expr, Item, Place, Stmt};
use crate::sema::types::{IntW, Ty};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

fn local_function<'a>(program: &'a mut super::CheckedProgram, name: &str) -> &'a mut super::Expr {
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
    &mut stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == name => Some(binding),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing function {name}"))
        .value
}

#[test]
fn capture_freezes_root_place_id_and_read_kind() {
    let mut program = checked(
        "func i32 main = () -> {\n    val string owner = 'x'\n    func i32 read = () -> return owner.len()\n    return read()\n}\n",
    );
    let Expr::FuncLit { captures, .. } = local_function(&mut program, "read") else {
        panic!("read must be a function literal")
    };
    let [capture] = captures.as_slice() else {
        panic!("read must have one capture")
    };
    assert_eq!(capture.kind, Some(BorrowKind::Read));
    let Place::Local { binding_id, .. } = &capture.source else {
        panic!("capture source must be a root Local Place")
    };
    assert_eq!(*binding_id, capture.binding_id);
}

#[test]
fn capture_body_write_freezes_write_kind() {
    let mut program = checked(
        "func i32 main = () -> {\n    var i32 owner = 1\n    func unit change = () -> increase owner\n    change()\n    return owner\n}\n",
    );
    let Expr::FuncLit { captures, .. } = local_function(&mut program, "change") else {
        panic!("change must be a function literal")
    };
    assert_eq!(captures[0].kind, Some(BorrowKind::Write));
}

#[test]
fn final_gate_rejects_capture_kind_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n    var i32 owner = 1\n    func unit change = () -> increase owner\n    change()\n    return owner\n}\n",
    );
    let Expr::FuncLit { captures, .. } = local_function(&mut program, "change") else {
        panic!("change must be a function literal")
    };
    captures[0].kind = Some(BorrowKind::Read);
    let error = validate_resolved_hir(&program).expect_err("capture kind drift must fail closed");
    assert!(
        error.msg.contains("capture loan kind") && error.msg.contains("漂移"),
        "{}",
        error.msg
    );
}

#[test]
fn final_gate_rejects_capture_source_type_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n    val string owner = 'x'\n    func i32 read = () -> return owner.len()\n    return read()\n}\n",
    );
    let Expr::FuncLit { captures, .. } = local_function(&mut program, "read") else {
        panic!("read must be a function literal")
    };
    let Place::Local { info, .. } = &mut captures[0].source else {
        panic!("capture source must be Local")
    };
    info.ty = Ty::Int(IntW::W32);
    let error = validate_resolved_hir(&program).expect_err("capture source drift must fail closed");
    assert!(
        error.msg.contains("capture") && error.msg.contains("漂移"),
        "{}",
        error.msg
    );
}
