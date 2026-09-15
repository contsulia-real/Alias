//! Static source contract for `reinterpret<T>` pointer views.

use super::super::{Checker, Env};
use crate::ast::{CallArg, Expr, TypeExpr};
use crate::sema::hir::{LowerPointerViewInfo, PointerViewSource};
use crate::sema::types::{check_value_type_slot, ensure_pointer_pointee, Ty};
use crate::{AliasError, AliasResult, Span};

impl Checker {
    pub(super) fn check_reinterpret(
        &mut self,
        expression: &Expr,
        target: &TypeExpr,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let [arg] = args else {
            return Err(AliasError {
                msg: "reinterpret<T> 恰好接受 1 个参数".into(),
                span,
            });
        };
        if arg.label.is_some() {
            return Err(AliasError {
                msg: "reinterpret<T> 不接受命名实参".into(),
                span: arg.span,
            });
        }
        let target = check_value_type_slot(target, span, &self.structs)?;
        ensure_pointer_pointee(&target, span)?;
        let source_ty = self.expr(&arg.value, env)?;
        if !matches!(
            source_ty,
            Ty::Ptr {
                nullable: false,
                ..
            }
        ) {
            return Err(AliasError {
                msg: format!(
                    "reinterpret<T> 需要 non-null ptr source，实际 {}",
                    source_ty.name()
                ),
                span: arg.value.span(),
            });
        }
        let source = if let Some(info) = self.pointer_views.get(&Self::expr_key(&arg.value)) {
            info.source
        } else if let Some(borrow) = self.borrow_places.get(&Self::expr_key(&arg.value)) {
            PointerViewSource::Loan(borrow.loan_id)
        } else if let Some(binding) = self
            .expr_binding_ids
            .get(&Self::expr_key(&arg.value))
            .copied()
        {
            if !self.borrowed_bindings.contains_key(&binding) {
                return Err(AliasError {
                    msg: "reinterpret<T> 当前只从 borrow-derived pointer view 派生".into(),
                    span: arg.value.span(),
                });
            }
            PointerViewSource::Binding(binding)
        } else {
            return Err(AliasError {
                msg: "reinterpret<T> source 必须是 borrow-derived pointer view".into(),
                span: arg.value.span(),
            });
        };
        self.pointer_views
            .insert(Self::expr_key(expression), LowerPointerViewInfo { source });
        Ok(Ty::Ptr {
            pointee: Box::new(target),
            nullable: false,
        })
    }
}
