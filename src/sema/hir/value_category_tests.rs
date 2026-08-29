use super::{Body, Expr, ExprCategory, Item, ResolvedConversion, Stmt};

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

fn binding_value<'a>(stmts: &'a mut [Stmt], name: &str) -> &'a mut Expr {
    stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == name => Some(&mut binding.value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing binding {name}"))
}

#[test]
fn resolved_hir_distinguishes_places_from_values() {
    let mut program = checked(
        "struct point { var i32 x = 7 }\nfunc i32 main = () -> {\n    var i32 n = 1\n    val point p = point()\n    val array<i32> xs = [n]\n    val i32 from_local = n\n    val i32 from_field = p.x\n    val i32 from_index = xs[0]\n    val point identity = try_from p\n    return from_local + from_field + from_index\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };

    assert_eq!(binding_value(stmts, "n").category(), Some(ExprCategory::Value));
    assert_eq!(binding_value(stmts, "p").category(), Some(ExprCategory::Value));
    assert_eq!(binding_value(stmts, "xs").category(), Some(ExprCategory::Value));
    assert_eq!(
        binding_value(stmts, "from_local").category(),
        Some(ExprCategory::Place)
    );
    assert_eq!(
        binding_value(stmts, "from_field").category(),
        Some(ExprCategory::Place)
    );
    assert_eq!(
        binding_value(stmts, "from_index").category(),
        Some(ExprCategory::Place)
    );

    let identity = binding_value(stmts, "identity");
    let Expr::Convert {
        expr: inner,
        mode: ResolvedConversion::Identity,
        info,
        ..
    } = identity
    else {
        panic!("same-struct try_from must lower to Identity Convert")
    };
    assert_eq!(inner.category(), Some(ExprCategory::Place));
    assert_eq!(info.category, Some(ExprCategory::Place));
}

#[test]
fn final_hir_gate_rejects_category_shape_drift() {
    let mut program = checked(
        "func i32 main = () -> {\n    var i32 n = 1\n    val i32 copy = n\n    return copy\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let Expr::Ident(_, Some(_), _, info) = binding_value(stmts, "copy") else {
        panic!("copy initializer must be resolved Ident")
    };
    info.category = Some(ExprCategory::Value);

    let error = super::validate_resolved_hir(&program)
        .expect_err("category drift must fail the final HIR gate");
    assert!(
        error
            .msg
            .contains("Expr category 与 resolved HIR 形状不一致"),
        "实际: {}",
        error.msg
    );
}

#[test]
fn final_hir_gate_rejects_missing_category() {
    let mut program = checked("func i32 main = () -> return 1\n");
    let Body::Single(stmt) = main_body(&mut program) else {
        panic!("fixture main must use single-statement body")
    };
    let Stmt::Return {
        value: Some(Expr::Int(_, _, info)),
    } = stmt.as_mut()
    else {
        panic!("fixture must return integer literal")
    };
    info.category = None;

    let error = super::validate_resolved_hir(&program)
        .expect_err("missing category must fail the final HIR gate");
    assert!(
        error.msg.contains("Expr category 未 finalization"),
        "实际: {}",
        error.msg
    );
}
