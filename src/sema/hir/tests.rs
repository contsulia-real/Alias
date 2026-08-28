use super::{
    ArmBody, Body, CallTarget, Expr, Item, MethodId, MethodTarget, Stmt, StrPart,
};
use crate::codegen::abi::{project_ty, projected_ty, VTy};

#[test]
fn checked_hir_has_exact_types_and_stable_targets() {
    let source = r#"
struct point { val i32 x = 7 }
func i32 point.bump = (i32 amount) -> return self.x + amount
func i32 helper = (i32 value) -> return value
func i32 main = () -> {
    val i32 outer = 4
    func i32 capture = () -> return outer
    val point p = point()
    val i32 a = p.x
    val i32 b = p bump 1
    val i32 c = helper outer
    return a + b + c + capture
}
"#;
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    let checked = crate::sema::check(program).unwrap();
    let projections = project_ty(&checked);
    checked.for_each_ty(&mut |ty| {
        assert!(!ty.contains_unknown(), "仍含未确定类型 {}", ty.name());
        assert!(!matches!(projected_ty(&projections, ty), VTy::Unknown));
    });

    let mut saw_field = false;
    let mut saw_method = false;
    let mut saw_capture = false;
    let mut stack = Vec::new();
    for item in checked.items.iter().rev() {
        if let Item::Binding(binding) = item {
            stack.push(TestNode::Expr(&binding.value));
        }
    }
    while let Some(node) = stack.pop() {
        match node {
            TestNode::Expr(expr) => match expr {
                Expr::Ident(_, id, ..) => assert!(id.is_some(), "可求值 ident 必须有 BindingId"),
                Expr::Field {
                    recv, field_index, ..
                } => {
                    saw_field = true;
                    assert_eq!(*field_index, 0);
                    stack.push(TestNode::Expr(recv));
                }
                Expr::MethodCall {
                    recv, args, target, ..
                } => {
                    if matches!(target, MethodTarget::User { .. }) {
                        saw_method = true;
                    }
                    for arg in args.iter().rev() {
                        stack.push(TestNode::Expr(&arg.value));
                    }
                    stack.push(TestNode::Expr(recv));
                }
                Expr::FuncLit { captures, body, .. } => {
                    saw_capture |= !captures.is_empty();
                    push_body(&mut stack, body);
                }
                _ => push_expr(&mut stack, expr),
            },
            TestNode::Stmt(stmt) => push_stmt(&mut stack, stmt),
        }
    }
    assert!(saw_field);
    assert!(saw_method);
    assert!(saw_capture);
}

#[test]
fn resolved_hir_rejects_unknown_user_method_id() {
    let source = r#"
struct point { val i32 x = 7 }
func i32 point.bump = (i32 amount) -> return self.x + amount
func i32 main = () -> {
    val point p = point()
    return p bump 1
}
"#;
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    let mut checked = crate::sema::check(program).unwrap();
    let main = checked
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
    let Body::Block(stmts) = body.as_mut() else {
        panic!("fixture main must use block body")
    };
    let target = stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Return {
                value: Some(Expr::MethodCall { target, .. }),
            } => Some(target),
            _ => None,
        })
        .expect("fixture must contain user method call");
    let MethodTarget::User { id, .. } = target else {
        panic!("fixture call must resolve to user method")
    };
    *id = MethodId(u32::MAX);

    let error = super::validate::validate_resolved_hir(&checked)
        .expect_err("unknown MethodId must fail the final HIR gate");
    assert!(error.msg.contains("未知 MethodId"), "实际: {}", error.msg);
}

