use super::operators::{
    binary_flows_expected, contextual_conversion, conversion_exists, literal_slot_unify,
    require_value,
};
use crate::ast::{CtorKind, Expr};
use crate::builtins::is_result_constructor;
use crate::sema::hir::{BindingId, ResolvedConversion};
use crate::sema::types::{types_match, Ty};
use crate::sema::{Checker, Env, LowerCallTarget, LowerExprInfo, Scope};
use crate::{AliasError, AliasResult, Span};

pub(in crate::sema) type ExprCheckResult<T> = Result<T, ExprCheckError>;

pub(in crate::sema) enum ExprCheckError {
    Mismatch {
        expected: Ty,
        actual: Ty,
        span: Span,
    },
    LiteralOutOfRange {
        literal: String,
        expected: Ty,
        span: Span,
    },
    Diagnostic(AliasError),
}

impl ExprCheckError {
    pub(in crate::sema) fn into_alias(self) -> AliasError {
        match self {
            Self::Mismatch {
                expected,
                actual,
                span,
            } => AliasError {
                msg: format!("需要 {}, 实际 {}", expected.name(), actual.name()),
                span,
            },
            Self::LiteralOutOfRange {
                literal,
                expected,
                span,
            } => AliasError {
                msg: format!("字面量 {literal} 超出 {} 的表示范围", expected.name()),
                span,
            },
            Self::Diagnostic(error) => error,
        }
    }
}

impl From<AliasError> for ExprCheckError {
    fn from(error: AliasError) -> Self {
        Self::Diagnostic(error)
    }
}

impl Checker {
    pub(super) fn expr_key(e: &Expr) -> usize {
        // expr facts 与 binding facts 共享同一阶段约束：key 是本次 check→lower 调用链内
        // AST 节点的临时地址，不是持久 identity。两阶段之间移动/替换 AST 会让 facts
        // 错配；若未来需要 AST 重写，必须先引入稳定 NodeId，而不是继续依赖地址。
        e as *const Expr as usize
    }

    pub(super) fn record_binding_ref(&mut self, e: &Expr, id: BindingId) {
        self.expr_binding_ids.insert(Self::expr_key(e), id);
    }

    pub(in crate::sema) fn record_expr_type(&mut self, e: &Expr, ty: Ty) {
        self.expr_facts
            .entry(Self::expr_key(e))
            .and_modify(|info| info.ty = ty.clone())
            .or_insert(LowerExprInfo {
                ty,
                call_target: None,
                implicit_zero_callee: None,
            });
    }

    pub(super) fn record_literal_components(&mut self, e: &Expr, ty: &Ty) {
        if let Expr::Neg { expr, .. } = e {
            if matches!(expr.as_ref(), Expr::Int(..)) {
                self.record_expr_type(expr, ty.clone());
            }
        }
    }

    pub(in crate::sema) fn record_call_target(&mut self, e: &Expr, target: LowerCallTarget) {
        self.expr_facts
            .entry(Self::expr_key(e))
            .and_modify(|info| info.call_target = Some(target.clone()))
            .or_insert(LowerExprInfo {
                ty: Ty::Unknown,
                call_target: Some(target),
                implicit_zero_callee: None,
            });
    }

    fn record_implicit_zero_call(&mut self, e: &Expr, callee_ty: Ty, result_ty: Ty) {
        self.expr_facts
            .entry(Self::expr_key(e))
            .and_modify(|info| {
                info.ty = result_ty.clone();
                info.call_target = Some(LowerCallTarget::FunctionValue);
                info.implicit_zero_callee = Some(callee_ty.clone());
            })
            .or_insert(LowerExprInfo {
                ty: result_ty,
                call_target: Some(LowerCallTarget::FunctionValue),
                implicit_zero_callee: Some(callee_ty),
            });
    }

