//! Canonical recovery of a stable Place from already-resolved HIR expressions.
//!
//! Field indices and binding IDs are frozen before this module runs. Call argument planning and
//! ownership flow share this owner so a projected read cannot silently use a coarser root rule
//! than the loan source recorded at the same expression.

use super::{Expr, Place, PlaceInfo, ResolvedConversion};

enum Projection<'a> {
    Field(usize, PlaceInfo),
    Index(&'a Expr, PlaceInfo),
}

pub(super) fn from_expr(expr: &Expr) -> Option<Place> {
    let mut current = expr;
    let mut projections = Vec::new();
    loop {
        match current {
            Expr::Ident(_, Some(binding_id), span, info) => {
                let mut place = Place::Local {
                    binding_id: *binding_id,
                    info: PlaceInfo {
                        ty: info.ty.clone(),
                        span: *span,
                    },
                };
                for projection in projections.into_iter().rev() {
                    place = match projection {
                        Projection::Field(field_index, info) => Place::Field {
                            base: Box::new(place),
                            field_index,
                            info,
                        },
                        Projection::Index(index, info) => Place::Index {
                            base: Box::new(place),
                            index: Box::new(index.clone()),
                            info,
                        },
                    };
                }
                return Some(place);
            }
            Expr::Field {
                recv,
                field_index,
                span,
                info,
            } => {
                projections.push(Projection::Field(
                    *field_index,
                    PlaceInfo {
                        ty: info.ty.clone(),
                        span: *span,
                    },
                ));
                current = recv;
            }
            Expr::Index {
                recv,
                idx,
                span,
                info,
            } => {
                projections.push(Projection::Index(
                    idx,
                    PlaceInfo {
                        ty: info.ty.clone(),
                        span: *span,
                    },
                ));
                current = recv;
            }
            Expr::Convert {
                expr: inner,
                mode: ResolvedConversion::Identity,
                ..
            } => current = inner,
            _ => return None,
        }
    }
}

/// Exact source-shape check for a Place cloned from an HIR expression. Index expressions retain
/// the source span/type identity established by lowering; changing either side independently must
/// not let a caller pass protect a different storage projection from the value codegen evaluates.
pub(super) fn same_source(left: &Place, right: &Place) -> bool {
    let mut stack = vec![(left, right)];
    while let Some((left, right)) = stack.pop() {
        match (left, right) {
            (
                Place::Local {
                    binding_id: left_id,
                    info: left_info,
                },
                Place::Local {
                    binding_id: right_id,
                    info: right_info,
                },
            ) if left_id == right_id && left_info.ty == right_info.ty => {}
            (
                Place::Field {
                    base: left_base,
                    field_index: left_index,
                    info: left_info,
                },
                Place::Field {
                    base: right_base,
                    field_index: right_index,
                    info: right_info,
                },
            ) if left_index == right_index && left_info.ty == right_info.ty => {
                stack.push((left_base, right_base));
            }
            (
                Place::Index {
                    base: left_base,
                    index: left_index,
                    info: left_info,
                },
                Place::Index {
                    base: right_base,
                    index: right_index,
                    info: right_info,
                },
            ) if left_info.ty == right_info.ty
                && left_index.ty() == right_index.ty()
                && left_index.span() == right_index.span() =>
            {
                stack.push((left_base, right_base));
            }
            _ => return false,
        }
    }
    true
}
