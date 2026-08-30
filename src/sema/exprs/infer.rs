use super::calls::resolve_method_target;
use super::operators::{
    contextual_conversion, conversion_exists, literal_slot_unify, require_value,
};
use super::typing::ExprCheckError;
use crate::ast::{Expr, StrPartAst};
use crate::builtins::{classify_ownership_builtin, OwnershipBuiltinName};
use crate::sema::hir::BuiltinCall;
use crate::sema::types::{
    check_value_type_slot, default_negative_int_ty, default_positive_int_ty, types_match, FloatW,
    IntW, Ty,
};
use crate::sema::{Checker, Env, LowerCallTarget};
use crate::{AliasError, AliasResult};

impl Checker {
    pub(super) fn expr_inner(&mut self, e: &Expr, env: &Env) -> AliasResult<Ty> {
        match e {
            Expr::Int(value, ..) => Ok(default_positive_int_ty(*value)),
            Expr::Float(..) => Ok(Ty::Float(FloatW::F64)),
            Expr::Bool(..) => Ok(Ty::Bool),
            Expr::Str(parts, _) => {
                for p in parts {
                    if let StrPartAst::Hole(h) = p {
                        if contextual_conversion(h).is_some() {
                            self.expr_expected(h, env, &Ty::Str)
                                .map_err(ExprCheckError::into_alias)?;
                        } else {
                            require_value(self.expr(h, env)?, h.span())?;
                        }
                    }
                }
                Ok(Ty::Str)
            }
            Expr::Ident(..) | Expr::This(..) => Err(AliasError {
                msg: "内部 sema 不变式被破坏: 直接名字绕过统一解析入口".into(),
                span: e.span(),
            }),
            Expr::Cast { target, expr, span } => {
                let target_ty = check_value_type_slot(target, *span, &self.structs)?;
                let source_ty = require_value(self.expr(expr, env)?, expr.span())?;
                if conversion_exists(&source_ty, &target_ty) {
                    Ok(target_ty)
                } else {
                    Err(AliasError {
                        msg: format!("不存在 {} → {} 转换", source_ty.name(), target_ty.name()),
                        span: *span,
                    })
                }
            }
            Expr::Neg { expr, .. } => {
                if let Expr::Int(magnitude, span) = expr.as_ref() {
                    return default_negative_int_ty(*magnitude).ok_or_else(|| AliasError {
                        msg: "负整数字面量超出 i64 表示范围".into(),
                        span: *span,
                    });
                }
                let t = self.expr(expr, env)?;
                if t.is_unknown() {
                    return Ok(Ty::Unknown);
                }
                match t {
                    Ty::Int(w) => Ok(Ty::Int(w)),
                    Ty::Float(w) => Ok(Ty::Float(w)),
                    other => Err(AliasError {
                        msg: format!("取负需要有符号整数或浮点, 实际 {}", other.name()),
                        span: expr.span(),
                    }),
                }
            }
            Expr::Not { expr, .. } => {
                self.require_bool(expr, env, "! 操作数")?;
                Ok(Ty::Bool)
            }
            Expr::BitNot { expr, .. } => {
                let t = self.expr(expr, env)?;
                if t.is_unknown() {
                    return Ok(Ty::Unknown);
                }
                match t {
                    Ty::Int(w) => Ok(Ty::Int(w)),
                    Ty::UInt(w) => Ok(Ty::UInt(w)),
                    other => Err(AliasError {
                        msg: format!("~ 操作数需要整数, 实际 {}", other.name()),
                        span: expr.span(),
                    }),
                }
            }
            Expr::Binary { op, lhs, rhs, span } => {
                let l = self.expr(lhs, env)?;
                // 整数字面量先采用已确定的 lhs 槽宽，再记录到 rhs fact；若先按默认 i32
                // 检查，合法的 i64/u64 运算会在 binary type comparison 前被错误拒绝。
                let r = if matches!(&l, Ty::Int(_) | Ty::UInt(_)) {
                    match literal_slot_unify(&l, rhs) {
                        Some(r) => {
                            let ty = r.map_err(ExprCheckError::into_alias)?;
                            self.record_expr_type(rhs, ty.clone());
                            self.record_literal_components(rhs, &ty);
                            ty
                        }
                        None => self.expr(rhs, env)?,
                    }
                } else {
                    self.expr(rhs, env)?
                };
                self.binary(*op, l, r, *span)
            }
            Expr::Ternary {
                cond,
                then_expr,
                else_expr,
                span,
            } => {
                self.require_bool(cond, env, "?: 条件")?;
                let a = self.expr(then_expr, env)?;
                let b = self.expr(else_expr, env)?;
                if a.is_unknown() {
                    Ok(b)
                } else if b.is_unknown() || types_match(&a, &b) {
                    Ok(a)
                } else {
                    Err(AliasError {
                        msg: format!("?: 两个值分支类型不一致: {} 与 {}", a.name(), b.name()),
                        span: *span,
                    })
                }
            }
            Expr::Call { callee, args, span } => {
                let ownership_intrinsic = match callee.as_ref() {
                    Expr::Ident(name, _) => classify_ownership_builtin(name),
                    _ => None,
                };
                if let Some(intrinsic) = ownership_intrinsic {
                    // ownership-copy capability 与 execution plan 在同一次解析中完成并写入 fact；
                    // generic call resolution 永远不重新计算 clone/shallow 的语义计划。
                    let (ty, target) = match intrinsic {
                        OwnershipBuiltinName::Clone => {
                            let (ty, plan) = self.check_clone_call(args, *span, env, None)?;
                            (ty, BuiltinCall::DeepClone(plan))
                        }
                        OwnershipBuiltinName::Shallow => {
                            let (ty, plan) = self.check_shallow_call(args, *span, env, None)?;
                            (ty, BuiltinCall::ShallowClone(plan))
                        }
                        OwnershipBuiltinName::Borrow => {
                            let ty = self.check_borrow_call(e, args, *span, env)?;
                            self.record_call_target(e, LowerCallTarget::Borrow);
                            return Ok(ty);
                        }
                        OwnershipBuiltinName::Move => {
                            let ty = self.check_move_call(e, args, *span, env)?;
                            self.record_call_target(e, LowerCallTarget::Move);
                            return Ok(ty);
                        }
                    };
                    self.record_call_target(e, LowerCallTarget::Builtin(target));
                    Ok(ty)
                } else {
                    let ty = self.call(callee, args, *span, env)?;
                    let target = self.resolve_call_target(callee, args, env)?;
                    self.record_call_target(e, target);
                    Ok(ty)
                }
            }
            Expr::Juxtapose { lhs, rhs, span } => {
                let lhs_ty = self.expr_raw_callable(lhs, env)?;
                match &lhs_ty {
                    Ty::Func { params, ret } if params.len() == 1 => {
                        match self.expr_expected(rhs, env, &params[0]) {
                            Ok(_) => {}
                            Err(ExprCheckError::Mismatch { actual, .. }) => {
                                return Err(AliasError {
                                    msg: format!(
                                        "第 1 个实参需要 {}, 实际 {}",
                                        params[0].name(),
                                        actual.name()
                                    ),
                                    span: rhs.span(),
                                });
                            }
                            Err(error) => return Err(error.into_alias()),
                        }
                        self.record_call_target(e, LowerCallTarget::FunctionValue);
                        Ok((**ret).clone())
                    }
                    Ty::FuncPoly | Ty::Unknown => {
                        require_value(self.expr(rhs, env)?, rhs.span())?;
                        self.record_call_target(e, LowerCallTarget::FunctionValue);
                        Ok(Ty::Unknown)
                    }
                    _ => {
                        let Expr::Ident(name, _) = rhs.as_ref() else {
                            return Err(AliasError {
                                msg: format!("{} 不是可调用值", lhs_ty.name()),
                                span: *span,
                            });
                        };
                        let ty = self.method_call_with_receiver_ty(
                            lhs_ty.clone(),
                            name,
                            &[],
                            *span,
                            env,
                        )?;
                        let target = resolve_method_target(self, &lhs_ty, name, *span)?;
                        self.record_call_target(e, LowerCallTarget::Method(target));
                        Ok(ty)
                    }
                }
            }
            Expr::MethodCall {
                recv,
                name,
                args,
                span,
            } => {
                let ty = self.method_call(recv, name, args, *span, env)?;
                // method_call 必须先完成 receiver 检查并写入 fact，再据同一静态类型固化
                // MethodTarget；反过来按名字猜 target 会把 sema 决策泄漏给 lowering/backend。
                let recv_ty = self.expr_facts[&Self::expr_key(recv)].ty.clone();
                let target = resolve_method_target(self, &recv_ty, name, *span)?;
                self.record_call_target(e, LowerCallTarget::Method(target));
                Ok(ty)
            }
            Expr::Index { recv, idx, .. } => {
                let rt = self.expr(recv, env)?;
                if rt.is_unknown() {
                    return Ok(Ty::Unknown);
                }
                let Ty::Array(elem) = rt else {
                    return Err(AliasError {
                        msg: format!("下标访问需要 array 类型, 实际 {}", rt.name()),
                        span: recv.span(),
                    });
                };
                let it = self.expr(idx, env)?;
                if !it.is_unknown() && it != Ty::Int(IntW::W32) {
                    return Err(AliasError {
                        msg: format!("下标需要 i32, 实际 {}", it.name()),
                        span: idx.span(),
                    });
                }
                Ok(*elem)
            }
            Expr::ArrayLit { elems, .. } => {
                let mut elem_ty: Option<Ty> = None;
                for item in elems {
                    let t = require_value(self.expr(item, env)?, item.span())?;
                    self.record_owning_slot_read(item, env, &t)?;
                    match &elem_ty {
                        None => elem_ty = Some(t),
                        Some(first) if !types_match(first, &t) => {
                            return Err(AliasError {
                                msg: format!(
                                    "数组元素类型不一致: {} 与 {}",
                                    first.name(),
                                    t.name()
                                ),
                                span: item.span(),
                            });
                        }
                        _ => {}
                    }
                }
                Ok(Ty::Array(Box::new(elem_ty.unwrap_or(Ty::Unknown))))
            }
            Expr::Field { recv, name, span } => {
                let rt = self.expr(recv, env)?;
                if rt.is_unknown() {
                    return Ok(Ty::Unknown);
                }
                match rt {
                    Ty::Struct(s) => {
                        let (index, field) = self.struct_field(&s, name, *span)?;
                        let ty = field.ty.clone();
                        self.field_indices.insert(Self::expr_key(e), index);
                        Ok(ty)
                    }
                    other => Err(AliasError {
                        msg: format!("{} 没有字段 '{}'", other.name(), name),
                        span: *span,
                    }),
                }
            }
            Expr::FuncLit { params, body, span } => {
                let (ty, param_ids) = self.funclit(params, body, env, None, *span)?;
                if let Ty::Func {
                    params: param_types,
                    ..
                } = &ty
                {
                    self.record_params(params, param_types, &param_ids)?;
                }
                Ok(ty)
            }
            Expr::Match {
                subject,
                arms,
                span,
            } => self
                .match_expr(subject, arms, *span, env, None)
                .map_err(ExprCheckError::into_alias),
            Expr::Propagate { expr, span } => self.propagate(expr, *span, env),
        }
    }

    pub(super) fn require_bool(&mut self, e: &Expr, env: &Env, what: &str) -> AliasResult<()> {
        let t = self.expr(e, env)?;
        if t.is_unknown() || t == Ty::Bool {
            Ok(())
        } else {
            Err(AliasError {
                msg: format!("{what}需要 bool, 实际 {}", t.name()),
                span: e.span(),
            })
        }
    }
}
