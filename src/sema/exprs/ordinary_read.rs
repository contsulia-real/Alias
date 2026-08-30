//! Owning-slot ordinary-read resolution.
//!
//! A stable Place entering a known owning storage context is not a reference bit-copy. This owner
//! resolves the exact Place and freezes the recursive DeepClone plan during checking; lowering and
//! codegen must consume those facts without reconstructing intent from expression shape or Ty.

use super::super::{Checker, Env, LowerCallTarget};
use super::deep_clone::deep_clone_plan_with;
use crate::ast::Expr;
use crate::sema::hir::{LowerOwningReadInfo, ResolvedConversion};
use crate::sema::types::{types_match, Ty};
use crate::{AliasError, AliasResult};

impl Checker {
    fn owning_read_source<'a>(&self, expr: &'a Expr) -> Option<&'a Expr> {
        let mut current = expr;
        // Identity conversion is semantically transparent. If it hid the inner stable Place from
        // this owner, an owning target could retain a shared heap pointer merely by spelling
        // `try_from place`, while the same Place without the wrapper would be cloned.
        loop {
            match current {
                Expr::Call { args, .. }
                    if matches!(
                        self.expr_facts
                            .get(&Self::expr_key(current))
                            .and_then(|fact| fact.call_target.as_ref()),
                        Some(LowerCallTarget::ContextualConversion(
                            ResolvedConversion::Identity
                        ))
                    ) =>
                {
                    let [arg] = args.as_slice() else {
                        return None;
                    };
                    current = &arg.value;
                }
                _ => break,
            }
        }
        let source = current;
        loop {
            match current {
                Expr::Ident(..) => return Some(source),
                Expr::Field { recv, .. } | Expr::Index { recv, .. } => current = recv,
                _ => return None,
            }
        }
    }

    pub(in crate::sema) fn record_owning_slot_read(
        &mut self,
        expr: &Expr,
        env: &Env,
        expected: &Ty,
    ) -> AliasResult<()> {
        let Some(source) = self.owning_read_source(expr) else {
            return Ok(());
        };
        let key = Self::expr_key(source);
        let Some(expr_fact) = self.expr_facts.get(&key) else {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: owning-slot read 缺少表达式类型 fact".into(),
                span: source.span(),
            });
        };
        // A direct zero-parameter function name is already resolved as a call result, not a Place
        // read. Treating its spelling as a Place would silently clone the function binding instead.
        if expr_fact.implicit_zero_callee.is_some() {
            return Ok(());
        }
        if !types_match(expected, &expr_fact.ty) {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: owning-slot read 类型与目标槽不一致".into(),
                span: source.span(),
            });
        }
        let place = self.resolve_place_expr(source, env)?;
        if !types_match(place.ty(), expected) {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: owning-slot read Place 类型漂移".into(),
                span: source.span(),
            });
        }
        let plan = deep_clone_plan_with(expected, source.span(), &|name| {
            self.structs.get(name).map(|info| {
                info.fields
                    .iter()
                    .map(|field| field.ty.clone())
                    .collect::<Vec<_>>()
            })
        })?;
        self.owning_reads
            .insert(key, LowerOwningReadInfo { place, plan });
        Ok(())
    }
}
