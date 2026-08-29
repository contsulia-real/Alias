use super::{place_relation, Body, Expr, Item, Place, PlaceRelation, Stmt};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

fn assignment_targets(program: &super::CheckedProgram) -> Vec<&Place> {
    let main = program
        .items
        .iter()
        .find_map(|item| match item {
            Item::Binding(binding) if binding.name == "main" => Some(binding),
            _ => None,
        })
        .expect("main binding");
    let Expr::FuncLit { body, .. } = &main.value else {
        panic!("main value must be function literal")
    };
    let Body::Block(stmts) = body.as_ref() else {
        panic!("fixture main must use block body")
    };
    stmts
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Assign { target, .. } => Some(target),
            _ => None,
        })
        .collect()
}

#[test]
fn different_local_roots_are_disjoint() {
    let program = checked(
        "func i32 main = () -> {\n    var i32 a = 0\n    var i32 b = 0\n    a = 1\n    b = 2\n    return a + b\n}\n",
    );
    let targets = assignment_targets(&program);
    assert_eq!(place_relation(targets[0], targets[1]), PlaceRelation::Disjoint);
}

#[test]
fn different_fields_are_disjoint_and_ancestor_overlaps() {
    let program = checked(
        "struct pair { var i32 left = 0 var i32 right = 0 }\nfunc i32 main = () -> {\n    val pair p = pair()\n    p.left = 1\n    p.right = 2\n    return p.left + p.right\n}\n",
    );
    let targets = assignment_targets(&program);
    assert_eq!(place_relation(targets[0], targets[1]), PlaceRelation::Disjoint);
    let Place::Field { base, .. } = targets[0] else {
        panic!("fixture target must be Field")
    };
    assert_eq!(place_relation(targets[0], base), PlaceRelation::Overlap);
}

#[test]
fn constant_indices_prove_disjoint_or_overlap() {
    let program = checked(
        "struct cell { var i32 value = 0 }\nfunc i32 main = () -> {\n    val array<cell> cells = [cell(), cell()]\n    cells[0].value = 1\n    cells[1].value = 2\n    cells[0].value = 3\n    return cells[0].value + cells[1].value\n}\n",
    );
    let targets = assignment_targets(&program);
    assert_eq!(place_relation(targets[0], targets[1]), PlaceRelation::Disjoint);
    assert_eq!(place_relation(targets[0], targets[2]), PlaceRelation::Overlap);
}

#[test]
fn independent_dynamic_indices_are_unknown() {
    let program = checked(
        "struct cell { var i32 value = 0 }\nfunc i32 main = () -> {\n    val array<cell> cells = [cell(), cell()]\n    var i32 i = 0\n    var i32 j = 1\n    cells[i].value = 1\n    cells[j].value = 2\n    return cells[0].value + cells[1].value\n}\n",
    );
    let targets = assignment_targets(&program);
    assert_eq!(place_relation(targets[0], targets[1]), PlaceRelation::Unknown);
}

#[test]
fn later_field_divergence_can_prove_disjoint_after_unknown_index() {
    let program = checked(
        "struct cell { var i32 left = 0 var i32 right = 0 }\nfunc i32 main = () -> {\n    val array<cell> cells = [cell(), cell()]\n    var i32 i = 0\n    var i32 j = 1\n    cells[i].left = 1\n    cells[j].right = 2\n    return cells[0].left + cells[1].right\n}\n",
    );
    let targets = assignment_targets(&program);
    assert_eq!(place_relation(targets[0], targets[1]), PlaceRelation::Disjoint);
}
