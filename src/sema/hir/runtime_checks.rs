//! Canonical sema decisions for safety conditions that may require runtime checks.
//!
//! The HIR stores these decisions so codegen only emits or elides a check; it never infers proof
//! from expression shape. Pointer provenance, bounds, alignment, and raw-initialization operations
//! extend this owner as those semantic nodes land.

use super::{BinOp, Expr, Place, ResolvedConversion, RuntimeCheckRequirement};
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

pub(super) fn pointer_binary(
    op: BinOp,
    left: &Expr,
    right: &Expr,
) -> AliasResult<(
    Option<RuntimeCheckRequirement>,
    Option<RuntimeCheckRequirement>,
    Option<RuntimeCheckRequirement>,
)> {
    let left_ty = left.ty();
    let right_ty = right.ty();
    if !matches!(
        left_ty,
        Ty::Ptr {
            nullable: false,
            ..
        }
    ) {
        return Ok((None, None, None));
    }
    if matches!(op, BinOp::Add | BinOp::Sub)
        && matches!(right_ty, Ty::Int(_) | Ty::UInt(_))
    {
        let delta = constant_integer(right);
        if let (Some(current), Some(delta)) = (known_refer_offset(left), delta) {
            let next = if op == BinOp::Add {
                current.checked_add(delta)
            } else {
                current.checked_sub(delta)
            };
            if !matches!(next, Some(0 | 1)) {
                return Err(AliasError {
                    msg: "pointer arithmetic 的静态 offset 超出 source view".into(),
                    span: right.span(),
                });
            }
            return Ok((None, None, Some(RuntimeCheckRequirement::Proven)));
        }
        return Ok((
            None,
            None,
            Some(if constant_integer_is_zero(right) {
                RuntimeCheckRequirement::Proven
            } else {
                RuntimeCheckRequirement::Required
            }),
        ));
    }
    if left_ty != right_ty {
        return Ok((None, None, None));
    }
    // Static pointer values currently carry no symbolic provenance identity. Preserve that
    // uncertainty explicitly instead of letting codegen infer a proof from expression shape.
    Ok(match op {
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            (Some(RuntimeCheckRequirement::Required), None, None)
        }
        BinOp::Sub => (
            Some(RuntimeCheckRequirement::Required),
            Some(RuntimeCheckRequirement::Required),
            None,
        ),
        _ => (None, None, None),
    })
}

fn constant_integer_is_zero(expr: &Expr) -> bool {
    match expr {
        Expr::Int(0, ..) => true,
        Expr::Convert { expr, mode: ResolvedConversion::Identity, .. } => {
            constant_integer_is_zero(expr)
        }
        _ => false,
    }
}

fn constant_integer(expr: &Expr) -> Option<i128> {
    match expr {
        Expr::Int(value, ..) => Some(*value as i128),
        Expr::Neg { expr, .. } => constant_integer(expr)?.checked_neg(),
        Expr::Convert { expr, mode: ResolvedConversion::Identity, .. } => constant_integer(expr),
        _ => None,
    }
}

fn known_refer_offset(expr: &Expr) -> Option<i128> {
    match expr {
        Expr::Refer { .. } => Some(0),
        Expr::Binary { op, lhs, rhs, pointer_offset_source: Some(_), .. } => {
            let left = known_refer_offset(lhs)?;
            let right = constant_integer(rhs)?;
            if *op == BinOp::Add {
                left.checked_add(right)
            } else {
                left.checked_sub(right)
            }
        }
        Expr::Convert { expr, mode: ResolvedConversion::Identity, .. } => known_refer_offset(expr),
        _ => None,
    }
}
