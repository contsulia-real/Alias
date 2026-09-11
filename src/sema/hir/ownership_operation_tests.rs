use super::{
    AssignmentOperation, BindingOperation, Body, Expr, Item, OwningWrite, PreviousOwner, Stmt,
    validate_resolved_hir,
};

const CONTAINER_SOURCE: &str = r#"
struct cell { var i32 value = 1 }
struct holder { val cell item = cell() }
func i32 main = () -> {
    val cell original = cell()
    val holder object = holder(item = original)
    val array<cell> items = [original]
    items.push(cell())
    val result<cell, string> wrapped = ok(original)
    return object.item.value + items.len()
}
"#;

#[test]
fn replacement_destruction_plan_is_resolved_and_validated() {
    use super::destruction::DestroyNode;
    let source = r#"
struct bucket { var array<string> values = ['old'] }
func i32 main = () -> {
    var bucket value = bucket()
    value = bucket()
    return 0
}
"#;
    let ast = crate::parser::parse(crate::lexer::lex(source).unwrap()).unwrap();
    let mut program = crate::sema::check(ast).unwrap();
    let Item::Binding(main) = &mut program.items[1] else { panic!("main") };
    let Expr::FuncLit { body, .. } = &mut main.value else { panic!("function") };
    let Body::Block(stmts) = body.as_mut() else { panic!("body") };
    let Stmt::Assign { destroy_plan, .. } = &mut stmts[1] else { panic!("replacement") };
    let plan = destroy_plan.as_mut().expect("resolved recipe");
    assert_eq!(plan.nodes, vec![
        DestroyNode::Struct { name: "bucket".into(), fields: vec![1] },
        DestroyNode::Array { element: 2 },
        DestroyNode::String,
    ]);
    plan.nodes[2] = DestroyNode::Inline;
    let error = validate_resolved_hir(&program).expect_err("lost child destruction must fail closed");
    assert!(error.msg.contains("destruction plan"), "{}", error.msg);
}

#[test]
fn replacement_previous_owner_is_a_converged_cfg_fact() {
    let source = "func i32 main = () -> { var string value = 'old'\nif true { val string moved = move value }\nvalue = 'new'\nvalue = value\nreturn value.len() }";
    let ast = crate::parser::parse(crate::lexer::lex(source).unwrap()).unwrap();
    let mut program = crate::sema::check(ast).unwrap();
    let Item::Binding(main) = &mut program.items[0] else { panic!("main") };
    let Expr::FuncLit { body, .. } = &mut main.value else { panic!("function") };
    let Body::Block(stmts) = body.as_mut() else { panic!("body") };
    let Stmt::Assign { previous_owner, .. } = &stmts[3] else { panic!("self assignment") };
    assert_eq!(*previous_owner, Some(PreviousOwner::Live));
    let Stmt::Assign { previous_owner, .. } = &mut stmts[2] else { panic!("reinitialization") };
    assert_eq!(*previous_owner, Some(PreviousOwner::MaybeMoved));
    *previous_owner = Some(PreviousOwner::Live);
    let error = validate_resolved_hir(&program).expect_err("lost move predecessor must fail closed");
    assert!(error.msg.contains("previous-owner fact"), "{}", error.msg);
}

#[test]
fn replacement_previous_owner_includes_loop_backedges() {
    let source = r#"
func i32 main = () -> {
    var string value = 'old'
    var i32 count = 0
    while count < 2 {
        value = 'new'
        val string taken = move value
        count = count + 1
    }
    return count
}
"#;
    let ast = crate::parser::parse(crate::lexer::lex(source).unwrap()).unwrap();
    let mut program = crate::sema::check(ast).unwrap();
    let Item::Binding(main) = &mut program.items[0] else { panic!("main") };
    let Expr::FuncLit { body, .. } = &mut main.value else { panic!("function") };
    let Body::Block(stmts) = body.as_mut() else { panic!("body") };
    let Stmt::While { body, .. } = &mut stmts[2] else { panic!("loop") };
    let Stmt::Assign { previous_owner, .. } = &mut body[0] else { panic!("replacement") };
    // The entry edge owns 'old'; the backedge has transferred 'new'.
    assert_eq!(*previous_owner, Some(PreviousOwner::MaybeMoved));
    *previous_owner = None;
    let error = validate_resolved_hir(&program).expect_err("missing loop fact must fail closed");
    assert!(error.msg.contains("previous-owner fact"), "{}", error.msg);
}

