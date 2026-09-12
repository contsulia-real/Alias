//! Canonical sema decisions for safety conditions that may require runtime checks.
//!
//! The HIR stores these decisions so codegen only emits or elides a check; it never infers proof
//! from expression shape. Pointer provenance, bounds, alignment, and raw-initialization operations
//! extend this owner as those semantic nodes land.

use super::{Expr, Place, ResolvedConversion, RuntimeCheckRequirement};
use crate::sema::types::{IntW, Ty};
use crate::{AliasError, AliasResult, Span};

pub(super) fn constant_i32(expr: &Expr) -> Option<i32> {
    if expr.ty() != &Ty::Int(IntW::W32) {
        return None;
    }
    match expr {
        Expr::Int(value, ..) => i32::try_from(*value).ok(),
        Expr::Neg { expr, .. } => match expr.as_ref() {
            Expr::Int(value, ..) if *value == i32::MAX as u64 + 1 => Some(i32::MIN),
            Expr::Int(value, ..) => i32::try_from(*value).ok().map(|value| -value),
            _ => None,
        },
        Expr::Convert {
            expr,
            mode: ResolvedConversion::Identity,
            ..
        } => constant_i32(expr),
        _ => None,
    }
}

pub(super) fn array_index(
    receiver: &Expr,
    index: &Expr,
    span: Span,
) -> AliasResult<RuntimeCheckRequirement> {
    let Expr::ArrayLit { elems, .. } = receiver else {
        return Ok(RuntimeCheckRequirement::Required);
    };
    let Some(index) = constant_i32(index) else {
        return Ok(RuntimeCheckRequirement::Required);
    };
    if index < 0 || index as usize >= elems.len() {
        return Err(AliasError {
            msg: "数组字面量的常量下标越界".into(),
            span,
        });
    }
    Ok(RuntimeCheckRequirement::Proven)
}

pub(super) fn array_place_index(_base: &Place, _index: &Expr) -> RuntimeCheckRequirement {
    // A stable array Place can be resized by aliases between evaluations. Without a frozen length
    // fact in HIR, its current runtime header is the only authority and the guard cannot be elided.
    RuntimeCheckRequirement::Required
}
