//! Deep-clone capability and resolved execution-plan owner.
//!
//! This module decides DeepCloneable(T). Codegen consumes the resulting DeepClonePlan and must
//! not reconstruct cloneability from Ty/VTy or physical representation.

use super::{require_value, ExprCheckError};
use super::super::{Checker, Env};
use crate::ast::{CallArg, Expr};
use crate::sema::hir::DeepClonePlan;
use crate::sema::types::{types_match, Ty};
use crate::{AliasError, AliasResult, Span};
use std::collections::HashSet;

impl Checker {
    pub(super) fn check_clone_call(
        &mut self,
        args: &[CallArg],
        span: Span,
        env: &Env,
        expected: Option<&Ty>,
    ) -> AliasResult<(Ty, DeepClonePlan)> {
        let [arg] = args else {
            return Err(AliasError {
                msg: "clone 恰好接受 1 个参数".into(),
                span,
            });
        };
        if arg.label.is_some() {
            return Err(AliasError {
                msg: "clone 不接受命名实参".into(),
                span: arg.span,
            });
        }

        // clone 读取 source 本身。直接函数名/this 不能触发普通表达式入口的零参隐式调用，
        // 否则 clone(f) 会错误地变成 clone(f()) 并绕过 function-not-cloneable 规则。
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
                            "clone 参数需要 {}, 实际 {}",
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
        let plan = deep_clone_plan_with(&ty, span, &|name| {
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

/// DeepCloneable(T) 与对应 resolved execution plan 的唯一 semantic owner。
///
/// `struct_fields` 只提供字段静态类型；它不拥有 capability 规则。final-HIR gate 通过
/// 同一函数复核已写入 plan，从而避免 checker 与 validator 各维护一份类型矩阵。
pub(in crate::sema) fn deep_clone_plan_with<F>(
    ty: &Ty,
    span: Span,
    struct_fields: &F,
) -> AliasResult<DeepClonePlan>
where
    F: Fn(&str) -> Option<Vec<Ty>>,
{
    if ty.contains_unknown() {
        return Err(AliasError {
            msg: format!("clone 无法确定 {} 的完整静态类型", ty.name()),
            span,
        });
    }
    let mut visiting = HashSet::new();
    build_deep_clone_plan(ty, span, struct_fields, &mut visiting)
}

fn build_deep_clone_plan<F>(
    ty: &Ty,
    span: Span,
    struct_fields: &F,
    visiting: &mut HashSet<String>,
) -> AliasResult<DeepClonePlan>
where
    F: Fn(&str) -> Option<Vec<Ty>>,
{
    Ok(match ty {
        Ty::Int(_) | Ty::UInt(_) | Ty::Float(_) | Ty::Bool => DeepClonePlan::Inline,
        Ty::Str => DeepClonePlan::String,
        Ty::Struct(name) => {
            if !visiting.insert(name.clone()) {
                return Err(AliasError {
                    msg: format!(
                        "内部 sema 不变式被破坏: DeepCloneable 结构体依赖出现递归 '{name}'"
                    ),
                    span,
                });
            }
            let fields = struct_fields(name).ok_or_else(|| AliasError {
                msg: format!("内部 sema 不变式被破坏: clone 引用未知结构体 '{name}'"),
                span,
            })?;
            let plans = fields
                .iter()
                .map(|field| build_deep_clone_plan(field, span, struct_fields, visiting))
                .collect::<AliasResult<Vec<_>>>()?;
            visiting.remove(name);
            DeepClonePlan::Struct {
                name: name.clone(),
                fields: plans,
            }
        }
        Ty::Array(elem) => DeepClonePlan::Array(Box::new(build_deep_clone_plan(
            elem,
            span,
            struct_fields,
            visiting,
        )?)),
        Ty::Result(ok, err) => DeepClonePlan::Result {
            ok: Box::new(build_deep_clone_plan(ok, span, struct_fields, visiting)?),
            err: Box::new(build_deep_clone_plan(err, span, struct_fields, visiting)?),
        },
        Ty::Func { .. } | Ty::FuncPoly | Ty::Iterator(_) => {
            return Err(AliasError {
                msg: format!("类型 {} 不支持 clone", ty.name()),
                span,
            });
        }
        Ty::Unit => {
            return Err(AliasError {
                msg: "unit 不是可 clone 的值类型".into(),
                span,
            });
        }
        Ty::Unknown => {
            return Err(AliasError {
                msg: "clone 无法确定参数的静态类型".into(),
                span,
            });
        }
    })
}
