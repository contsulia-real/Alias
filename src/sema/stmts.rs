//! sema::stmts — 语句、函数体与控制流检查。

use super::exprs::ExprCheckError;
use super::types::{check_return_type_slot, check_value_type_slot, types_match, Ty};
use super::{decl_mismatch, Checker, Env, Scope, VarInfo};
use crate::ast::{ArmBody, BindKind, Binding, Body, Expr, Param, Stmt};
use crate::{AliasError, AliasResult, Span};

impl Checker {
    pub(super) fn bind(&mut self, b: &Binding, env: &Env) -> AliasResult<()> {
        if b.receiver.is_some() {
            return Err(AliasError {
                msg: "方法定义只能出现在顶层".into(),
                span: b.span,
            });
        }
        if self.structs.contains_key(&b.name) {
            return Err(AliasError {
                msg: format!("'{}' 已定义为结构体, 不能再定义为绑定", b.name),
                span: b.span,
            });
        }
        let declared = if b.kind == BindKind::Func {
            check_return_type_slot(&b.ty, b.span, &self.structs)?
        } else {
            check_value_type_slot(&b.ty, b.span, &self.structs)?
        };
        if b.kind == BindKind::Func {
            let Expr::FuncLit { params, body, span } = &b.value else {
                return Err(AliasError {
                    msg: "func 绑定必须由函数字面量初始化".into(),
                    span: b.value.span(),
                });
            };
            let init_ty = self.funclit(params, body, env, Some(&declared), *span)?;
            if let Ty::Func { ret, .. } = &init_ty {
                if !types_match(&declared, ret) {
                    return Err(decl_mismatch(b, &declared, ret));
                }
            }
            if b.name == "main" && !init_ty.is_unknown() {
                self.main = Some((init_ty.clone(), b.span));
            }
            Scope::insert(
                env,
                b.name.clone(),
                VarInfo {
                    ty: init_ty,
                    mutable: false,
                },
            );
        } else {
            let init_ty =
                self.expr_expected(&b.value, env, &declared)
                    .map_err(|error| match error {
                        literal @ ExprCheckError::LiteralOutOfRange { .. } => literal.into_alias(),
                        other => {
                            let error = other.into_alias();
                            AliasError {
                                msg: format!(
                                    "绑定 '{}' 声明类型为 {}: {}",
                                    b.name,
                                    declared.name(),
                                    error.msg
                                ),
                                span: error.span,
                            }
                        }
                    })?;
            Scope::insert(
                env,
                b.name.clone(),
                VarInfo {
                    ty: init_ty,
                    mutable: b.kind == BindKind::Var,
                },
            );
        }
        Ok(())
    }

    pub(super) fn funclit(
        &mut self,
        params: &[Param],
        body: &Body,
        env: &Env,
        expected: Option<&Ty>,
        fspan: Span,
    ) -> AliasResult<Ty> {
        let local = Scope::child(env);
        let mut param_tys = Vec::with_capacity(params.len());
        for p in params {
            let pt = check_value_type_slot(&p.ty, p.span, &self.structs)?;
            param_tys.push(pt.clone());
            Scope::insert(
                &local,
                p.name.clone(),
                VarInfo {
                    ty: pt,
                    mutable: false,
                },
            );
        }

        let ret_ty = expected.cloned().unwrap_or(Ty::Unknown);
        Scope::insert(
            &local,
            "this".into(),
            VarInfo {
                ty: Ty::Func {
                    params: param_tys.clone(),
                    ret: Box::new(ret_ty.clone()),
                },
                mutable: false,
            },
        );
        self.fn_ret.push(ret_ty.clone());
        let outer_loop_depth = self.loop_depth;
        self.loop_depth = 0;
        let check_result: AliasResult<()> = (|| {
            match body {
                Body::Single(stmt) => {
                    self.stmt(stmt, &local)?;
                    if expected.is_some_and(|t| *t != Ty::Unit) && !stmt_guarantees_return(stmt) {
                        return Err(AliasError {
                            msg: format!(
                                "返回类型为 {} 的函数所有可达路径都必须显式 return",
                                ret_ty.name()
                            ),
                            span: fspan,
                        });
                    }
                }
                Body::Block(stmts) => {
                    for s in stmts {
                        self.stmt(s, &local)?;
                    }
                    if expected.is_some_and(|t| *t != Ty::Unit)
                        && !block_terminates_with_return(stmts)
                    {
                        return Err(AliasError {
                            msg: format!(
                                "返回类型为 {} 的函数所有可达路径都必须显式 return",
                                ret_ty.name()
                            ),
                            span: fspan,
                        });
                    }
                }
            }
            Ok(())
        })();
        self.loop_depth = outer_loop_depth;
        self.fn_ret.pop();
        check_result?;

        let inferred_ret = expected.cloned().unwrap_or_else(|| {
            if body_guarantees_return(body) {
                Ty::Unknown
            } else {
                Ty::Unit
            }
        });
        Ok(Ty::Func {
            params: param_tys,
            ret: Box::new(inferred_ret),
        })
    }

