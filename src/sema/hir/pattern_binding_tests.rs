use super::{
    validate_resolved_hir, Body, DeepClonePlan, Expr, Item, PatternBindingOperation, Stmt,
};

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

fn match_expr<'a>(stmts: &'a mut [Stmt], binding_name: &str) -> &'a mut Expr {
    stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == binding_name => Some(&mut binding.value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing binding {binding_name}"))
}

#[test]
fn pattern_bindings_freeze_copy_clone_and_transfer_operations() {
    let mut program = checked(
        r#"
struct cell { var i32 value = 0 }
func i32 main = () -> {
    val cell source = cell(value = 7)
    val i32 cloned = match source { item -> item.value }
    val i32 transferred = match cell(value = 8) { item -> item.value }
    val result<cell, string> wrapped = ok(cell(value = 9))
    val i32 payload = match wrapped {
        ok(item) -> item.value
        err(_) -> 0
    }
    return cloned + transferred + payload
}
"#,
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };

    let Expr::Match { arms, .. } = match_expr(stmts, "cloned") else {
        panic!("cloned must be a match expression")
    };
    assert_eq!(
        arms[0].binding_operation,
        Some(PatternBindingOperation::DeepClone(DeepClonePlan::Struct {
            name: "cell".into(),
            fields: vec![DeepClonePlan::Inline],
        }))
    );

    let Expr::Match { arms, .. } = match_expr(stmts, "transferred") else {
        panic!("transferred must be a match expression")
    };
    assert_eq!(
        arms[0].binding_operation,
        Some(PatternBindingOperation::OwnershipTransfer)
    );

    let Expr::Match { arms, .. } = match_expr(stmts, "payload") else {
        panic!("payload must be a match expression")
    };
    assert_eq!(
        arms[0].binding_operation,
        Some(PatternBindingOperation::DeepClone(DeepClonePlan::Struct {
            name: "cell".into(),
            fields: vec![DeepClonePlan::Inline],
        }))
    );
    assert_eq!(arms[1].binding_operation, None);
}

#[test]
fn final_hir_gate_rejects_pattern_binding_operation_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n\
    val string source = 'x'\n\
    val i32 length = match source { text -> text.len() }\n\
    return length\n\
}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let Expr::Match { arms, .. } = match_expr(stmts, "length") else {
        panic!("length must be a match expression")
    };
    arms[0].binding_operation = Some(PatternBindingOperation::OwnershipTransfer);

    let error = validate_resolved_hir(&program).expect_err("operation drift must fail closed");
    assert!(
        error.msg.contains("Pattern binding operation"),
        "{}",
        error.msg
    );
}
