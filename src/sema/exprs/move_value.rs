//! Explicit ownership-transfer resolution.
//!
//! `move(place)` consumes an existing owning Place. The checker records the exact resolved Place;
//! lowering must not recover storage identity from the source expression's spelling.

use super::super::{Checker, Env};
use crate::ast::{CallArg, Expr};
use crate::sema::hir::LowerPlaceInfo;
use crate::sema::types::Ty;
use crate::{AliasError, AliasResult, Span};

impl Checker {
    pub(super) fn check_move_call(
        &mut self,
        call: &Expr,
        args: &[CallArg],
        span: Span,
        env: &Env,
    ) -> AliasResult<Ty> {
        let [arg] = args else {
            return Err(AliasError {
                msg: "move 恰好接受 1 个参数".into(),
                span,
            });
        };
        if arg.label.is_some() {
            return Err(AliasError {
                msg: "move 不接受命名实参".into(),
                span: arg.span,
            });
        }

        let place = self.resolve_place_expr(&arg.value, env)?;
        if !matches!(place, LowerPlaceInfo::Local { .. }) {
            return Err(AliasError {
                msg: "普通 struct 字段和 array 元素不能被 move-out 后留下 hole".into(),
                span: arg.value.span(),
            });
        }

        // Direct function bindings are values here, not zero-argument calls. This mirrors clone's
        // callable handling while preserving the resolved Place as the operation payload.
        let checked_ty = match &arg.value {
            Expr::Ident(..) | Expr::This(..) => self.expr_raw_callable(&arg.value, env)?,
            _ => self.expr(&arg.value, env)?,
        };
        if checked_ty != *place.ty() {
            return Err(AliasError {
                msg: "内部 sema 不变式被破坏: move Place 类型与表达式类型不一致".into(),
                span: arg.value.span(),
            });
        }
        self.move_places.insert(Self::expr_key(call), place);
        Ok(checked_ty)
    }
}
