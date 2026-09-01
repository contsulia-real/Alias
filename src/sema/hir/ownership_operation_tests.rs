use super::{validate_resolved_hir, AssignmentOperation, Body, Expr, Item, OwningWrite, Stmt};

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
    body
}

#[test]
fn assignment_ownership_operations_are_frozen() {
    let mut program = checked(
        r#"
struct box { var i32 value = 0 }
func i32 main = () -> {
    var i32 scalar = 1
    var box owner = box()
    var i32 alias = borrow scalar
    scalar = 2
    owner = box(value = 3)
    alias = borrow scalar
    owner.value = 4
    return scalar
}
"#,
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };

    let operations = stmts
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Assign { operation, .. } => *operation,
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        operations,
        [
            AssignmentOperation::Replace(OwningWrite::InlineCopy),
            AssignmentOperation::Replace(OwningWrite::OwnershipTransfer),
            AssignmentOperation::RebindBorrowedAlias,
            AssignmentOperation::Replace(OwningWrite::InlineCopy),
        ]
    );
}

#[test]
fn final_hir_gate_rejects_assignment_operation_drift() {
    let mut program =
        checked("func i32 main = () -> {\nvar i32 value = 1\nvalue = 2\nreturn value\n}\n");
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let operation = stmts.iter_mut().find_map(|stmt| match stmt {
        Stmt::Assign { operation, .. } => Some(operation),
        _ => None,
    });
    *operation.expect("assignment") = Some(AssignmentOperation::RebindBorrowedAlias);

    let error = validate_resolved_hir(&program).expect_err("operation drift must fail closed");
    assert!(error.msg.contains("Assignment operation"), "{}", error.msg);
}