    pub(in crate::sema) fn record_resolved_callee(&mut self, e: &Expr, result: &Ty) {
        let Expr::Call { callee, args, .. } = e else {
            return;
        };
        let Some(target) = self
            .expr_facts
            .get(&Self::expr_key(e))
            .and_then(|info| info.call_target.as_ref())
        else {
            return;
        };
        if *target == LowerCallTarget::FunctionValue {
            return;
        }
        let params = args
            .iter()
            .map(|arg| self.expr_facts[&Self::expr_key(&arg.value)].ty.clone())
            .collect();
        self.record_expr_type(
            callee,
            Ty::Func {
                params,
                ret: Box::new(result.clone()),
            },
        );
    }

    pub(in crate::sema) fn record_params(
        &mut self,
        params: &[crate::ast::Param],
        types: &[Ty],
        ids: &[BindingId],
    ) {
        assert_eq!(params.len(), types.len());
        assert_eq!(params.len(), ids.len());
        for ((param, ty), id) in params.iter().zip(types).zip(ids) {
            // Param facts use the same transient AST-address identity as expression facts.
            let key = param as *const crate::ast::Param as usize;
            self.param_types.insert(key, ty.clone());
            self.param_ids.insert(key, *id);
        }
    }

    /// 取得一个直接函数值引用的原始类型，不触发“零参裸名调用”。
    pub(super) fn expr_raw_callable(&mut self, e: &Expr, env: &Env) -> AliasResult<Ty> {
        let ty = match e {
            Expr::Ident(name, span) => {
                let info = Scope::get(env, name).ok_or_else(|| AliasError {
                    msg: format!("未定义的绑定 '{name}'"),
                    span: *span,
                })?;
                self.record_binding_ref(e, info.id);
                info.ty
            }
            Expr::This(span) => {
                Scope::get(env, "this")
                    .map(|info| info.ty)
                    .ok_or_else(|| AliasError {
                        msg: "this 只能出现在 func 体内".into(),
                        span: *span,
                    })?
            }
            _ => return self.expr(e, env),
        };
        self.record_expr_type(e, ty.clone());
        Ok(ty)
    }

    fn maybe_implicit_zero_call(&mut self, e: &Expr, raw: Ty) -> Ty {
        let Ty::Func { params, ret } = &raw else {
            return raw;
        };
        if !params.is_empty() {
            return raw;
        }
        let result = (**ret).clone();
        self.record_implicit_zero_call(e, raw, result.clone());
        result
    }

    pub(in crate::sema) fn expr_expected(
        &mut self,
        e: &Expr,
        env: &Env,
        expected: &Ty,
    ) -> ExprCheckResult<Ty> {
        let ty = self.expr_expected_inner(e, env, expected)?;
        self.record_expr_type(e, ty.clone());
        self.record_literal_components(e, &ty);
        self.record_resolved_callee(e, &ty);
        Ok(ty)
    }

