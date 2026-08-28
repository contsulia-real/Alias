use super::*;

impl Checker {
    pub(super) fn binary(&mut self, op: BinOp, l: Ty, r: Ty, span: Span) -> AliasResult<Ty> {
        use BinOp::*;
        if l.is_unknown() || r.is_unknown() {
            return Ok(Ty::Unknown);
        }
        if matches!(op, And | Or) {
            return if l == Ty::Bool && r == Ty::Bool {
                Ok(Ty::Bool)
            } else {
                Err(op_mismatch(op, &l, &r, span))
            };
        }
        let mixed = |span| {
            if l.is_numeric() && r.is_numeric() && l != r {
                AliasError {
                    msg: format!("{} 与 {} 禁止隐式混算", l.name(), r.name()),
                    span,
                }
            } else {
                op_mismatch(op, &l, &r, span)
            }
        };
        match op {
            Add | Sub | Mul | Div => match (&l, &r) {
                (Ty::Int(a), Ty::Int(b)) if a == b => Ok(Ty::Int(*a)),
                (Ty::UInt(a), Ty::UInt(b)) if a == b => Ok(Ty::UInt(*a)),
                (Ty::Float(a), Ty::Float(b)) if a == b => Ok(Ty::Float(*a)),
                _ => Err(mixed(span)),
            },
            Rem => match (&l, &r) {
                (Ty::Int(a), Ty::Int(b)) if a == b => Ok(Ty::Int(*a)),
                (Ty::UInt(a), Ty::UInt(b)) if a == b => Ok(Ty::UInt(*a)),
                _ => Err(mixed(span)),
            },
            Shl | Shr | BitAnd | BitXor | BitOr => match (&l, &r) {
                (Ty::Int(a), Ty::Int(b)) if a == b => Ok(Ty::Int(*a)),
                (Ty::UInt(a), Ty::UInt(b)) if a == b => Ok(Ty::UInt(*a)),
                _ => Err(mixed(span)),
            },
            Lt | Le | Gt | Ge | EqEq | NotEq => match (&l, &r) {
                (Ty::Int(a), Ty::Int(b)) if a == b => Ok(Ty::Bool),
                (Ty::UInt(a), Ty::UInt(b)) if a == b => Ok(Ty::Bool),
                (Ty::Float(a), Ty::Float(b)) if a == b => Ok(Ty::Bool),
                (Ty::Str, Ty::Str) => Ok(Ty::Bool),
                (Ty::Bool, Ty::Bool) if matches!(op, EqEq | NotEq) => Ok(Ty::Bool),
                _ => Err(mixed(span)),
            },
            And | Or => unreachable!(),
        }
    }
}

pub(super) fn binary_flows_expected(op: BinOp, expected: &Ty) -> bool {
    use BinOp::*;
    match op {
        Add | Sub | Mul | Div => expected.is_numeric(),
        Rem | Shl | Shr | BitAnd | BitXor | BitOr => {
            matches!(expected, Ty::Int(_) | Ty::UInt(_))
        }
        Lt | Le | Gt | Ge | EqEq | NotEq | And | Or => false,
    }
}

pub(super) fn literal_slot_unify(declared: &Ty, value: &Expr) -> Option<ExprCheckResult<Ty>> {
    let span = value.span();
    if let Expr::Float(..) = value {
        return if matches!(declared, Ty::Float(_)) {
            Some(Ok(declared.clone()))
        } else {
            None
        };
    }
    let (magnitude, negative) = match value {
        Expr::Int(n, _) => (*n, false),
        Expr::Neg { expr, .. } => match expr.as_ref() {
            Expr::Int(n, _) => (*n, true),
            _ => return None,
        },
        _ => return None,
    };
    if !matches!(declared, Ty::Int(_) | Ty::UInt(_)) {
        return None;
    }
    Some(if int_literal_fits(declared, magnitude, negative) {
        Ok(declared.clone())
    } else {
        let literal = if negative {
            format!("-{magnitude}")
        } else {
            magnitude.to_string()
        };
        Err(ExprCheckError::LiteralOutOfRange {
            literal,
            expected: declared.clone(),
            span,
        })
    })
}

pub(super) fn conversion_exists(source: &Ty, target: &Ty) -> bool {
    (source.is_numeric() && target.is_numeric())
        || (matches!(target, Ty::Str) && !source.is_unknown() && *source != Ty::Unit)
}

pub(in crate::sema) fn require_value(ty: Ty, span: Span) -> AliasResult<Ty> {
    if ty == Ty::Unit {
        Err(AliasError {
            msg: "无返回值表达式不能用于值位置".into(),
            span,
        })
    } else {
        Ok(ty)
    }
}

pub(super) fn contextual_conversion(e: &Expr) -> Option<(&str, &Expr, Span)> {
    let Expr::Call { callee, args, span } = e else {
        return None;
    };
    let Expr::Ident(name, _) = callee.as_ref() else {
        return None;
    };
    if name != "from" && name != "try_from" {
        return None;
    }
    let [arg] = args.as_slice() else {
        return None;
    };
    Some((name, &arg.value, *span))
}
