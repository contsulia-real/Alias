use super::{
    ArmBody, Binding, BindingOwner, Body, CheckedProgram, Expr, Item, MethodTarget, Place, Stmt,
    StrPart,
};
use crate::sema::types::Ty;

enum TypeNode<'a> {
    Binding(&'a Binding),
    Expr(&'a Expr),
    Stmt(&'a Stmt),
}

fn visit_place<'a>(
    place: &'a Place,
    visit: &mut impl FnMut(&Ty),
    stack: &mut Vec<TypeNode<'a>>,
) {
    let mut places = vec![place];
    while let Some(place) = places.pop() {
        visit(place.ty());
        match place {
            Place::Local { .. } => {}
            Place::Field { base, .. } => places.push(base),
            Place::Index { base, index, .. } => {
                stack.push(TypeNode::Expr(index));
                places.push(base);
            }
        }
    }
}

impl CheckedProgram {
    /// 枚举 codegen 单次 Ty→VTy 投影所需的全部类型。显式栈避免重新把不可信 HIR
    /// 嵌套映射到宿主递归；children 逆序入栈以保持确定的源码前序。新增 HIR payload
    /// 若未在此登记，会让后端投影表缺项并把已验证程序变成内部 panic。
    pub(crate) fn for_each_ty(&self, visit: &mut impl FnMut(&Ty)) {
        let mut stack = Vec::new();
        for item in self.items.iter().rev() {
            match item {
                Item::Binding(binding) => stack.push(TypeNode::Binding(binding)),
                Item::StructDef(def) => {
                    for field in def.fields.iter().rev() {
                        visit(&field.ty);
                        if let Some(default) = &field.default {
                            stack.push(TypeNode::Expr(default));
                        }
                    }
                }
            }
        }

        while let Some(node) = stack.pop() {
            match node {
                TypeNode::Binding(binding) => {
                    visit(&binding.ty);
                    if let BindingOwner::Method { receiver, .. } = &binding.owner {
                        visit(receiver);
                    }
                    stack.push(TypeNode::Expr(&binding.value));
                }
                TypeNode::Stmt(stmt) => match stmt {
                    Stmt::Binding(binding) => stack.push(TypeNode::Binding(binding)),
                    Stmt::Assign { target, value } => {
                        visit_place(target, visit, &mut stack);
                        stack.push(TypeNode::Expr(value));
                    }
                    Stmt::Expr { expr } => stack.push(TypeNode::Expr(expr)),
                    Stmt::Return { value } => {
                        if let Some(value) = value {
                            stack.push(TypeNode::Expr(value));
                        }
                    }
                    Stmt::If {
                        branches,
                        else_body,
                    } => {
                        if let Some(body) = else_body {
                            for stmt in body.iter().rev() {
                                stack.push(TypeNode::Stmt(stmt));
                            }
                        }
                        for (cond, body) in branches.iter().rev() {
                            for stmt in body.iter().rev() {
                                stack.push(TypeNode::Stmt(stmt));
                            }
                            stack.push(TypeNode::Expr(cond));
                        }
                    }
                    Stmt::While { cond, body } => {
                        for stmt in body.iter().rev() {
                            stack.push(TypeNode::Stmt(stmt));
                        }
                        stack.push(TypeNode::Expr(cond));
                    }
                    Stmt::For {
                        ty, iterable, body, ..
                    } => {
                        visit(ty);
                        for stmt in body.iter().rev() {
                            stack.push(TypeNode::Stmt(stmt));
                        }
                        stack.push(TypeNode::Expr(iterable));
                    }
                    Stmt::Break | Stmt::Continue => {}
                },
                TypeNode::Expr(expr) => {
                    visit(expr.ty());
                    if let Expr::MethodCall {
                        target: MethodTarget::User { receiver, .. },
                        ..
                    } = expr
                    {
                        visit(receiver);
                    }
                    match expr {
                        Expr::Int(..)
                        | Expr::Float(..)
                        | Expr::Bool(..)
                        | Expr::Ident(..)
                        | Expr::This(..)
                        | Expr::Typeof { .. } => {}
                        Expr::Str(parts, ..) => {
                            for part in parts.iter().rev() {
                                if let StrPart::Hole(hole) = part {
                                    stack.push(TypeNode::Expr(hole));
                                }
                            }
                        }
                        Expr::Cast { expr, .. }
                        | Expr::Convert { expr, .. }
                        | Expr::Neg { expr, .. }
                        | Expr::Not { expr, .. }
                        | Expr::BitNot { expr, .. }
                        | Expr::Propagate { expr, .. } => stack.push(TypeNode::Expr(expr)),
                        Expr::Binary { lhs, rhs, .. } => {
                            stack.push(TypeNode::Expr(rhs));
                            stack.push(TypeNode::Expr(lhs));
                        }
                        Expr::Ternary {
                            cond,
                            then_expr,
                            else_expr,
                            ..
                        } => {
                            stack.push(TypeNode::Expr(else_expr));
                            stack.push(TypeNode::Expr(then_expr));
                            stack.push(TypeNode::Expr(cond));
                        }
                        Expr::Call { callee, args, .. } => {
                            for arg in args.iter().rev() {
                                stack.push(TypeNode::Expr(&arg.value));
                            }
                            stack.push(TypeNode::Expr(callee));
                        }
                        Expr::MethodCall { recv, args, .. } => {
                            for arg in args.iter().rev() {
                                stack.push(TypeNode::Expr(&arg.value));
                            }
                            stack.push(TypeNode::Expr(recv));
                        }
                        Expr::Field { recv, .. } => stack.push(TypeNode::Expr(recv)),
                        Expr::Index { recv, idx, .. } => {
                            stack.push(TypeNode::Expr(idx));
                            stack.push(TypeNode::Expr(recv));
                        }
                        Expr::ArrayLit { elems, .. } => {
                            for elem in elems.iter().rev() {
                                stack.push(TypeNode::Expr(elem));
                            }
                        }
                        Expr::FuncLit { params, body, .. } => {
                            for param in params {
                                visit(&param.ty);
                            }
                            match body.as_ref() {
                                Body::Block(stmts) => {
                                    for stmt in stmts.iter().rev() {
                                        stack.push(TypeNode::Stmt(stmt));
                                    }
                                }
                                Body::Single(stmt) => stack.push(TypeNode::Stmt(stmt)),
                            }
                        }
                        Expr::Match { subject, arms, .. } => {
                            for arm in arms.iter().rev() {
                                match &arm.body {
                                    ArmBody::Block(stmts) => {
                                        for stmt in stmts.iter().rev() {
                                            stack.push(TypeNode::Stmt(stmt));
                                        }
                                    }
                                    ArmBody::Value(value) | ArmBody::Ret(value) => {
                                        stack.push(TypeNode::Expr(value));
                                    }
                                }
                            }
                            stack.push(TypeNode::Expr(subject));
                        }
                        Expr::ReadPlace { source, .. } | Expr::Move { source, .. } => {
                            visit_place(source, visit, &mut stack)
                        }
                    }
                }
            }
        }
    }
}