fn container_initializers(program: &mut super::CheckedProgram) -> Vec<&mut Expr> {
    let mut values = Vec::new();
    for item in &mut program.items {
        match item {
            Item::StructDef(def) => {
                values.extend(
                    def.fields
                        .iter_mut()
                        .filter_map(|field| field.default.as_mut()),
                );
            }
            Item::Binding(binding) => {
                let Expr::FuncLit { body, .. } = &mut binding.value else {
                    continue;
                };
                let Body::Block(stmts) = body.as_mut() else {
                    continue;
                };
                for stmt in stmts {
                    let value = match stmt {
                        Stmt::Binding(binding) => &mut binding.value,
                        Stmt::Expr { expr } => expr,
                        _ => continue,
                    };
                    match value {
                        Expr::Call { args, .. } | Expr::MethodCall { args, .. } => {
                            values.extend(args.iter_mut().map(|arg| &mut arg.value));
                        }
                        Expr::ArrayLit { elems, .. } => values.extend(elems),
                        _ => {}
                    }
                }
            }
        }
    }
    values
}

#[test]
fn container_writes_are_frozen_and_fail_closed_at_each_destination() {
    let mut program = checked(CONTAINER_SOURCE);
    let values = container_initializers(&mut program);
    assert_eq!(
        values
            .iter()
            .map(|value| value.info().container_write)
            .collect::<Vec<_>>(),
        [
            Some(OwningWrite::InlineCopy),
            Some(OwningWrite::OwnershipTransfer),
            Some(OwningWrite::OwnershipTransfer),
            Some(OwningWrite::OwnershipTransfer),
            Some(OwningWrite::OwnershipTransfer),
            Some(OwningWrite::OwnershipTransfer),
        ]
    );
    for index in 0..values.len() {
        for replacement in [
            None,
            Some(if index == 0 {
                OwningWrite::OwnershipTransfer
            } else {
                OwningWrite::InlineCopy
            }),
        ] {
            let mut program = checked(CONTAINER_SOURCE);
            container_initializers(&mut program)[index]
                .info_mut()
                .container_write = replacement;
            let error =
                validate_resolved_hir(&program).expect_err("container operation must be validated");
            assert!(error.msg.contains("container write"), "{}", error.msg);
        }
    }
}

#[test]
fn container_write_cannot_be_attached_to_an_unrelated_expression() {
    let mut program = checked(CONTAINER_SOURCE);
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("block main")
    };
    let Stmt::Binding(binding) = &mut stmts[0] else {
        panic!("first binding")
    };
    binding.value.info_mut().container_write = Some(OwningWrite::OwnershipTransfer);
    let error = validate_resolved_hir(&program).expect_err("wrong destination must fail closed");
    assert!(
        error.msg.contains("非 container destination"),
        "{}",
        error.msg
    );
}

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

#[test]
fn binding_destruction_plan_is_frozen_and_fail_closed() {
    use super::destruction::DestroyNode;

    let mut program = checked(
        "func i32 main = () -> {\nval string value = 'live'\nreturn value.len()\n}\n",
    );
    let Body::Block(stmts) = main_body(&mut program) else {
        panic!("fixture main must use block body")
    };
    let Stmt::Binding(binding) = &mut stmts[0] else {
        panic!("fixture starts with binding")
    };
    let plan = binding.destroy_plan.as_mut().expect("resolved destruction plan");
    assert_eq!(plan.nodes, [DestroyNode::String]);
    plan.nodes[0] = DestroyNode::Inline;

    let error =
        validate_resolved_hir(&program).expect_err("binding destruction drift must fail closed");
    assert!(
        error.msg.contains("Binding destruction plan"),
        "{}",
        error.msg
    );
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
