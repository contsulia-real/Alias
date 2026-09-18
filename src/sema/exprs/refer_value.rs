//! Resolution of a Place into a non-owning pointer view.
//!
//! The source Place and loan identity are frozen during checking. Descriptor identity is a
//! runtime value and therefore remains a codegen concern, but codegen never recovers the source
//! from syntax.

use super::super::{Checker, Env};
use crate::ast::{CallArg, Expr};
use crate::sema::hir::{LowerBorrowInfo, LowerPlaceInfo};
use crate::sema::types::Ty;
use crate::{AliasError, AliasResult, Span};

impl Checker {
    pub(super) fn check_refer_call(
        &mut self,
        call: &Expr,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let [arg] = args else {
            return Err(AliasError {
                msg: "refer 恰好接受 1 个参数".into(),
                span,
            });
        };
        if arg.label.is_some() {
            return Err(AliasError {
                msg: "refer 不接受命名实参".into(),
                span: arg.span,
            });
        }

        let place = self.resolve_place_expr(&arg.value, env)?;
        if matches!(
            &place,
            LowerPlaceInfo::Field { base, .. } | LowerPlaceInfo::Index { base, .. }
                if !matches!(base.as_ref(), LowerPlaceInfo::Local { .. })
        ) {
            return Err(AliasError {
                msg: "refer nested subplace 尚缺独立 heap object descriptor".into(),
                span: arg.value.span(),
            });
        }
        if self
            .borrowed_bindings
            .contains_key(&place.root_binding_id())
        {
            return Err(AliasError {
                msg: "refer source 必须直接根植于 owning Place".into(),
                span: arg.value.span(),
            });
        }
        let source_writable = self.place_terminal_is_writable(&arg.value, env)?;
        let checked_ty = match &arg.value {
            Expr::Ident(..) | Expr::This(..) => self.expr_raw_callable(&arg.value, env)?,
            _ => self.expr(&arg.value, env)?,
        };
        if checked_ty != *place.ty() {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: refer Place 类型与表达式类型不一致".into(),
                span: arg.value.span(),
            });
        }
        if matches!(checked_ty, Ty::Unit | Ty::Unknown) {
            return Err(AliasError {
                msg: format!("refer 需要完整可寻址类型，实际 {}", checked_ty.name()),
                span: arg.value.span(),
            });
        }
        let loan_id = self.fresh_loan_id()?;
        self.borrow_places.insert(
            Self::expr_key(call),
            LowerBorrowInfo {
                loan_id,
                place,
                source_writable,
            },
        );
        Ok(Ty::Ptr {
            pointee: Box::new(checked_ty),
            nullable: false,
        })
    }
}
