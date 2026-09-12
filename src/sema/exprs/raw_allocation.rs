//! Source contracts for raw allocation ownership producers and consumers.
//!
//! `malloc<T>` is intrinsic generic syntax, not a user generic function. `free` accepts an owned
//! pointer value; the resolved-HIR ownership gate proves that the value really carries an
//! independently consumable allocation-root capability.

use super::super::{Checker, Env};
use crate::ast::{CallArg, TypeExpr};
use crate::sema::types::{check_value_type_slot, Ty};
use crate::{AliasError, AliasResult, Span};

impl Checker {
    pub(super) fn check_raw_allocate(
        &mut self,
        element_ty: &TypeExpr,
        args: &[CallArg],
        span: Span,
    ) -> AliasResult<Ty> {
        if !args.is_empty() {
            return Err(AliasError {
                msg: "malloc<T>(count) 的源码整数类型合同尚未冻结；当前只开放 malloc<T>()".into(),
                span,
            });
        }
        let pointee = check_value_type_slot(element_ty, span, &self.structs)?;
        Ok(Ty::Ptr {
            pointee: Box::new(pointee),
            nullable: true,
        })
    }

    pub(super) fn check_raw_free(
        &mut self,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let [arg] = args else {
            return Err(AliasError {
                msg: "free 恰好接受 1 个参数".into(),
                span,
            });
        };
        if arg.label.is_some() {
            return Err(AliasError {
                msg: "free 不接受命名实参".into(),
                span: arg.span,
            });
        }
        let pointer_ty = self.expr(&arg.value, env)?;
        if !matches!(pointer_ty, Ty::Ptr { .. }) {
            return Err(AliasError {
                msg: format!("free 需要 allocation-root ptr，实际 {}", pointer_ty.name()),
                span: arg.value.span(),
            });
        }
        Ok(Ty::Unit)
    }
}
