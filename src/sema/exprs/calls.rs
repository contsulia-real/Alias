use super::operators::require_value;
use super::typing::ExprCheckError;
use crate::ast::{CallArg, CtorKind, Expr};
use crate::builtins::{classify_call_builtin, classify_result_constructor, CallBuiltinName};
use crate::sema::hir::{BuiltinCall, MethodTarget};
use crate::sema::types::Ty;
use crate::sema::{builtin_method, Checker, Env, LowerCallTarget, MethodInfo, Scope};
use crate::{AliasError, AliasResult, Span};

impl Checker {
    pub(super) fn call(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        if let Expr::Ident(name, _) = callee {
            // User-defined struct constructors live in the top-level namespace, but current
            // language semantics allow a lexical binding to shadow that constructor name.
            // Scope lookup must therefore win before constructor classification.
            if Scope::get(env, name).is_none() && self.structs.contains_key(name) {
                return self.construct(name, args, span, env);
            }
            if let Some(kind) = classify_result_constructor(name) {
                return self.result_ctor(name, kind, args, span, env);
            }
        }
        for a in args {
            if let Some(lbl) = &a.label {
                return Err(AliasError {
                    msg: format!("函数调用不接受命名实参 '{lbl}'"),
                    span: a.span,
                });
            }
        }
        if let Expr::Ident(name, _) = callee {
            if let Some(builtin) = classify_call_builtin(name) {
                match builtin {
                    CallBuiltinName::Increase | CallBuiltinName::Decrease => {
                        return Err(AliasError {
                            msg: format!("{name} 只能作为独立语句使用"),
                            span,
                        });
                    }
                    CallBuiltinName::Print | CallBuiltinName::Println => {
                        let [arg] = args else {
                            return Err(AliasError {
                                msg: format!("{name} 恰好接受 1 个参数"),
                                span,
                            });
                        };
                        require_value(self.expr(&arg.value, env)?, arg.value.span())?;
                        return Ok(Ty::Unit);
                    }
                    CallBuiltinName::From | CallBuiltinName::TryFrom => {
                        let [arg] = args else {
                            return Err(AliasError {
                                msg: format!("{name} 恰好接受 1 个参数"),
                                span,
                            });
                        };
                        require_value(self.expr(&arg.value, env)?, arg.value.span())?;
                        return Err(AliasError {
                            msg: format!("{name} 需要目标类型上下文"),
                            span,
                        });
                    }
                    CallBuiltinName::Typeof => {
                        let [arg] = args else {
                            return Err(AliasError {
                                msg: "typeof 恰好接受 1 个参数".into(),
                                span,
                            });
                        };
                        let t = require_value(
                            self.expr_raw_callable(&arg.value, env)?,
                            arg.value.span(),
                        )?;
                        if t.contains_unknown() {
                            return Err(AliasError {
                                msg: "typeof 无法确定实参的静态类型".into(),
                                span: arg.value.span(),
                            });
                        }
                        return Ok(Ty::Str);
                    }
                }
            }
        }

        let ft = self.expr_raw_callable(callee, env)?;
        match ft {
            Ty::Func { params, ret } => {
                if args.len() != params.len() {
                    return Err(AliasError {
                        msg: format!("期望 {} 个参数, 实际 {} 个", params.len(), args.len()),
                        span,
                    });
                }
                for (i, (a, pt)) in args.iter().zip(&params).enumerate() {
                    match self.expr_expected(&a.value, env, pt) {
                        Ok(_) => {}
                        Err(ExprCheckError::Mismatch { actual, .. }) => {
                            return Err(AliasError {
                                msg: format!(
                                    "第 {} 个实参需要 {}, 实际 {}",
                                    i + 1,
                                    pt.name(),
                                    actual.name()
                                ),
                                span: a.value.span(),
                            });
                        }
                        Err(e) => return Err(e.into_alias()),
                    }
                }
                Ok(*ret)
            }
            Ty::FuncPoly | Ty::Unknown => {
                for a in args {
                    require_value(self.expr(&a.value, env)?, a.value.span())?;
                }
                Ok(Ty::Unknown)
            }
            other => Err(AliasError {
                msg: format!("{} 不是可调用值", other.name()),
                span,
            }),
        }
    }

