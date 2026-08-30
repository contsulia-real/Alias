//! Explicit non-owning borrow resolution.
//!
//! The AST expression is resolved to a canonical Place during checking. Loan kind and NLL region
//! depend on later uses, so they are deliberately not guessed here.

use super::super::{Checker, Env};
use crate::ast::{CallArg, Expr};
use crate::sema::hir::LowerBorrowInfo;
use crate::sema::types::Ty;
use crate::{AliasError, AliasResult, Span};

impl Checker {
    pub(super) fn check_borrow_call(
        &mut self,
        call: &Expr,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let [arg] = args else {
            return Err(AliasError {
                msg: "borrow 恰好接受 1 个参数".into(),
                span,
            });
        };
        if arg.label.is_some() {
            return Err(AliasError {
                msg: "borrow 不接受命名实参".into(),
                span: arg.span,
            });
        }

        let place = self.resolve_place_expr(&arg.value, env)?;
        if self
            .borrowed_bindings
            .contains_key(&place.root_binding_id())
        {
            return Err(AliasError {
                msg: "borrow source 当前必须直接根植于 owning Place；borrowed alias 的 reborrow 尚未解析 canonical owner".into(),
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
                msg: "内部 sema 不变式被破坏: borrow Place 类型与表达式类型不一致".into(),
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
        Ok(checked_ty)
    }
}
