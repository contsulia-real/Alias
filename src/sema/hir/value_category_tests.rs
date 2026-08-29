use super::{Body, Expr, ExprCategory, Item, ResolvedConversion, Stmt, ValueCategory};

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
fn resolved_hir_distinguishes_places_and_proven_owned_temporaries() {
    let mut program = checked(
        "struct point { var i32 x = 7 }\nfunc point make = () -> point()\nfunc i32 main = () -> {\n    var i32 n = 1\n    val point p = point()\n    val array<i32> xs = [n]\n    val point from_func = make()\n    val i32 from_local = n\n    val i32 from_field = p.x\n    val i32 from_index = xs[0]\n    val point identity_place = try_from p\n    val point identity_owned = try_from point()\n    return from_local + from_field + from_index\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };

    assert_eq!(
        binding_value(stmts, "n").category(),
        Some(ExprCategory::Value(ValueCategory::InlineValue))
    );
    assert_eq!(
        binding_value(stmts, "p").category(),
        Some(ExprCategory::Value(ValueCategory::OwnedTemporary))
    );
    assert_eq!(
        binding_value(stmts, "xs").category(),
        Some(ExprCategory::Value(ValueCategory::OwnedTemporary))
    );
    assert_eq!(
        binding_value(stmts, "from_func").category(),
        Some(ExprCategory::Value(ValueCategory::General)),
        "动态函数返回必须等待 return effect，不能只按结果类型猜 owner"
    );
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

    let identity_place = binding_value(stmts, "identity_place");
    let Expr::Convert {
        expr: inner,
        mode: ResolvedConversion::Identity,
        info,
        ..
    } = identity_place
    else {
        panic!("same-struct try_from must lower to Identity Convert")
    };
    assert_eq!(inner.category(), Some(ExprCategory::Place));
    assert_eq!(info.category, Some(ExprCategory::Place));

    let identity_owned = binding_value(stmts, "identity_owned");
    let Expr::Convert {
        expr: inner,
        mode: ResolvedConversion::Identity,
        info,
        ..
    } = identity_owned
    else {
        panic!("same-struct constructor try_from must lower to Identity Convert")
    };
    assert_eq!(
        inner.category(),
        Some(ExprCategory::Value(ValueCategory::OwnedTemporary))
    );
    assert_eq!(
        info.category,
        Some(ExprCategory::Value(ValueCategory::OwnedTemporary))
    );
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
    info.category = Some(ExprCategory::Value(ValueCategory::General));

    let error = super::validate_resolved_hir(&program)
        .expect_err("category drift must fail the final HIR gate");
    assert!(
        error
            .msg
            .contains("Expr category 与 resolved HIR ownership 形状不一致"),
        "实际: {}",
        error.msg
    );
}

#[test]
fn final_hir_gate_rejects_owned_temporary_drift() {
    let mut program = checked(
        "struct point { val i32 x = 1 }\nfunc i32 main = () -> {\n    val point p = point()\n    return 0\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let Expr::Call { info, .. } = binding_value(stmts, "p") else {
        panic!("point initializer must be resolved constructor Call")
    };
    info.category = Some(ExprCategory::Value(ValueCategory::General));

    let error = super::validate_resolved_hir(&program)
        .expect_err("OwnedTemporary drift must fail the final HIR gate");
    assert!(
        error
            .msg
            .contains("Expr category 与 resolved HIR ownership 形状不一致"),
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
