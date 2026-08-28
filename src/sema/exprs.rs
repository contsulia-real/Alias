//! sema::exprs — 表达式静态语义 facade。

use super::hir::{BindingId, BuiltinCall, CallTarget, ExprInfo, MethodTarget};
use super::types::{
    check_value_type_slot, default_negative_int_ty, default_positive_int_ty, int_literal_fits,
    types_match, FloatW, IntW, Ty,
};
use super::{op_mismatch, Checker, Env, Scope, VarInfo};
use crate::ast::{ArmBody, BinOp, CallArg, CtorKind, Expr, MatchArm, Pattern, Stmt, StrPartAst};
use crate::{AliasError, AliasResult, Span};
use std::collections::HashSet;

mod calls;
mod infer;
mod match_expr;
mod operators;
mod typing;

use calls::resolve_method_target;
use operators::{binary_flows_expected, contextual_conversion, conversion_exists, literal_slot_unify};
pub(super) use operators::require_value;
pub(super) use typing::{ExprCheckError, ExprCheckResult};
