use super::{
    AssignmentOperation, BindingOperation, Body, Expr, Item, OwningWrite, Stmt,
    validate_resolved_hir,
};

#[test]
fn binding_initialization_operations_are_frozen() {
    let mut program = checked(
        r#"
struct box { var i32 value = 0 }
val box global = box(value = 1)
func box fresh = () -> return box(value = 2)
func i32 main = () -> {
    val i32 scalar = 3
    val i32 copied = scalar
    val box owner = fresh()
    val box cloned = global
    val box transferred = move owner
    val box alias = borrow cloned
    return alias.value + copied + transferred.value
}
"#,
    );
    for item in &program.items {
        if let Item::Binding(binding) = item {
            assert_eq!(
                binding.operation,
                Some(BindingOperation::Initialize(OwningWrite::OwnershipTransfer))
            );
        }
    }
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let operations = stmts
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Binding(binding) => Some(binding.operation),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        operations,
        [
            Some(BindingOperation::Initialize(OwningWrite::InlineCopy)),
            Some(BindingOperation::Initialize(OwningWrite::InlineCopy)),
            Some(BindingOperation::Initialize(OwningWrite::OwnershipTransfer)),
            Some(BindingOperation::Initialize(OwningWrite::OwnershipTransfer)),
            Some(BindingOperation::Initialize(OwningWrite::OwnershipTransfer)),
            Some(BindingOperation::BindBorrowedAlias),
        ]
    );
}

#[test]
fn final_hir_gate_rejects_missing_or_drifted_binding_operation() {
    for operation in [
        None,
        Some(BindingOperation::BindBorrowedAlias),
        Some(BindingOperation::Initialize(OwningWrite::OwnershipTransfer)),
    ] {
        let mut program = checked("func i32 main = () -> {\nval i32 value = 1\nreturn value\n}\n");
        let Body::Block(stmts) = main_body(&mut program) else {
            panic!("fixture main must use block body")
        };
        let Stmt::Binding(binding) = &mut stmts[0] else {
            panic!("fixture starts with binding")
        };
        binding.operation = operation;
        let error =
            validate_resolved_hir(&program).expect_err("binding operation must fail closed");
        assert!(error.msg.contains("Binding operation"), "{}", error.msg);
    }
}

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