#[test]
fn resolved_hir_rejects_corrupt_constructor_field_index() {
    let source = r#"
struct point { val i32 x = 7 }
func i32 main = () -> {
    val point p = point(x = 9)
    return p.x
}
"#;
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    let mut checked = crate::sema::check(program).unwrap();
    let main = checked
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
    let Body::Block(stmts) = body.as_mut() else {
        panic!("fixture main must use block body")
    };
    let indices = stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Binding(binding) if binding.name == "p" => match &mut binding.value {
                Expr::Call {
                    target: CallTarget::StructConstructor {
                        arg_field_indices, ..
                    },
                    ..
                } => Some(arg_field_indices),
                _ => None,
            },
            _ => None,
        })
        .expect("fixture must contain struct constructor");
    indices[0] = usize::MAX;

    let error = super::validate::validate_resolved_hir(&checked)
        .expect_err("corrupt constructor index must fail the final HIR gate");
    assert!(
        error.msg.contains("字段索引越界或重复"),
        "实际: {}",
        error.msg
    );
}

#[test]
fn resolved_hir_rejects_corrupt_field_index() {
    let source = r#"
struct point { val i32 x = 7 }
func i32 main = () -> {
    val point p = point()
    return p.x
}
"#;
    let tokens = crate::lexer::lex(source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    let mut checked = crate::sema::check(program).unwrap();
    let main = checked
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
    let Body::Block(stmts) = body.as_mut() else {
        panic!("fixture main must use block body")
    };
    let field_index = stmts
        .iter_mut()
        .find_map(|stmt| match stmt {
            Stmt::Return {
                value: Some(Expr::Field { field_index, .. }),
            } => Some(field_index),
            _ => None,
        })
        .expect("fixture must contain field access");
    *field_index = usize::MAX;

    let error = super::validate::validate_resolved_hir(&checked)
        .expect_err("corrupt field index must fail the final HIR gate");
    assert!(error.msg.contains("字段索引越界"), "实际: {}", error.msg);
}

#[test]
fn deep_capture_and_type_walk_do_not_depend_on_host_recursion() {
    let depth = 32;
    let source = nested_closure_source(depth);
    let tokens = crate::lexer::lex(&source).unwrap();
    let program = crate::parser::parse(tokens).unwrap();
    let checked = crate::sema::check(program).unwrap();

    let mut visited = 0usize;
    checked.for_each_ty(&mut |ty| {
        assert!(!ty.contains_unknown());
        visited += 1;
    });
    assert!(visited > depth);

    let projections = project_ty(&checked);
    checked.for_each_ty(&mut |ty| {
        assert!(!matches!(projected_ty(&projections, ty), VTy::Unknown));
    });
}

fn nested_closure_source(depth: usize) -> String {
    assert!(depth > 0);
    let deepest = depth - 1;
    let sum = std::iter::once("root".to_string())
        .chain((0..depth).map(|i| format!("x{i}")))
        .collect::<Vec<_>>()
        .join(" + ");
    let mut body = format!("val i32 x{deepest} = {}\nreturn {sum}\n", deepest + 1);
    for level in (0..deepest).rev() {
        body = format!(
            "val i32 x{level} = {}\nfunc i32 f{} = () -> {{\n{body}}}\nreturn f{}()\n",
            level + 1,
            level + 1,
            level + 1,
        );
    }
    format!(
        "func i32 main = () -> {{\nval i32 root = 7\nfunc i32 f0 = () -> {{\n{body}}}\nreturn f0()\n}}\n"
    )
}

enum TestNode<'a> {
    Expr(&'a Expr),
    Stmt(&'a Stmt),
}

fn push_body<'a>(stack: &mut Vec<TestNode<'a>>, body: &'a Body) {
    match body {
        Body::Block(stmts) => {
            for stmt in stmts.iter().rev() {
                stack.push(TestNode::Stmt(stmt));
            }
        }
        Body::Single(stmt) => stack.push(TestNode::Stmt(stmt)),
    }
}

fn push_stmt<'a>(stack: &mut Vec<TestNode<'a>>, stmt: &'a Stmt) {
    match stmt {
        Stmt::Binding(binding) => stack.push(TestNode::Expr(&binding.value)),
        Stmt::Assign { value, .. } => stack.push(TestNode::Expr(value)),
        Stmt::FieldAssign { recv, value, .. } => {
            stack.push(TestNode::Expr(value));
            stack.push(TestNode::Expr(recv));
        }
        Stmt::Expr { expr, .. } => stack.push(TestNode::Expr(expr)),
        Stmt::Return { value, .. } => {
            if let Some(value) = value {
                stack.push(TestNode::Expr(value));
            }
        }
        Stmt::If {
            branches,
            else_body,
            ..
        } => {
            if let Some(body) = else_body {
                for stmt in body.iter().rev() {
                    stack.push(TestNode::Stmt(stmt));
                }
            }
            for (cond, body) in branches.iter().rev() {
                for stmt in body.iter().rev() {
                    stack.push(TestNode::Stmt(stmt));
                }
                stack.push(TestNode::Expr(cond));
            }
        }
        Stmt::While { cond, body, .. } => {
            for stmt in body.iter().rev() {
                stack.push(TestNode::Stmt(stmt));
            }
            stack.push(TestNode::Expr(cond));
        }
        Stmt::For { iterable, body, .. } => {
            for stmt in body.iter().rev() {
                stack.push(TestNode::Stmt(stmt));
            }
            stack.push(TestNode::Expr(iterable));
        }
        Stmt::Break | Stmt::Continue => {}
    }
}

