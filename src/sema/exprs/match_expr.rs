use super::*;

impl Checker {
    pub(super) fn match_expr(
        &mut self,
        subject: &Expr,
        arms: &[MatchArm],
        span: Span,
        env: &Env,
        expected: Option<&Ty>,
    ) -> ExprCheckResult<Ty> {
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
                }
                .into());
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
                        }
                        .into());
                    }
                    if !seen_int.insert(*value) {
                        return Err(AliasError {
                            msg: format!("match 重复 Pattern: {value}"),
                            span: *pspan,
                        }
                        .into());
                    }
                    None
                }
                (Pattern::Int { span: pspan, .. }, _) => {
                    return Err(AliasError {
                        msg: format!("整数字面量 Pattern 不适用于 {}", st.name()),
                        span: *pspan,
                    }
                    .into());
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
                        }
                        .into());
                    }
                    *seen = true;
                    None
                }
                (Pattern::Bool { span: pspan, .. }, _) => {
                    return Err(AliasError {
                        msg: format!("bool Pattern 不适用于 {}", st.name()),
                        span: *pspan,
                    }
                    .into());
                }
                (Pattern::Str { value, span: pspan }, Ty::Str) => {
                    if !seen_str.insert(value.clone()) {
                        return Err(AliasError {
                            msg: format!("match 重复 Pattern: '{value}'"),
                            span: *pspan,
                        }
                        .into());
                    }
                    None
                }
                (Pattern::Str { span: pspan, .. }, _) => {
                    return Err(AliasError {
                        msg: format!("字符串 Pattern 不适用于 {}", st.name()),
                        span: *pspan,
                    }
                    .into());
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
                            }
                            .into());
                        }
                        CtorKind::Err if seen_err => {
                            return Err(AliasError {
                                msg: "match 重复覆盖 err 臂".into(),
                                span: *pspan,
                            }
                            .into());
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
                    }
                    .into());
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
                            }
                            .into());
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
            return Err(AliasError { msg, span }.into());
        }

        if let Some(want) = expected {
            Ok(want.clone())
        } else {
            // 没有任何产值臂意味着所有臂都显式终止控制流；最终 HIR 用 unit
            // 表示“该语句不产值”，不能把 lowering 临时 Unknown 泄漏到后端。
            Ok(common.unwrap_or(Ty::Unit))
        }
    }

    fn match_arm(
        &mut self,
        arm: &MatchArm,
        binding: Option<&(String, Ty)>,
        env: &Env,
        expected: Option<&Ty>,
    ) -> ExprCheckResult<Option<Ty>> {
        let local = Scope::child(env);
        if let Some((name, bind_ty)) = binding {
            let id = self.fresh_binding_id();
            self.match_binding_ids
                .insert(arm as *const MatchArm as usize, id);
            Scope::insert(
                &local,
                name.clone(),
                VarInfo {
                    id,
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
                self.check_return_value(Some(e), e.span(), &local)?;
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
                    Some(Stmt::Expr { expr, .. }) => Ok(Some(match expected {
                        Some(w) => self.expr_expected(expr, &local, w)?,
                        None => self.expr(expr, &local)?,
                    })),
                    _ => Ok(Some(Ty::Unit)),
                }
            }
        }
    }

    pub(super) fn propagate(&mut self, expr: &Expr, span: Span, env: &Env) -> AliasResult<Ty> {
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
}
