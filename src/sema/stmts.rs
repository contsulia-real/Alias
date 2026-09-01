//! sema::stmts — 语句、函数体与控制流检查。

use super::exprs::{require_value, ExprCheckError};
use super::hir::{BindingId, BuiltinCall};
use super::types::{check_return_type_slot, check_value_type_slot, types_match, Ty};
use super::{
    decl_mismatch, ensure_user_lexical_name, resolved_builtin_call, Checker, Env, LowerCallTarget,
    Scope, VarInfo,
};
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
        ensure_user_lexical_name(&b.name, b.span)?;
        // struct 名只在顶层与普通绑定共享命名空间；子词法作用域仍允许 shadow 构造器。
        // 把该检查下放到所有 Scope 会错误拒绝当前规范允许的局部 shadow。
        if env.parent.is_none() && self.structs.contains_key(&b.name) {
            return Err(AliasError {
                msg: format!("'{}' 已定义为结构体, 不能再定义为绑定", b.name),
                span: b.span,
            });
        }
        let binding_id = self.binding_id_for(b)?;
        if Scope::get_here(env, &b.name).is_some_and(|existing| existing.id != binding_id) {
            return Err(AliasError {
                msg: format!("同一词法作用域不能重复声明绑定 '{}'", b.name),
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
            let (init_ty, param_ids) = self.funclit(params, body, env, Some(&declared), *span)?;
            if let Ty::Func {
                params: param_types,
                ..
            } = &init_ty
            {
                self.record_params(params, param_types, &param_ids)?;
            }
            self.record_expr_type(&b.value, init_ty.clone());
            self.binding_types
                .insert(b as *const Binding as usize, init_ty.clone());
            if let Ty::Func { ret, .. } = &init_ty {
                if !types_match(&declared, ret) {
                    return Err(decl_mismatch(b, &declared, ret));
                }
            }
            if b.name == "main" && !init_ty.is_unknown() {
                self.main = Some((binding_id, init_ty.clone(), b.span));
            }
            Scope::insert(
                env,
                b.name.clone(),
                VarInfo {
                    id: binding_id,
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
            self.record_owning_slot_read(&b.value, env, &declared)?;
            if let Some(borrow) = self.borrow_places.get(&Self::expr_key(&b.value)) {
                if env.parent.is_none() {
                    return Err(AliasError {
                        msg: "borrowed binding 当前只允许位于受函数体控制的局部作用域".into(),
                        span: b.span,
                    });
                }
                self.borrowed_bindings
                    .insert(binding_id, borrow.source_writable);
            }
            self.binding_types
                .insert(b as *const Binding as usize, declared.clone());
            Scope::insert(
                env,
                b.name.clone(),
                VarInfo {
                    id: binding_id,
                    ty: init_ty,
                    mutable: b.kind == BindKind::Var,
                },
            );
        }
        Ok(())
    }

    /// 检查函数字面量并在第一次建立参数作用域时分配稳定 BindingId。
    pub(super) fn funclit(
        &mut self,
        params: &[Param],
        body: &Body,
        env: &Env,
        expected: Option<&Ty>,
        fspan: Span,
    ) -> AliasResult<(Ty, Vec<BindingId>)> {
        let local = Scope::child(env);
        let mut param_tys = Vec::with_capacity(params.len());
        let mut param_ids = Vec::with_capacity(params.len());
        for p in params {
            // synthetic `self` 是关键字拥有的方法参数；只有它绕过用户名字门禁，避免
            // 同时放开源码参数对预定义名字的重声明。
            if p.name != "self" {
                ensure_user_lexical_name(&p.name, p.span)?;
            }
            if Scope::get_here(&local, &p.name).is_some() {
                return Err(AliasError {
                    msg: format!("同一参数列表不能重复参数名 '{}'", p.name),
                    span: p.span,
                });
            }
            let pt = check_value_type_slot(&p.ty, p.span, &self.structs)?;
            let id = self.fresh_binding_id()?;
            param_tys.push(pt.clone());
            param_ids.push(id);
            Scope::insert(
                &local,
                p.name.clone(),
                VarInfo {
                    id,
                    ty: pt,
                    mutable: false,
                },
            );
        }

        let ret_ty = expected.cloned().unwrap_or(Ty::Unknown);
        // `this` 是特殊当前函数引用，不进入普通 HIR BindingId 存储模型。
        let this_scope_id = self.fresh_binding_id()?;
        if Scope::get_here(&local, "this").is_some() {
            return Err(AliasError {
                msg: "参数名不能使用函数内保留名 'this'".into(),
                span: fspan,
            });
        }
        Scope::insert(
            &local,
            "this".into(),
            VarInfo {
                id: this_scope_id,
                ty: Ty::Func {
                    params: param_tys.clone(),
                    param_effects: None,
                    return_effect: None,
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
        let Some(checked_ret) = self.fn_ret.pop() else {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: funclit 返回类型栈为空".into(),
                span: fspan,
            });
        };
        check_result?;

        let inferred_ret = expected.cloned().unwrap_or_else(|| {
            if body_guarantees_return(body) {
                checked_ret
            } else {
                Ty::Unit
            }
        });
        Ok((
            Ty::Func {
                params: param_tys,
                param_effects: None,
                return_effect: None,
                ret: Box::new(inferred_ret),
            },
            param_ids,
        ))
    }

    pub(super) fn check_return_value(
        &mut self,
        value: Option<&Expr>,
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let Some(ret) = self.fn_ret.last().cloned() else {
            return Err(AliasError {
                msg: "顶层不允许 return".into(),
                span,
            });
        };
        if ret.is_unknown() {
            let inferred = match value {
                Some(expr) => require_value(self.expr(expr, env)?, expr.span())?,
                None => Ty::Unit,
            };
            let Some(slot) = self.fn_ret.last_mut() else {
                return Err(AliasError {
                    msg: "内部 sema 不变式被破坏: 推断 return 时函数栈为空".into(),
                    span,
                });
            };
            *slot = inferred.clone();
            return Ok(inferred);
        }
        match value {
            Some(_) if ret == Ty::Unit => Err(AliasError {
                msg: "unit 函数的 return 不能携带值".into(),
                span,
            }),
            Some(expr) => {
                self.expr_expected(expr, env, &ret).map_err(|error| {
                    let error = error.into_alias();
                    AliasError {
                        msg: format!("return 需要 {}: {}", ret.name(), error.msg),
                        span: error.span,
                    }
                })?;
                Ok(ret)
            }
            None if ret != Ty::Unit => Err(AliasError {
                msg: format!("return 需要 {}, 不能省略返回值", ret.name()),
                span,
            }),
            None => Ok(ret),
        }
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
                self.check_local_assignment(s, target, value, *span, env)?;
                Ok(None)
            }
            Stmt::FieldAssign {
                recv,
                field,
                value,
                span,
            } => {
                self.check_field_assignment(s, recv, field, value, *span, env)?;
                Ok(None)
            }
            Stmt::Expr { expr, .. } => {
                if let Expr::Call { callee, args, span } = expr {
                    if let Expr::Ident(name, _) = callee.as_ref() {
                        if let Some(builtin @ (BuiltinCall::Increase | BuiltinCall::Decrease)) =
                            resolved_builtin_call(name)
                        {
                            self.incdec(name, args, *span, env)?;
                            self.record_expr_type(expr, Ty::Unit);
                            self.record_call_target(expr, LowerCallTarget::Builtin(builtin));
                            self.record_resolved_callee(expr, &Ty::Unit);
                            return Ok(None);
                        }
                    }
                }
                self.expr(expr, env)?;
                Ok(None)
            }
            Stmt::Return { value, span } => {
                let ret = self.check_return_value(value.as_ref(), *span, env)?;
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
                ensure_user_lexical_name(name, *span)?;
                let declared = check_value_type_slot(ty, *span, &self.structs)?;
                self.for_types
                    .insert(s as *const Stmt as usize, declared.clone());
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
                let id = self.fresh_binding_id()?;
                self.for_ids.insert(s as *const Stmt as usize, id);
                let child = Scope::child(env);
                Scope::insert(
                    &child,
                    name.clone(),
                    VarInfo {
                        id,
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
        Stmt::Expr { expr, .. } => expr_all_arms_never(expr),
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
