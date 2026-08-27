//! sema::exprs — 表达式静态语义。

use super::types::{
    check_value_type_slot, default_negative_int_ty, default_positive_int_ty, int_literal_fits,
    types_match, FloatW, IntW, Ty,
};
use super::{literal_slot_unify, op_mismatch, Checker, Env, Scope, VarInfo};
use crate::ast::{ArmBody, BinOp, CallArg, CtorKind, Expr, MatchArm, Pattern, Stmt, StrPartAst};
use crate::{AliasError, AliasResult, Span};
use std::collections::HashSet;

impl Checker {
    /// 在已知目标类型语境中检查表达式。声明、赋值、参数、字段、三元与
    /// match 都走这一入口，使窄数值字面量的范围规则完全一致。
    pub(super) fn expr_expected(&mut self, e: &Expr, env: &Env, expected: &Ty) -> AliasResult<Ty> {
        if let Some((name, arg, span)) = contextual_conversion(e) {
            let source = require_value(self.expr(arg, env)?, arg.span())?;
            if conversion_exists(&source, expected) {
                return Ok(expected.clone());
            }
            if name == "from" {
                return Err(AliasError {
                    msg: format!("from 不存在 {} → {} 转换", source.name(), expected.name()),
                    span,
                });
            }
            return self.check_inferred_expected(arg, env, expected);
        }
        if let Some(r) = literal_slot_unify(expected, e) {
            return r;
        }
        match (e, expected) {
            (
                Expr::Ternary {
                    cond,
                    then_expr,
                    else_expr,
                    span: _,
                },
                expected,
            ) => {
                self.require_bool(cond, env, "?: 条件")?;
                self.expr_expected(then_expr, env, expected)?;
                self.expr_expected(else_expr, env, expected)?;
                Ok(expected.clone())
            }
            (
                Expr::Match {
                    subject,
                    arms,
                    span,
                },
                expected,
            ) => self.match_expr(subject, arms, *span, env, Some(expected)),
            (Expr::Binary { op, .. }, expected) if binary_flows_expected(*op, expected) => {
                let got = self.expr_with_numeric_literal_context(e, env, expected)?;
                if types_match(expected, &got) {
                    Ok(expected.clone())
                } else {
                    Err(AliasError {
                        msg: format!("需要 {}, 实际 {}", expected.name(), got.name()),
                        span: e.span(),
                    })
                }
            }
            (Expr::BitNot { expr, .. }, Ty::Int(_) | Ty::UInt(_)) => {
                self.expr_expected(expr, env, expected)?;
                Ok(expected.clone())
            }
            (Expr::ArrayLit { elems, .. }, Ty::Array(elem)) => {
                for item in elems {
                    match self.expr_expected(item, env, elem) {
                        Ok(_) => {}
                        Err(e) if e.msg.starts_with("需要 ") => {
                            let got = self.expr(item, env)?;
                            return Err(AliasError {
                                msg: format!(
                                    "数组元素类型不一致: {} 与 {}",
                                    elem.name(),
                                    got.name()
                                ),
                                span: item.span(),
                            });
                        }
                        Err(e) => return Err(e),
                    }
                }
                Ok(expected.clone())
            }
            (Expr::Call { callee, args, span }, Ty::Result(ok, err)) => {
                if let Expr::Ident(name, _) = callee.as_ref() {
                    if Scope::get(env, name).is_none() && (name == "ok" || name == "err") {
                        let [arg] = args.as_slice() else {
                            return Err(AliasError {
                                msg: format!("{name} 构造恰好接受 1 个参数"),
                                span: *span,
                            });
                        };
                        if arg.label.is_some() {
                            return Err(AliasError {
                                msg: "result 构造不接受命名实参".into(),
                                span: arg.span,
                            });
                        }
                        let payload = if name == "ok" {
                            ok.as_ref()
                        } else {
                            err.as_ref()
                        };
                        self.expr_expected(&arg.value, env, payload)?;
                        return Ok(expected.clone());
                    }
                }
                self.check_inferred_expected(e, env, expected)
            }
            _ => self.check_inferred_expected(e, env, expected),
        }
    }

    fn check_inferred_expected(&mut self, e: &Expr, env: &Env, expected: &Ty) -> AliasResult<Ty> {
        let got = require_value(self.expr(e, env)?, e.span())?;
        if types_match(expected, &got) {
            Ok(expected.clone())
        } else {
            Err(AliasError {
                msg: format!("需要 {}, 实际 {}", expected.name(), got.name()),
                span: e.span(),
            })
        }
    }

