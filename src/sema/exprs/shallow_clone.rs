//! Shallow-clone capability and resolved execution-plan owner.
//!
//! `ShallowCloneable(T)` answers whether copying T's ownership-safe aggregate structure can avoid
//! duplicating any unique dynamic ownership capability. User-level `shallow(x)` additionally
//! requires a real aggregate root; scalar `Inline` is only a recursive leaf and is not a legal
//! standalone shallow root.

use super::{require_value, ExprCheckError};
use super::super::{Checker, Env};
use crate::ast::{CallArg, Expr};
use crate::sema::hir::ShallowClonePlan;
use crate::sema::types::{types_match, Ty};
use crate::{AliasError, AliasResult, Span};
use std::collections::HashSet;

impl Checker {
    pub(super) fn check_shallow_call(
        &mut self,
        args: &[CallArg],
        span: Span,
        env: &Env,
        expected: Option<&Ty>,
    ) -> AliasResult<(Ty, ShallowClonePlan)> {
        let [arg] = args else {
            return Err(AliasError {
                msg: "shallow 恰好接受 1 个参数".into(),
                span,
            });
        };
        if arg.label.is_some() {
            return Err(AliasError {
                msg: "shallow 不接受命名实参".into(),
                span: arg.span,
            });
        }

        // shallow 与 clone 都读取 source 本身；直接函数名/this 不能触发零参裸名隐式调用，
        // 否则 shallow(f) 会先变成 shallow(f())，从而绕过 function-not-shallowable 规则。
        let direct_callable = matches!(&arg.value, Expr::Ident(..) | Expr::This(..));
        let ty = match (expected, direct_callable) {
            (Some(expected), true) => {
                let got = require_value(
                    self.expr_raw_callable(&arg.value, env)?,
                    arg.value.span(),
                )?;
                if !types_match(expected, &got) {
                    return Err(AliasError {
                        msg: format!(
                            "shallow 参数需要 {}, 实际 {}",
                            expected.name(),
                            got.name()
                        ),
                        span: arg.value.span(),
                    });
                }
                expected.clone()
            }
            (Some(expected), false) => require_value(
                self.expr_expected(&arg.value, env, expected)
                    .map_err(ExprCheckError::into_alias)?,
                arg.value.span(),
            )?,
            (None, true) => require_value(
                self.expr_raw_callable(&arg.value, env)?,
                arg.value.span(),
            )?,
            (None, false) => require_value(self.expr(&arg.value, env)?, arg.value.span())?,
        };

        let plan = shallow_clone_root_plan_with(&ty, span, &|name| {
            self.structs.get(name).map(|info| {
                info.fields
                    .iter()
                    .map(|field| field.ty.clone())
                    .collect::<Vec<_>>()
            })
        })?;
        Ok((ty, plan))
    }
}

/// User-level `shallow` root plan owner. The recursive predicate allows Inline leaves, but a root
/// Inline value has no independent ownership root to duplicate, so the operation is deliberately
/// not exposed for scalars even though they are safe children of a shallowable aggregate.
pub(in crate::sema) fn shallow_clone_root_plan_with<F>(
    ty: &Ty,
    span: Span,
    struct_fields: &F,
) -> AliasResult<ShallowClonePlan>
where
    F: Fn(&str) -> Option<Vec<Ty>>,
{
    if ty.contains_unknown() {
        return Err(AliasError {
            msg: format!("shallow 无法确定 {} 的完整静态类型", ty.name()),
            span,
        });
    }
    let mut visiting = HashSet::new();
    let plan = build_shallow_clone_plan(ty, span, struct_fields, &mut visiting)?;
    if matches!(&plan, ShallowClonePlan::Inline) {
        return Err(AliasError {
            msg: format!("类型 {} 不提供 shallow", ty.name()),
            span,
        });
    }
    Ok(plan)
}

fn build_shallow_clone_plan<F>(
    ty: &Ty,
    span: Span,
    struct_fields: &F,
    visiting: &mut HashSet<String>,
) -> AliasResult<ShallowClonePlan>
where
    F: Fn(&str) -> Option<Vec<Ty>>,
{
    Ok(match ty {
        Ty::Int(_) | Ty::UInt(_) | Ty::Float(_) | Ty::Bool => ShallowClonePlan::Inline,
        Ty::Struct(name) => {
            if !visiting.insert(name.clone()) {
                return Err(AliasError {
                    msg: format!(
                        "内部 sema 不变式被破坏: ShallowCloneable 结构体依赖出现递归 '{name}'"
                    ),
                    span,
                });
            }
            let fields = struct_fields(name).ok_or_else(|| AliasError {
                msg: format!("内部 sema 不变式被破坏: shallow 引用未知结构体 '{name}'"),
                span,
            })?;
            let plans = fields
                .iter()
                .map(|field| build_shallow_clone_plan(field, span, struct_fields, visiting))
                .collect::<AliasResult<Vec<_>>>()?;
            visiting.remove(name);
            ShallowClonePlan::Struct {
                name: name.clone(),
                fields: plans,
            }
        }
        Ty::Result(ok, err) => ShallowClonePlan::Result {
            ok: Box::new(build_shallow_clone_plan(ok, span, struct_fields, visiting)?),
            err: Box::new(build_shallow_clone_plan(err, span, struct_fields, visiting)?),
        },
        Ty::Str | Ty::Array(_) | Ty::Iterator(_) | Ty::Func { .. } | Ty::FuncPoly => {
            return Err(AliasError {
                msg: format!("类型 {} 不支持 shallow", ty.name()),
                span,
            });
        }
        Ty::Unit => {
            return Err(AliasError {
                msg: "unit 不是可 shallow 的值类型".into(),
                span,
            });
        }
        Ty::Unknown => {
            return Err(AliasError {
                msg: "shallow 无法确定参数的静态类型".into(),
                span,
            });
        }
    })
}