    pub(super) fn stmt(&mut self, s: &Stmt, env: &Env) -> AliasResult<Option<Ty>> {
        match s {
            Stmt::Binding(b) => {
                self.bind(b, env)?;
                Ok(None)
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => {
                // sema 先解析目标的静态类型，再用它检查 RHS；这只决定类型上下文，
                // 不改变 codegen 中 RHS 的运行时求值顺序。
                let Some(info) = Scope::get(env, target) else {
                    return Err(AliasError {
                        msg: format!("赋值目标 '{target}' 未定义"),
                        span: *span,
                    });
                };
                if !info.mutable {
                    return Err(AliasError {
                        msg: format!("'{target}' 是 val 绑定, 不可重新赋值"),
                        span: *span,
                    });
                }
                self.expr_expected(value, env, &info.ty).map_err(|error| {
                    let error = error.into_alias();
                    AliasError {
                        msg: format!("赋值目标 '{target}' 需要 {}: {}", info.ty.name(), error.msg),
                        span: error.span,
                    }
                })?;
                Ok(None)
            }
            Stmt::FieldAssign {
                recv,
                field,
                value,
                span,
            } => {
                // 字段类型是 RHS 的目标上下文，必须在检查 from/try_from 前解析。
                let rt = self.expr(recv, env)?;
                if rt.is_unknown() {
                    self.expr(value, env)?;
                    return Ok(None);
                }
                let Ty::Struct(s) = rt else {
                    return Err(AliasError {
                        msg: format!("{} 没有字段 '{}'", rt.name(), field),
                        span: *span,
                    });
                };
                let info = &self.structs[&s];
                let Some(f) = info.fields.iter().find(|fi| fi.name == *field).cloned() else {
                    return Err(AliasError {
                        msg: format!("结构体 {s} 没有字段 '{field}'"),
                        span: *span,
                    });
                };
                if !f.mutable {
                    return Err(AliasError {
                        msg: format!("'{field}' 是 val 字段, 不可赋值"),
                        span: *span,
                    });
                }
                self.expr_expected(value, env, &f.ty).map_err(|error| {
                    let error = error.into_alias();
                    AliasError {
                        msg: format!("字段 '{field}' 需要 {}: {}", f.ty.name(), error.msg),
                        span: error.span,
                    }
                })?;
                Ok(None)
            }
            Stmt::ExprStmt { expr, .. } => {
                if let Expr::Call { callee, args, span } = expr {
                    if let Expr::Ident(name, _) = callee.as_ref() {
                        if name == "increase" || name == "decrease" {
                            self.incdec(name, args, *span, env)?;
                            return Ok(None);
                        }
                    }
                }
                self.expr(expr, env)?;
                Ok(None)
            }
            Stmt::Return { value, span } => {
                let Some(ret) = self.fn_ret.last().cloned() else {
                    return Err(AliasError {
                        msg: "顶层不允许 return".into(),
                        span: *span,
                    });
                };
                match value {
                    Some(_) if ret == Ty::Unit => {
                        return Err(AliasError {
                            msg: "unit 函数的 return 不能携带值".into(),
                            span: *span,
                        });
                    }
                    Some(e) => {
                        self.expr_expected(e, env, &ret).map_err(|error| {
                            let error = error.into_alias();
                            AliasError {
                                msg: format!("return 需要 {}: {}", ret.name(), error.msg),
                                span: error.span,
                            }
                        })?;
                    }
                    None if ret != Ty::Unit && !ret.is_unknown() => {
                        return Err(AliasError {
                            msg: format!("return 需要 {}, 不能省略返回值", ret.name()),
                            span: *span,
                        });
                    }
                    None => {}
                }
                Ok(Some(ret))
            }
            Stmt::If {
                branches,
                else_body,
                ..
            } => {
                for (cond, body) in branches {
                    let ct = self.expr(cond, env)?;
                    if !ct.is_unknown() && ct != Ty::Bool {
                        return Err(AliasError {
                            msg: format!("if 条件需要 bool, 实际 {}", ct.name()),
                            span: cond.span(),
                        });
                    }
                    let child = Scope::child(env);
                    for s in body {
                        self.stmt(s, &child)?;
                    }
                }
                if let Some(body) = else_body {
                    let child = Scope::child(env);
                    for s in body {
                        self.stmt(s, &child)?;
                    }
                }
                Ok(None)
            }
            Stmt::While { cond, body, span } => {
                let ct = self.expr(cond, env)?;
                if !ct.is_unknown() && ct != Ty::Bool {
                    return Err(AliasError {
                        msg: format!("while 条件需要 bool, 实际 {}", ct.name()),
                        span: *span,
                    });
                }
                self.check_loop_body(body, env)?;
                Ok(None)
            }
            Stmt::For {
                ty,
                name,
                iterable,
                body,
                span,
            } => {
                let declared = check_value_type_slot(ty, *span, &self.structs)?;
                let source = self.expr(iterable, env)?;
                let elem = match source {
                    Ty::Array(elem) | Ty::Iterator(elem) => *elem,
                    Ty::Unknown => Ty::Unknown,
                    other => {
                        return Err(AliasError {
                            msg: format!("for 需要 array<T> 或 iterator<T>, 实际 {}", other.name()),
                            span: iterable.span(),
                        })
                    }
                };
                if !types_match(&declared, &elem) {
                    return Err(AliasError {
                        msg: format!(
                            "for 循环变量 '{}' 声明 {}, 迭代元素为 {}",
                            name,
                            declared.name(),
                            elem.name()
                        ),
                        span: *span,
                    });
                }
                let child = Scope::child(env);
                Scope::insert(
                    &child,
                    name.clone(),
                    VarInfo {
                        ty: declared,
                        mutable: false,
                    },
                );
                self.loop_depth += 1;
                let result = (|| {
                    for s in body {
                        self.stmt(s, &child)?;
                    }
                    Ok(())
                })();
                self.loop_depth -= 1;
                result?;
                Ok(None)
            }
            Stmt::Break { span } => {
                if self.loop_depth == 0 {
                    return Err(AliasError {
                        msg: "break 只能出现在 for/while 内".into(),
                        span: *span,
                    });
                }
                Ok(None)
            }
            Stmt::Continue { span } => {
                if self.loop_depth == 0 {
                    return Err(AliasError {
                        msg: "continue 只能出现在 for/while 内".into(),
                        span: *span,
                    });
                }
                Ok(None)
            }
        }
    }

    fn check_loop_body(&mut self, body: &[Stmt], env: &Env) -> AliasResult<()> {
        let child = Scope::child(env);
        self.loop_depth += 1;
        let result = (|| {
            for s in body {
                self.stmt(s, &child)?;
            }
            Ok(())
        })();
        self.loop_depth -= 1;
        result
    }
}

fn body_guarantees_return(body: &Body) -> bool {
    match body {
        Body::Single(s) => stmt_guarantees_return(s),
        Body::Block(stmts) => block_terminates_with_return(stmts),
    }
}

/// 纯控制流形状判定：只证明“所有可达路径均显式 return”。
/// 循环永不用于证明必返回，即使条件字面量为 true。
pub(super) fn block_terminates_with_return(stmts: &[Stmt]) -> bool {
    for stmt in stmts {
        if stmt_guarantees_return(stmt) {
            return true;
        }
    }
    false
}

fn stmt_guarantees_return(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return { .. } => true,
        Stmt::If {
            branches,
            else_body: Some(else_body),
            ..
        } => {
            branches
                .iter()
                .all(|(_, b)| block_terminates_with_return(b))
                && block_terminates_with_return(else_body)
        }
        Stmt::ExprStmt { expr, .. } => expr_all_arms_never(expr),
        _ => false,
    }
}

fn expr_all_arms_never(e: &Expr) -> bool {
    match e {
        Expr::Match { arms, .. } => arms.iter().all(|a| arm_body_never(&a.body)),
        _ => false,
    }
}

fn arm_body_never(b: &ArmBody) -> bool {
    match b {
        ArmBody::Ret(_) => true,
        ArmBody::Value(_) => false,
        ArmBody::Block(stmts) => block_terminates_with_return(stmts),
    }
}
