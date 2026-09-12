use super::{
    validate_resolved_hir, Body, Expr, ExprCategory, Item, OwnershipCapability, Stmt, ValueCategory,
};
use crate::sema::types::{IntW, Ty};

fn checked(source: &str) -> super::CheckedProgram {
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    crate::sema::check(program).unwrap()
}

fn local_value(program: &mut super::CheckedProgram) -> &mut Expr {
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
            Stmt::Binding(binding) if binding.name == "value" => Some(&mut binding.value),
            _ => None,
        })
        .expect("value binding")
}

fn replace_with_raw_node(program: &mut super::CheckedProgram, allocate: bool) {
    let target = local_value(program);
    let span = target.span();
    let mut info = target.info().clone();
    if allocate {
        info.category = Some(ExprCategory::Value(ValueCategory::OwnedTemporary));
        info.ownership_capability = Some(OwnershipCapability::Available);
    }
    let operand = std::mem::replace(target, Expr::Int(1, span, info.clone()));
    *target = if allocate {
        Expr::RawAllocate {
            element_ty: Ty::Int(IntW::W32),
            count: Box::new(operand),
            span,
            info,
        }
    } else {
        Expr::FreeRawAllocation {
            pointer: Box::new(operand),
            span,
            info,
        }
    };
}

#[test]
fn abstract_raw_allocation_nodes_remain_fail_closed_until_source_ownership_lands() {
    for allocate in [true, false] {
        let mut program =
            checked("func i32 main = () -> {\n    val i32 value = 1\n    return value\n}\n");
        replace_with_raw_node(&mut program, allocate);
        let error = validate_resolved_hir(&program)
            .expect_err("raw allocation nodes must not pass the final typed gate yet");
        assert!(
            error.msg.contains("raw allocation HIR"),
            "实际: {}",
            error.msg
        );
    }
}