    fn expr_expected_inner(&mut self, e: &Expr, env: &Env, expected: &Ty) -> ExprCheckResult<Ty> {
        if matches!(expected, Ty::Func { .. } | Ty::FuncPoly)
            && matches!(e, Expr::Ident(..) | Expr::This(..))
        {
            let got = require_value(self.expr_raw_callable(e, env)?, e.span())?;
            return if types_match(expected, &got) {
                Ok(expected.clone())
            } else {
                Err(ExprCheckError::Mismatch {
                    expected: expected.clone(),
                    actual: got,
                    span: e.span(),
                })
            };
        }
        if let Some((name, arg, span)) = contextual_conversion(e) {
            let source = require_value(self.expr(arg, env)?, arg.span())?;
            if conversion_exists(&source, expected) {
                self.record_call_target(
                    e,
                    LowerCallTarget::ContextualConversion(ResolvedConversion::Convert),
                );
                return Ok(expected.clone());
            }
            if name == "from" {
                return Err(AliasError {
                    msg: format!("from 不存在 {} → {} 转换", source.name(), expected.name()),
                    span,
                }
                .into());
            }
            self.record_call_target(
                e,
                LowerCallTarget::ContextualConversion(ResolvedConversion::Identity),
            );
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
                    Err(ExprCheckError::Mismatch {
                        expected: expected.clone(),
                        actual: got,
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
                        Err(ExprCheckError::Mismatch { actual, .. }) => {
                            return Err(AliasError {
                                msg: format!(
                                    "数组元素类型不一致: {} 与 {}",
                                    elem.name(),
                                    actual.name()
                                ),
                                span: item.span(),
                            }
                            .into());
                        }
                        Err(e) => return Err(e),
                    }
                }
                Ok(expected.clone())
            }
            (Expr::Call { callee, args, span }, Ty::Result(ok, err)) => {
                if let Expr::Ident(name, _) = callee.as_ref() {
                    if is_result_constructor(name) {
                        self.record_call_target(
                            e,
                            LowerCallTarget::ResultConstructor(if name == "ok" {
                                CtorKind::Ok
                            } else {
                                CtorKind::Err
                            }),
                        );
                        let [arg] = args.as_slice() else {
                            return Err(AliasError {
                                msg: format!("{name} 构造恰好接受 1 个参数"),
                                span: *span,
                            }
                            .into());
                        };
                        if arg.label.is_some() {
                            return Err(AliasError {
                                msg: "result 构造不接受命名实参".into(),
                                span: arg.span,
                            }
                            .into());
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

    fn check_inferred_expected(
        &mut self,
        e: &Expr,
        env: &Env,
        expected: &Ty,
    ) -> ExprCheckResult<Ty> {
        let got = require_value(self.expr(e, env)?, e.span())?;
        if types_match(expected, &got) {
            Ok(expected.clone())
        } else {
            Err(ExprCheckError::Mismatch {
                expected: expected.clone(),
                actual: got,
                span: e.span(),
            })
        }
    }

    fn expr_with_numeric_literal_context(
        &mut self,
        e: &Expr,
        env: &Env,
        expected: &Ty,
    ) -> ExprCheckResult<Ty> {
        let ty = self.expr_with_numeric_literal_context_inner(e, env, expected)?;
        self.record_expr_type(e, ty.clone());
        self.record_literal_components(e, &ty);
        self.record_resolved_callee(e, &ty);
        Ok(ty)
    }

    fn expr_with_numeric_literal_context_inner(
        &mut self,
        e: &Expr,
        env: &Env,
        expected: &Ty,
    ) -> ExprCheckResult<Ty> {
        if let Some(result) = literal_slot_unify(expected, e) {
            return result;
        }
        if let Some((name, arg, span)) = contextual_conversion(e) {
            let source = require_value(self.expr(arg, env)?, arg.span())?;
            if conversion_exists(&source, expected) {
                self.record_call_target(
                    e,
                    LowerCallTarget::ContextualConversion(ResolvedConversion::Convert),
                );
                return Ok(expected.clone());
            }
            if name == "from" {
                return Err(AliasError {
                    msg: format!("from 不存在 {} → {} 转换", source.name(), expected.name()),
                    span,
                }
                .into());
            }
            self.record_call_target(
                e,
                LowerCallTarget::ContextualConversion(ResolvedConversion::Identity),
            );
            return Ok(source);
        }
        let Expr::Binary { op, lhs, rhs, span } = e else {
            return Ok(self.expr(e, env)?);
        };
        if !binary_flows_expected(*op, expected) {
            return Ok(self.expr(e, env)?);
        }

        let lhs_ty = self.expr_with_numeric_literal_context(lhs, env, expected)?;
        let rhs_expected = if lhs_ty.is_numeric() {
            &lhs_ty
        } else {
            expected
        };
        let rhs_ty = self.expr_with_numeric_literal_context(rhs, env, rhs_expected)?;
        Ok(self.binary(*op, lhs_ty, rhs_ty, *span)?)
    }

    pub(in crate::sema) fn expr(&mut self, e: &Expr, env: &Env) -> AliasResult<Ty> {
        let ty = if matches!(e, Expr::Ident(..) | Expr::This(..)) {
            let raw = self.expr_raw_callable(e, env)?;
            self.maybe_implicit_zero_call(e, raw)
        } else {
            self.expr_inner(e, env)?
        };
        self.record_expr_type(e, ty.clone());
        self.record_resolved_callee(e, &ty);
        Ok(ty)
    }
}
