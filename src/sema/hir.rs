//! typed HIR facade：model、lowering、validation、capture analysis 与 type traversal。

mod binding_contract;
mod capture;
mod lower;
mod model;
mod typed_contract;
mod validate;
mod value_categories;
mod visit;

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod typed_contract_tests;
#[cfg(test)]
mod value_category_tests;

pub(crate) use model::{
    ArmBody, BinOp, BindKind, Binding, BindingId, BindingOwner, Body, BuiltinCall, CallArg,
    CallTarget, CheckedProgram, CtorKind, Expr, ExprCategory, ExprInfo, Item, MatchArm, MethodId,
    MethodTarget, Param, Pattern, Place, PlaceInfo, ResolvedConversion, Stmt, StrPart, StructDef,
    StructField, ValueCategory,
};

use crate::sema::types::Ty;
use std::collections::HashMap;

/// check 阶段对一个赋值语句解析出的 Place 身份与最终 target 类型。该类型只跨越
/// check → lower，不进入最终 HIR；lowering 会把它固化成 model::Place。
#[derive(Debug, Clone, PartialEq)]
pub(super) enum LowerPlaceInfo {
    Local { binding_id: BindingId, ty: Ty },
    Field { field_index: usize, ty: Ty },
}

/// sema check → HIR lowering 的短生命周期边界合同。所有字段必须在 lowering 完成时
/// 被精确消费；它不是 final HIR model，也不能越过 lower 存活到 capture/validation/codegen。
pub(super) struct LowerFacts {
    pub(super) exprs: HashMap<usize, crate::sema::LowerExprInfo>,
    pub(super) bindings: HashMap<usize, Ty>,
    pub(super) binding_ids: HashMap<usize, BindingId>,
    pub(super) receivers: HashMap<usize, Ty>,
    pub(super) method_ids: HashMap<usize, MethodId>,
    pub(super) method_self_ids: HashMap<usize, BindingId>,
    pub(super) fields: HashMap<usize, Ty>,
    pub(super) field_indices: HashMap<usize, usize>,
    pub(super) assignment_places: HashMap<usize, LowerPlaceInfo>,
    pub(super) ctor_arg_indices: HashMap<usize, usize>,
    pub(super) params: HashMap<usize, Ty>,
    pub(super) param_ids: HashMap<usize, BindingId>,
    pub(super) fors: HashMap<usize, Ty>,
    pub(super) for_ids: HashMap<usize, BindingId>,
    pub(super) match_binding_ids: HashMap<usize, BindingId>,
    pub(super) expr_binding_ids: HashMap<usize, BindingId>,
}

pub(super) fn lower(
    program: crate::ast::Program,
    facts: LowerFacts,
    main_id: BindingId,
) -> crate::AliasResult<CheckedProgram> {
    lower::lower(program, facts, main_id)
}

/// codegen 前唯一 final-HIR gate。value category、stable BindingId graph、局部 typed-node
/// 方程和其余 resolved cross-reference 分责验证，但只允许经这一入口共同通过。
pub(super) fn validate_resolved_hir(program: &CheckedProgram) -> crate::AliasResult<()> {
    value_categories::validate(program)?;
    binding_contract::validate(program)?;
    typed_contract::validate(program)?;
    validate::validate_resolved_hir(program)
}
