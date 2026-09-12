use super::compile_to_object;
use crate::sema::hir::{
    BindingOperation, Body, DestroyNode, DestroyPlan, Expr, ExprCategory, ExprInfo, Item,
    OwnershipCapability, OwningWrite, Stmt, StorageRelation, ValueCategory,
};
use crate::sema::types::{IntW, Ty};

fn run_raw_allocation_fixture(count: u64) {
    let source =
        format!("func i32 main = () -> {{\n    val i64 allocation = {count}\n    return 0\n}}\n");
    let tokens = crate::lexer::lex(&source).unwrap();
    let ast = crate::parser::parse(tokens).unwrap();
    let mut program = crate::sema::check(ast).unwrap();
    let binding = program
        .items
        .iter_mut()
        .find_map(|item| match item {
            Item::Binding(main) if main.name == "main" => Some(main),
            _ => None,
        })
        .and_then(|main| match &mut main.value {
            Expr::FuncLit { body, .. } => match body.as_mut() {
                Body::Block(stmts) => stmts.iter_mut().find_map(|stmt| match stmt {
                    Stmt::Binding(binding) if binding.name == "allocation" => Some(binding),
                    _ => None,
                }),
                Body::Single(_) => None,
            },
            _ => None,
        })
        .expect("fixture allocation binding");

    let span = binding.value.span();
    let count = std::mem::replace(
        &mut binding.value,
        Expr::Int(
            0,
            span,
            ExprInfo {
                ty: Ty::Int(IntW::W64),
                category: Some(ExprCategory::Value(ValueCategory::InlineValue)),
                ownership_capability: Some(OwnershipCapability::None),
                return_pass: None,
                projection_read: None,
                container_write: None,
            },
        ),
    );
    let pointer_ty = Ty::Ptr {
        pointee: Box::new(Ty::Int(IntW::W32)),
        nullable: true,
    };
    binding.ty = pointer_ty.clone();
    binding.relation = Some(StorageRelation::Owning);
    binding.operation = Some(BindingOperation::Initialize(OwningWrite::OwnershipTransfer));
    binding.destroy_plan = Some(Box::new(DestroyPlan {
        nodes: vec![DestroyNode::RawAllocationRoot],
    }));
    binding.value = Expr::RawAllocate {
        element_ty: Ty::Int(IntW::W32),
        count: Box::new(count),
        span,
        info: ExprInfo {
            ty: pointer_ty,
            category: Some(ExprCategory::Value(ValueCategory::OwnedTemporary)),
            ownership_capability: Some(OwnershipCapability::Available),
            return_pass: None,
            projection_read: None,
            container_write: None,
        },
    };

    // Start from an already validated program and substitute the still-gated structured node. This
    // exercises the one native object/link/process path without weakening the source activation
    // gate before its ownership proof is complete.
    let object = compile_to_object(program).unwrap();
    let executable = crate::TempExecutable::create().unwrap();
    crate::linker::link_exe(&object, &executable.path).unwrap();
    let status = std::process::Command::new(&executable.path)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
}

#[test]
fn raw_allocation_descriptor_round_trips_through_native_scope_cleanup() {
    for count in [0, 4, i64::MAX as u64] {
        run_raw_allocation_fixture(count);
    }
}
