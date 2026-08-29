use super::{Body, Expr, Item, Stmt, StorageRelation};

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

fn binding<'a>(stmts: &'a mut [Stmt], name: &str) -> &'a mut super::Binding {
    stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == name => Some(binding),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing binding {name}"))
}

#[test]
fn resolved_hir_freezes_only_proven_owning_relations() {
    let mut program = checked(
        "struct point { val i32 x = 1 }\nfunc point make = () -> point()\nfunc i32 main = () -> {\n    val i32 scalar = 1\n    val point fresh = point()\n    val i32 scalar_copy = scalar\n    val point from_func = make()\n    val point from_place = fresh\n    return scalar_copy\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };

    assert_eq!(binding(stmts, "scalar").relation, Some(StorageRelation::Owning));
    assert_eq!(binding(stmts, "fresh").relation, Some(StorageRelation::Owning));
    assert_eq!(
        binding(stmts, "scalar_copy").relation,
        Some(StorageRelation::Owning),
        "标量 Place 普通读取是值复制，可直接固定 owning slot"
    );
    assert_eq!(
        binding(stmts, "from_func").relation,
        None,
        "动态函数返回必须等待 return effect"
    );
    assert_eq!(
        binding(stmts, "from_place").relation,
        None,
        "动态 Place 普通读取必须等待显式 DeepClone HIR"
    );
}

#[test]
fn final_hir_gate_rejects_storage_relation_drift() {
    let mut program = checked(
        "struct point { val i32 x = 1 }\nfunc i32 main = () -> {\n    val point p = point()\n    return 0\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    binding(stmts, "p").relation = None;

    let error = super::validate_resolved_hir(&program)
        .expect_err("relation drift must fail the final HIR gate");
    assert!(
        error
            .msg
            .contains("Binding storage relation 与 initializer semantic category 不一致"),
        "实际: {}",
        error.msg
    );
}
