use super::{BindingId, Expr, Place, ResolvedConversion};
use crate::sema::types::{IntW, Ty};

/// 两个 resolved Place 的静态关系。`Unknown` 与 `Overlap` 一样必须被 ownership / borrow
/// conflict 当作冲突；只有 `Disjoint` 才能授权 move replacement 或并存的独占 loan。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaceRelation {
    Disjoint,
    Overlap,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Projection {
    Field(usize),
    Index(Option<i32>),
}

fn constant_i32(expr: &Expr) -> Option<i32> {
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

fn decompose(place: &Place) -> (BindingId, Vec<Projection>) {
    let mut current = place;
    let mut path = Vec::new();
    let root = loop {
        match current {
            Place::Local { binding_id, .. } => break *binding_id,
            Place::Field {
                base, field_index, ..
            } => {
                path.push(Projection::Field(*field_index));
                current = base;
            }
            Place::Index { base, index, .. } => {
                path.push(Projection::Index(constant_i32(index)));
                current = base;
            }
        }
    };
    path.reverse();
    (root, path)
}

/// Canonical Place overlap owner。
///
/// 规则完全基于 resolved semantic identity：不同 Local root 可证明 disjoint；不同字段或
/// 不同常量下标可证明 disjoint；ancestor/equal path overlap；动态下标在没有其它 projection
/// 能证明 disjoint 时保持 Unknown。这里不读取机器地址，也不做 runtime alias guessing。
pub(crate) fn relation(left: &Place, right: &Place) -> PlaceRelation {
    let (left_root, left_path) = decompose(left);
    let (right_root, right_path) = decompose(right);
    if left_root != right_root {
        return PlaceRelation::Disjoint;
    }

    let mut uncertain = false;
    for (left, right) in left_path.iter().zip(&right_path) {
        match (*left, *right) {
            (Projection::Field(a), Projection::Field(b)) => {
                if a != b {
                    return PlaceRelation::Disjoint;
                }
            }
            (Projection::Index(Some(a)), Projection::Index(Some(b))) => {
                if a != b {
                    return PlaceRelation::Disjoint;
                }
            }
            (Projection::Index(_), Projection::Index(_)) => uncertain = true,
            // 对已通过 typed-HIR gate 的 Place，同一 base 不会同时既是 struct 又是 array。
            // relation owner 仍 fail-closed，避免被损坏 HIR 诱导出假的 Disjoint。
            (Projection::Field(_), Projection::Index(_))
            | (Projection::Index(_), Projection::Field(_)) => return PlaceRelation::Unknown,
        }
    }

    if uncertain {
        PlaceRelation::Unknown
    } else {
        // 完全相同，或一方是另一方 ancestor，均实际覆盖同一 storage region。
        PlaceRelation::Overlap
    }
}