    pub(super) fn resolve_call_target(&self, callee: &Expr, env: &Env) -> LowerCallTarget {
        let Expr::Ident(name, _) = callee else {
            return LowerCallTarget::FunctionValue;
        };
        // Mirror call(): lexical bindings shadow user struct constructors. Predefined names
        // cannot enter Scope, so their structured classification remains authoritative below.
        if Scope::get(env, name).is_some() {
            return LowerCallTarget::FunctionValue;
        }
        if self.structs.contains_key(name) {
            return LowerCallTarget::StructConstructor(name.clone());
        }
        if let Some(kind) = classify_result_constructor(name) {
            return LowerCallTarget::ResultConstructor(kind);
        }
        match classify_call_builtin(name) {
            Some(CallBuiltinName::Print) => LowerCallTarget::Builtin(BuiltinCall::Print),
            Some(CallBuiltinName::Println) => LowerCallTarget::Builtin(BuiltinCall::Println),
            Some(CallBuiltinName::Increase) => LowerCallTarget::Builtin(BuiltinCall::Increase),
            Some(CallBuiltinName::Decrease) => LowerCallTarget::Builtin(BuiltinCall::Decrease),
            Some(CallBuiltinName::Typeof) => LowerCallTarget::Typeof,
            // Successful contextual conversion checks record their resolved target before
            // ordinary call-target resolution. Without an expected type these calls fail.
            Some(CallBuiltinName::From | CallBuiltinName::TryFrom) | None => {
                LowerCallTarget::FunctionValue
            }
        }
    }

    fn construct(
        &mut self,
        name: &str,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let info = self.structs[name].clone();
        let mut covered = vec![false; info.fields.len()];
        for a in args {
            let Some(lbl) = &a.label else {
                return Err(AliasError {
                    msg: format!("结构体 {name} 构造必须使用命名实参"),
                    span: a.span,
                });
            };
            let Some(idx) = info.fields.iter().position(|f| &f.name == lbl) else {
                return Err(AliasError {
                    msg: format!("结构体 {name} 没有字段 '{lbl}'"),
                    span: a.span,
                });
            };
            if covered[idx] {
                return Err(AliasError {
                    msg: format!("结构体 {name} 构造重复指定字段 '{lbl}'"),
                    span: a.span,
                });
            }
            covered[idx] = true;
            // CallArg facts share the check→lower AST-address lifetime invariant documented
            // in Checker::expr_key/binding_id_for; no AST rewrite may occur between phases.
            self.ctor_arg_indices
                .insert(a as *const CallArg as usize, idx);
            let want = &info.fields[idx].ty;
            match self.expr_expected(&a.value, env, want) {
                Ok(_) => {}
                Err(ExprCheckError::Mismatch { actual, .. }) => {
                    return Err(AliasError {
                        msg: format!(
                            "字段 '{}' 需要 {}, 实际 {}",
                            lbl,
                            want.name(),
                            actual.name()
                        ),
                        span: a.value.span(),
                    });
                }
                Err(e) => return Err(e.into_alias()),
            }
        }
        for (f, done) in info.fields.iter().zip(&covered) {
            if !done && !f.has_default {
                return Err(AliasError {
                    msg: format!("结构体 {name} 构造缺少字段 '{}'", f.name),
                    span,
                });
            }
        }
        Ok(Ty::Struct(name.to_string()))
    }

    fn result_ctor(
        &mut self,
        name: &str,
        kind: CtorKind,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let [arg] = args else {
            return Err(AliasError {
                msg: format!("{name} 构造恰好接受 1 个参数"),
                span,
            });
        };
        if arg.label.is_some() {
            return Err(AliasError {
                msg: "result 构造不接受命名实参".into(),
                span: arg.span,
            });
        }
        let t = require_value(self.expr(&arg.value, env)?, arg.value.span())?;
        Ok(match kind {
            CtorKind::Ok => Ty::Result(Box::new(t), Box::new(Ty::Unknown)),
            CtorKind::Err => Ty::Result(Box::new(Ty::Unknown), Box::new(t)),
        })
    }