    /// 数值目标类型只负责给字面量提供声明宽度，不能把已有变量强行改型。
    /// 这样 `u8 x = 1 + 2` 仍按 u8 检查，而 `u32 + i32` 会进入统一
    /// 二元诊断并报告禁止隐式混算，而不是提前退化为“需要 u32”。
    fn expr_with_numeric_literal_context(
        &mut self,
        e: &Expr,
        env: &Env,
        expected: &Ty,
    ) -> AliasResult<Ty> {
        if let Some(result) = literal_slot_unify(expected, e) {
            return result;
        }
        let Expr::Binary { op, lhs, rhs, span } = e else {
            return self.expr(e, env);
        };
        if !binary_flows_expected(*op, expected) {
            return self.expr(e, env);
        }

        let lhs_ty = self.expr_with_numeric_literal_context(lhs, env, expected)?;
        let rhs_expected = if lhs_ty.is_numeric() {
            &lhs_ty
        } else {
            expected
        };
        let rhs_ty = self.expr_with_numeric_literal_context(rhs, env, rhs_expected)?;
        self.binary(*op, lhs_ty, rhs_ty, *span)
    }

    pub(super) fn expr(&mut self, e: &Expr, env: &Env) -> AliasResult<Ty> {
        match e {
            Expr::Int(value, ..) => Ok(default_positive_int_ty(*value)),
            Expr::Float(..) => Ok(Ty::Float(FloatW::F64)),
            Expr::Bool(..) => Ok(Ty::Bool),
            Expr::Str(parts, _) => {
                for p in parts {
                    if let StrPartAst::Hole(h) = p {
                        if contextual_conversion(h).is_some() {
                            self.expr_expected(h, env, &Ty::Str)?;
                        } else {
                            require_value(self.expr(h, env)?, h.span())?;
                        }
                    }
                }
                Ok(Ty::Str)
            }
            Expr::Ident(name, span) => {
                Scope::get(env, name)
                    .map(|info| info.ty)
                    .ok_or_else(|| AliasError {
                        msg: format!("未定义的绑定 '{name}'"),
                        span: *span,
                    })
            }
            Expr::This(span) => {
                Scope::get(env, "this")
                    .map(|info| info.ty)
                    .ok_or_else(|| AliasError {
                        msg: "this 只能出现在 func 体内".into(),
                        span: *span,
                    })
            }
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
                // 已知整数左操作数为 RHS 整数字面量提供目标槽类型。
                // 仅字面量参与，不放宽变量/表达式之间的隐式混算。
                let r = if matches!(&l, Ty::Int(_) | Ty::UInt(_)) {
                    match literal_slot_unify(&l, rhs) {
                        Some(r) => r?,
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
            Expr::Call { callee, args, span } => self.call(callee, args, *span, env),
            Expr::MethodCall {
                recv,
                name,
                args,
                span,
            } => self.method_call(recv, name, args, *span, env),
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
                        let info = &self.structs[&s];
                        info.fields
                            .iter()
                            .find(|f| f.name == *name)
                            .map(|f| f.ty.clone())
                            .ok_or_else(|| AliasError {
                                msg: format!("结构体 {s} 没有字段 '{name}'"),
                                span: *span,
                            })
                    }
                    other => Err(AliasError {
                        msg: format!("{} 没有字段 '{}'", other.name(), name),
                        span: *span,
                    }),
                }
            }
            Expr::FuncLit { params, body, span } => self.funclit(params, body, env, None, *span),
            Expr::Match {
                subject,
                arms,
                span,
            } => self.match_expr(subject, arms, *span, env, None),
            Expr::Propagate { expr, span } => self.propagate(expr, *span, env),
        }
    }

    fn require_bool(&mut self, e: &Expr, env: &Env, what: &str) -> AliasResult<()> {
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

    fn match_expr(
        &mut self,
        subject: &Expr,
        arms: &[MatchArm],
        span: Span,
        env: &Env,
        expected: Option<&Ty>,
    ) -> AliasResult<Ty> {
        let st = require_value(self.expr(subject, env)?, subject.span())?;
        if st.is_unknown() {
            return Ok(Ty::Unknown);
        }

        let mut seen_ok = false;
        let mut seen_err = false;
        let mut seen_true = false;
        let mut seen_false = false;
        let mut seen_int = HashSet::new();
        let mut seen_str = HashSet::new();
        let mut covered_all = false;
        let mut common: Option<Ty> = None;

        for arm in arms {
            if covered_all {
                return Err(AliasError {
                    msg: "match 存在不可达 Pattern".into(),
                    span: arm.pattern.span(),
                });
            }

            let binding: Option<(String, Ty)> = match (&arm.pattern, &st) {
                (Pattern::Wildcard { .. }, _) => None,
                (Pattern::Binding { name, .. }, _) => Some((name.clone(), st.clone())),
                (Pattern::Int { value, span: pspan }, Ty::Int(_) | Ty::UInt(_)) => {
                    let negative = *value < 0;
                    let magnitude = value.unsigned_abs() as u64;
                    if !int_literal_fits(&st, magnitude, negative) {
                        return Err(AliasError {
                            msg: format!("字面量 {value} 超出 {} 的表示范围", st.name()),
                            span: *pspan,
                        });
                    }
                    if !seen_int.insert(*value) {
                        return Err(AliasError {
                            msg: format!("match 重复 Pattern: {value}"),
                            span: *pspan,
                        });
                    }
                    None
                }
                (Pattern::Int { span: pspan, .. }, _) => {
                    return Err(AliasError {
                        msg: format!("整数字面量 Pattern 不适用于 {}", st.name()),
                        span: *pspan,
                    });
                }
                (Pattern::Bool { value, span: pspan }, Ty::Bool) => {
                    let seen = if *value {
                        &mut seen_true
                    } else {
                        &mut seen_false
                    };
                    if *seen {
                        return Err(AliasError {
                            msg: format!("match 重复 Pattern: {value}"),
                            span: *pspan,
                        });
                    }
                    *seen = true;
                    None
                }
                (Pattern::Bool { span: pspan, .. }, _) => {
                    return Err(AliasError {
                        msg: format!("bool Pattern 不适用于 {}", st.name()),
                        span: *pspan,
                    });
                }
                (Pattern::Str { value, span: pspan }, Ty::Str) => {
                    if !seen_str.insert(value.clone()) {
                        return Err(AliasError {
                            msg: format!("match 重复 Pattern: '{value}'"),
                            span: *pspan,
                        });
                    }
                    None
                }
                (Pattern::Str { span: pspan, .. }, _) => {
                    return Err(AliasError {
                        msg: format!("字符串 Pattern 不适用于 {}", st.name()),
                        span: *pspan,
                    });
                }
                (
                    Pattern::Constructor {
                        ctor,
                        binding,
                        span: pspan,
                    },
                    Ty::Result(ok_ty, err_ty),
                ) => {
                    match ctor {
                        CtorKind::Ok if seen_ok => {
                            return Err(AliasError {
                                msg: "match 重复覆盖 ok 臂".into(),
                                span: *pspan,
                            });
                        }
                        CtorKind::Err if seen_err => {
                            return Err(AliasError {
                                msg: "match 重复覆盖 err 臂".into(),
                                span: *pspan,
                            });
                        }
                        CtorKind::Ok => seen_ok = true,
                        CtorKind::Err => seen_err = true,
                    }
                    binding.as_ref().map(|name| {
                        let ty = match ctor {
                            CtorKind::Ok => (**ok_ty).clone(),
                            CtorKind::Err => (**err_ty).clone(),
                        };
                        (name.clone(), ty)
                    })
                }
                (Pattern::Constructor { span: pspan, .. }, _) => {
                    return Err(AliasError {
                        msg: format!("构造器 Pattern 需要 result 主语, 实际 {}", st.name()),
                        span: *pspan,
                    });
                }
            };

            let arm_ty = self.match_arm(arm, binding.as_ref(), env, expected)?;
            if expected.is_none() {
                if let Some(t) = arm_ty {
                    common = Some(match common.take() {
                        None => t,
                        Some(a) if a.is_unknown() => t,
                        Some(a) if t.is_unknown() || types_match(&a, &t) => a,
                        Some(a) => {
                            return Err(AliasError {
                                msg: format!("match 各臂类型不一致: {} 与 {}", a.name(), t.name()),
                                span: arm.span,
                            });
                        }
                    });
                }
            }

            covered_all = matches!(
                &arm.pattern,
                Pattern::Wildcard { .. } | Pattern::Binding { .. }
            ) || (matches!(&st, Ty::Result(_, _)) && seen_ok && seen_err)
                || (st == Ty::Bool && seen_true && seen_false);
        }

        if !covered_all {
            let msg = match &st {
                Ty::Result(_, _) => "match 必须同时覆盖 ok 与 err，或提供兜底 Pattern".into(),
                Ty::Bool => "match bool 必须覆盖 true 与 false，或提供兜底 Pattern".into(),
                _ => format!("match {} 必须提供 _ 或绑定 Pattern 作为兜底", st.name()),
            };
            return Err(AliasError { msg, span });
        }

        if let Some(want) = expected {
            Ok(want.clone())
        } else {
            Ok(common.unwrap_or(Ty::Unknown))
        }
    }

    fn match_arm(
        &mut self,
        arm: &MatchArm,
        binding: Option<&(String, Ty)>,
        env: &Env,
        expected: Option<&Ty>,
    ) -> AliasResult<Option<Ty>> {
        let local = Scope::child(env);
        if let Some((name, bind_ty)) = binding {
            Scope::insert(
                &local,
                name.clone(),
                VarInfo {
                    ty: bind_ty.clone(),
                    mutable: false,
                },
            );
        }
        match &arm.body {
            ArmBody::Value(e) => Ok(Some(match expected {
                Some(w) => self.expr_expected(e, &local, w)?,
                None => self.expr(e, &local)?,
            })),
            ArmBody::Ret(e) => {
                let Some(ret) = self.fn_ret.last().cloned() else {
                    return Err(AliasError {
                        msg: "顶层不允许 return".into(),
                        span: e.span(),
                    });
                };
                self.expr_expected(e, &local, &ret)?;
                Ok(None)
            }
            ArmBody::Block(stmts) => {
                for s in stmts {
                    self.stmt(s, &local)?;
                }
                if crate::sema::stmts::block_terminates_with_return(stmts) {
                    return Ok(None);
                }
                match stmts.last() {
                    Some(Stmt::ExprStmt { expr, .. }) => Ok(Some(match expected {
                        Some(w) => self.expr_expected(expr, &local, w)?,
                        None => self.expr(expr, &local)?,
                    })),
                    _ => Ok(Some(Ty::Unit)),
                }
            }
        }
    }

    fn propagate(&mut self, expr: &Expr, span: Span, env: &Env) -> AliasResult<Ty> {
        let t = self.expr(expr, env)?;
        if t.is_unknown() {
            return Ok(Ty::Unknown);
        }
        let Ty::Result(v_ty, e_ty) = t else {
            return Err(AliasError {
                msg: format!("? 只能作用于 result 值, 实际 {}", t.name()),
                span,
            });
        };
        let Some(ret) = self.fn_ret.last().cloned() else {
            return Err(AliasError {
                msg: "? 需要所在函数返回 result 类型".into(),
                span,
            });
        };
        let Ty::Result(_, fn_e) = &ret else {
            return Err(AliasError {
                msg: format!("? 需要所在函数返回 result 类型, 实际 {}", ret.name()),
                span,
            });
        };
        if !types_match(fn_e, &e_ty) {
            return Err(AliasError {
                msg: format!(
                    "? 错误类型不匹配: 表达式错误为 {}, 所在函数错误为 {}",
                    e_ty.name(),
                    fn_e.name()
                ),
                span,
            });
        }
        Ok(*v_ty)
    }

    fn binary(&mut self, op: BinOp, l: Ty, r: Ty, span: Span) -> AliasResult<Ty> {
        use BinOp::*;
        if l.is_unknown() || r.is_unknown() {
            return Ok(Ty::Unknown);
        }
        if matches!(op, And | Or) {
            return if l == Ty::Bool && r == Ty::Bool {
                Ok(Ty::Bool)
            } else {
                Err(op_mismatch(op, &l, &r, span))
            };
        }
        let mixed = |span| {
            if l.is_numeric() && r.is_numeric() && l != r {
                AliasError {
                    msg: format!("{} 与 {} 禁止隐式混算", l.name(), r.name()),
                    span,
                }
            } else {
                op_mismatch(op, &l, &r, span)
            }
        };
        match op {
            Add | Sub | Mul | Div => match (&l, &r) {
                (Ty::Int(a), Ty::Int(b)) if a == b => Ok(Ty::Int(*a)),
                (Ty::UInt(a), Ty::UInt(b)) if a == b => Ok(Ty::UInt(*a)),
                (Ty::Float(a), Ty::Float(b)) if a == b => Ok(Ty::Float(*a)),
                _ => Err(mixed(span)),
            },
            Rem => match (&l, &r) {
                (Ty::Int(a), Ty::Int(b)) if a == b => Ok(Ty::Int(*a)),
                (Ty::UInt(a), Ty::UInt(b)) if a == b => Ok(Ty::UInt(*a)),
                _ => Err(mixed(span)),
            },
            Shl | Shr | BitAnd | BitXor | BitOr => match (&l, &r) {
                (Ty::Int(a), Ty::Int(b)) if a == b => Ok(Ty::Int(*a)),
                (Ty::UInt(a), Ty::UInt(b)) if a == b => Ok(Ty::UInt(*a)),
                _ => Err(mixed(span)),
            },
            Lt | Le | Gt | Ge | EqEq | NotEq => match (&l, &r) {
                (Ty::Int(a), Ty::Int(b)) if a == b => Ok(Ty::Bool),
                (Ty::UInt(a), Ty::UInt(b)) if a == b => Ok(Ty::Bool),
                (Ty::Float(a), Ty::Float(b)) if a == b => Ok(Ty::Bool),
                (Ty::Str, Ty::Str) => Ok(Ty::Bool),
                (Ty::Bool, Ty::Bool) if matches!(op, EqEq | NotEq) => Ok(Ty::Bool),
                _ => Err(mixed(span)),
            },
            And | Or => unreachable!(),
        }
    }

    fn call(&mut self, callee: &Expr, args: &[CallArg], span: Span, env: &Env) -> AliasResult<Ty> {
        if let Expr::Ident(name, _) = callee {
            if Scope::get(env, name).is_none() && self.structs.contains_key(name) {
                return self.construct(name, args, span, env);
            }
            if Scope::get(env, name).is_none() && (name == "ok" || name == "err") {
                return self.result_ctor(name, args, span, env);
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
            if name == "increase" || name == "decrease" {
                return Err(AliasError {
                    msg: format!("{name} 只能作为独立语句使用"),
                    span,
                });
            }
            if name == "println" || name == "print" {
                let [arg] = args else {
                    return Err(AliasError {
                        msg: format!("{name} 恰好接受 1 个参数"),
                        span,
                    });
                };
                require_value(self.expr(&arg.value, env)?, arg.value.span())?;
                return Ok(Ty::Unit);
            }
            if name == "from" || name == "try_from" {
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
            if name == "typeof" {
                let [arg] = args else {
                    return Err(AliasError {
                        msg: "typeof 恰好接受 1 个参数".into(),
                        span,
                    });
                };
                let t = require_value(self.expr(&arg.value, env)?, arg.value.span())?;
                if t.contains_unknown() {
                    return Err(AliasError {
                        msg: "typeof 无法确定实参的静态类型".into(),
                        span: arg.value.span(),
                    });
                }
                return Ok(Ty::Str);
            }
        }
        let ft = self.expr(callee, env)?;
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
                        Err(e) if e.msg.starts_with("需要 ") => {
                            let got = self.expr(&a.value, env)?;
                            return Err(AliasError {
                                msg: format!(
                                    "第 {} 个实参需要 {}, 实际 {}",
                                    i + 1,
                                    pt.name(),
                                    got.name()
                                ),
                                span: a.value.span(),
                            });
                        }
                        Err(e) => return Err(e),
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
            let want = &info.fields[idx].ty;
            match self.expr_expected(&a.value, env, want) {
                Ok(_) => {}
                Err(e) if e.msg.starts_with("需要 ") => {
                    let got = self.expr(&a.value, env)?;
                    return Err(AliasError {
                        msg: format!("字段 '{}' 需要 {}, 实际 {}", lbl, want.name(), got.name()),
                        span: a.value.span(),
                    });
                }
                Err(e) => return Err(e),
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
        Ok(if name == "ok" {
            Ty::Result(Box::new(t), Box::new(Ty::Unknown))
        } else {
            Ty::Result(Box::new(Ty::Unknown), Box::new(t))
        })
    }

    pub(super) fn incdec(
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
            Ok(Ty::Unit)
        } else {
            Err(AliasError {
                msg: format!("{name} 需要数值类型, 实际 {}", info.ty.name()),
                span: *tspan,
            })
        }
    }

    fn method_call(
        &mut self,
        recv: &Expr,
        name: &str,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let rt = self.expr(recv, env)?;
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

        // array 的固有方法优先；其他名字继续进入完整类型扩展方法表。
        if let Ty::Array(elem) = &rt {
            match name {
                "len" => {
                    if !args.is_empty() {
                        return Err(AliasError {
                            msg: format!("期望 0 个参数, 实际 {} 个", args.len()),
                            span,
                        });
                    }
                    return Ok(Ty::Int(IntW::W32));
                }
                "push" => {
                    if args.len() != 1 {
                        return Err(AliasError {
                            msg: format!("期望 1 个参数, 实际 {} 个", args.len()),
                            span,
                        });
                    }
                    match self.expr_expected(&args[0].value, env, elem) {
                        Ok(_) => {}
                        Err(e) if e.msg.starts_with("需要 ") => {
                            let got = self.expr(&args[0].value, env)?;
                            return Err(AliasError {
                                msg: format!(
                                    "第 1 个实参需要 {}, 实际 {}",
                                    elem.name(),
                                    got.name()
                                ),
                                span: args[0].value.span(),
                            });
                        }
                        Err(e) => return Err(e),
                    }
                    return Ok(Ty::Unit);
                }
                "pop" => {
                    if !args.is_empty() {
                        return Err(AliasError {
                            msg: format!("期望 0 个参数, 实际 {} 个", args.len()),
                            span,
                        });
                    }
                    return Ok((**elem).clone());
                }
                "iterator" => {
                    if !args.is_empty() {
                        return Err(AliasError {
                            msg: format!("期望 0 个参数, 实际 {} 个", args.len()),
                            span,
                        });
                    }
                    return Ok(Ty::Iterator(elem.clone()));
                }
                _ => {}
            }
        }

        let rname = rt.name();
        let Some(sig) = self.methods.get(&rname).and_then(|m| m.get(name)).cloned() else {
            return Err(AliasError {
                msg: format!("类型 {rname} 上没有方法 '{name}'"),
                span,
            });
        };
        if args.len() != sig.params.len() {
            return Err(AliasError {
                msg: format!("期望 {} 个参数, 实际 {} 个", sig.params.len(), args.len()),
                span,
            });
        }
        for (i, (a, want)) in args.iter().zip(&sig.params).enumerate() {
            match self.expr_expected(&a.value, env, want) {
                Ok(_) => {}
                Err(e) if e.msg.starts_with("需要 ") => {
                    let got = self.expr(&a.value, env)?;
                    return Err(AliasError {
                        msg: format!(
                            "第 {} 个实参需要 {}, 实际 {}",
                            i + 1,
                            want.name(),
                            got.name()
                        ),
                        span: a.value.span(),
                    });
                }
                Err(e) => return Err(e),
            }
        }
        Ok(sig.ret)
    }
}

fn binary_flows_expected(op: BinOp, expected: &Ty) -> bool {
    use BinOp::*;
    match op {
        Add | Sub | Mul | Div => expected.is_numeric(),
        Rem | Shl | Shr | BitAnd | BitXor | BitOr => {
            matches!(expected, Ty::Int(_) | Ty::UInt(_))
        }
        Lt | Le | Gt | Ge | EqEq | NotEq | And | Or => false,
    }
}

fn conversion_exists(source: &Ty, target: &Ty) -> bool {
    (source.is_numeric() && target.is_numeric())
        || (matches!(target, Ty::Str) && !source.is_unknown() && *source != Ty::Unit)
}

fn require_value(ty: Ty, span: Span) -> AliasResult<Ty> {
    if ty == Ty::Unit {
        Err(AliasError {
            msg: "无返回值表达式不能用于值位置".into(),
            span,
        })
    } else {
        Ok(ty)
    }
}

fn contextual_conversion(e: &Expr) -> Option<(&str, &Expr, Span)> {
    let Expr::Call { callee, args, span } = e else {
        return None;
    };
    let Expr::Ident(name, _) = callee.as_ref() else {
        return None;
    };
    if name != "from" && name != "try_from" {
        return None;
    }
    let [arg] = args.as_slice() else {
        return None;
    };
    Some((name, &arg.value, *span))
}