fn push_expr<'a>(stack: &mut Vec<TestNode<'a>>, expr: &'a Expr) {
    match expr {
        Expr::Str(parts, ..) => {
            for part in parts.iter().rev() {
                if let StrPart::Hole(hole) = part {
                    stack.push(TestNode::Expr(hole));
                }
            }
        }
        Expr::Cast { expr, .. }
        | Expr::Convert { expr, .. }
        | Expr::Neg { expr, .. }
        | Expr::Not { expr, .. }
        | Expr::BitNot { expr, .. }
        | Expr::Propagate { expr, .. } => stack.push(TestNode::Expr(expr)),
        Expr::Binary { lhs, rhs, .. } => {
            stack.push(TestNode::Expr(rhs));
            stack.push(TestNode::Expr(lhs));
        }
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            stack.push(TestNode::Expr(else_expr));
            stack.push(TestNode::Expr(then_expr));
            stack.push(TestNode::Expr(cond));
        }
        Expr::Call {
            callee,
            args,
            target,
            ..
        } => {
            for arg in args.iter().rev() {
                stack.push(TestNode::Expr(&arg.value));
            }
            if *target == CallTarget::FunctionValue {
                stack.push(TestNode::Expr(callee));
            }
        }
        Expr::MethodCall { recv, args, .. } => {
            for arg in args.iter().rev() {
                stack.push(TestNode::Expr(&arg.value));
            }
            stack.push(TestNode::Expr(recv));
        }
        Expr::Field { recv, .. } => stack.push(TestNode::Expr(recv)),
        Expr::Index { recv, idx, .. } => {
            stack.push(TestNode::Expr(idx));
            stack.push(TestNode::Expr(recv));
        }
        Expr::ArrayLit { elems, .. } => {
            for elem in elems.iter().rev() {
                stack.push(TestNode::Expr(elem));
            }
        }
        Expr::FuncLit { body, .. } => push_body(stack, body),
        Expr::Match { subject, arms, .. } => {
            for arm in arms.iter().rev() {
                match &arm.body {
                    ArmBody::Block(stmts) => {
                        for stmt in stmts.iter().rev() {
                            stack.push(TestNode::Stmt(stmt));
                        }
                    }
                    ArmBody::Value(value) | ArmBody::Ret(value) => {
                        stack.push(TestNode::Expr(value))
                    }
                }
            }
            stack.push(TestNode::Expr(subject));
        }
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Ident(..)
        | Expr::This(..)
        | Expr::Typeof { .. } => {}
    }
}
