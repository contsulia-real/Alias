use super::{validate_resolved_hir, Body, Expr, Item, Place, RuntimeCheckRequirement, Stmt};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

fn local_value<'a>(program: &'a mut super::CheckedProgram, name: &str) -> &'a mut Expr {
    let main = program
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "main" => Some(binding),
            _ => None,
        })
        .expect("main binding");
    let Expr::FuncLit { body, .. } = &mut main.value else {
        panic!("main must be a function")
    };
    let Body::Block(stmts) = body.as_mut() else {
        panic!("fixture main must have a block")
    };
    stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == name => Some(&mut binding.value),
            _ => None,
        })
        .expect("local binding")
}

#[test]
fn direct_array_literal_constant_index_freezes_static_proof() {
    let mut program = checked(
        "func i32 main = () -> {\n    val i32 selected = [10, 20][1]\n    return selected\n}\n",
    );
    let Expr::Index { bounds_check, .. } = local_value(&mut program, "selected") else {
        panic!("selected must be an index expression")
    };
    assert_eq!(*bounds_check, RuntimeCheckRequirement::Proven);

    *bounds_check = RuntimeCheckRequirement::Required;
    let error = validate_resolved_hir(&program)
        .expect_err("a changed bounds-check decision must fail the final HIR gate");
    assert!(
        error.msg.contains("bounds-check fact 漂移"),
        "实际: {}",
        error.msg
    );
}

#[test]
fn array_place_index_keeps_runtime_check_requirement() {
    let mut program = checked(
        "func i32 main = () -> {\n    val array<i32> values = [10, 20]\n    val i32 at = 1\n    val i32 selected = values[at]\n    return selected\n}\n",
    );
    let Expr::ReadPlace { source, .. } = local_value(&mut program, "selected") else {
        panic!("selected must be an owning Place read")
    };
    let Place::Index { bounds_check, .. } = source.as_mut() else {
        panic!("selected source must be an Index Place")
    };
    assert_eq!(*bounds_check, RuntimeCheckRequirement::Required);
}

#[test]
fn direct_array_literal_constant_oob_is_rejected_statically() {
    let tokens = crate::lexer::lex("func i32 main = () -> return [10, 20][2]\n").unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    let error = crate::sema::check(program)
        .expect_err("a statically known out-of-bounds index must not reach codegen");
    assert!(
        error.msg.contains("数组字面量的常量下标越界"),
        "实际: {}",
        error.msg
    );
}