    pub(in crate::sema) fn incdec(
        &mut self,
        name: &str,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let [arg] = args else {
            return Err(AliasError {
                msg: format!("{name} 恰好接受 1 个参数"),
                span,
            });
        };
        let Expr::Ident(target, tspan) = &arg.value else {
            return Err(AliasError {
                msg: format!("{name} 的参数必须是可变绑定名"),
                span,
            });
        };
        let Some(info) = Scope::get(env, target) else {
            return Err(AliasError {
                msg: format!("'{target}' 未定义"),
                span: *tspan,
            });
        };
        if !info.mutable {
            return Err(AliasError {
                msg: format!("'{target}' 是 val 绑定, 不能 {name}"),
                span: *tspan,
            });
        }
        if info.ty.is_unknown() || info.ty.is_numeric() {
            self.record_binding_ref(&arg.value, info.id);
            self.record_expr_type(&arg.value, info.ty.clone());
            Ok(Ty::Unit)
        } else {
            Err(AliasError {
                msg: format!("{name} 需要数值类型, 实际 {}", info.ty.name()),
                span: *tspan,
            })
        }
    }

    pub(super) fn method_call(
        &mut self,
        recv: &Expr,
        name: &str,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let rt = self.expr(recv, env)?;
        self.method_call_with_receiver_ty(recv, rt, name, args, span, env)
    }

    pub(super) fn method_call_with_receiver_ty(
        &mut self,
        _recv: &Expr,
        rt: Ty,
        name: &str,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        if rt.is_unknown() {
            return Ok(Ty::Unknown);
        }
        for a in args {
            if let Some(lbl) = &a.label {
                return Err(AliasError {
                    msg: format!("方法调用不接受命名实参 '{lbl}'"),
                    span: a.span,
                });
            }
        }

        let rname = rt.name();
        let sig = if let Some(builtin) = builtin_method(&rt, name) {
            builtin
        } else {
            self.methods
                .get(&rname)
                .and_then(|m| m.get(name))
                .cloned()
                .ok_or_else(|| AliasError {
                    msg: format!("类型 {rname} 上没有方法 '{name}'"),
                    span,
                })?
        };
        if args.len() != sig.params().len() {
            return Err(AliasError {
                msg: format!("期望 {} 个参数, 实际 {} 个", sig.params().len(), args.len()),
                span,
            });
        }
        for (i, (a, want)) in args.iter().zip(sig.params()).enumerate() {
            match self.expr_expected(&a.value, env, want) {
                Ok(_) => {}
                Err(ExprCheckError::Mismatch { actual, .. }) => {
                    return Err(AliasError {
                        msg: format!(
                            "第 {} 个实参需要 {}, 实际 {}",
                            i + 1,
                            want.name(),
                            actual.name()
                        ),
                        span: a.value.span(),
                    });
                }
                Err(e) => return Err(e.into_alias()),
            }
        }
        Ok(sig.ret().clone())
    }
}

pub(super) fn resolve_method_target(
    checker: &Checker,
    recv: &Ty,
    name: &str,
    span: Span,
) -> AliasResult<MethodTarget> {
    if let Some(method) = builtin_method(recv, name) {
        return method.target(recv).ok_or_else(|| AliasError {
            msg: format!("内部 sema 不变式被破坏: 内建方法 {}.{name} 缺少 target", recv.name()),
            span,
        });
    }
    let rname = recv.name();
    checker
        .methods
        .get(&rname)
        .and_then(|table| table.get(name))
        .and_then(|method| method.target(recv))
        .ok_or_else(|| AliasError {
            msg: format!("内部 sema 不变式被破坏: 用户方法 {rname}.{name} 缺少 MethodId"),
            span,
        })
}
