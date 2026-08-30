//! typed HIR facade：model、lowering、validation、capture analysis 与 type traversal。

mod binding_contract;
mod capture;
mod lower;
mod model;
mod ownership_capabilities;
mod ownership_flow;
mod place_relation;
mod storage_relations;
mod typed_contract;
mod validate;
mod value_categories;
mod visit;

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod deep_clone_tests;
#[cfg(test)]
mod move_tests;
#[cfg(test)]
mod ordinary_read_tests;
#[cfg(test)]
mod place_relation_tests;
#[cfg(test)]
mod shallow_clone_tests;
#[cfg(test)]
mod storage_relation_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod typed_contract_tests;
#[cfg(test)]
mod value_category_tests;

pub(crate) use model::{
    ArmBody, BinOp, BindKind, Binding, BindingId, BindingOwner, Body, BuiltinCall, CallArg,
    CallTarget, CheckedProgram, CtorKind, DeepClonePlan, Expr, ExprCategory, ExprInfo, Item,
    MatchArm, MethodId, MethodTarget, OwnershipCapability, Param, Pattern, Place, PlaceInfo,
    ResolvedConversion, ShallowClonePlan, Stmt, StorageRelation, StrPart, StructDef, StructField,
    ValueCategory,
};
pub(crate) use place_relation::{relation as place_relation, PlaceRelation};

use crate::sema::types::Ty;
use std::collections::HashMap;

/// check 阶段解析出的结构化 Place。该类型只跨越 check → lower；最终 lowering 必须把
/// Local/Field/Index 的递归 projection 原样固化到 model::Place，不能重新按 AST 名字猜测。
#[derive(Debug, Clone, PartialEq)]
pub(super) enum LowerPlaceInfo {
    Local {
        binding_id: BindingId,
        ty: Ty,
    },
    Field {
        base: Box<LowerPlaceInfo>,
        field_index: usize,
        ty: Ty,
    },
    Index {
        base: Box<LowerPlaceInfo>,
        ty: Ty,
    },
}

impl LowerPlaceInfo {
    pub(super) fn ty(&self) -> &Ty {
        match self {
            Self::Local { ty, .. } | Self::Field { ty, .. } | Self::Index { ty, .. } => ty,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct LowerOwningReadInfo {
    pub(super) place: LowerPlaceInfo,
    pub(super) plan: DeepClonePlan,
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
    pub(super) move_places: HashMap<usize, LowerPlaceInfo>,
    pub(super) owning_reads: HashMap<usize, LowerOwningReadInfo>,
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

/// codegen 前唯一 final-HIR gate。value category、initial ownership capability、binding
/// storage relation、Place relation、stable BindingId graph、局部 typed-node 方程和其余
/// resolved cross-reference 分责验证，但只允许经这一入口共同通过。
pub(super) fn validate_resolved_hir(program: &CheckedProgram) -> crate::AliasResult<()> {
    value_categories::validate(program)?;
    ownership_capabilities::validate(program)?;
    storage_relations::validate(program)?;
    place_relation::validate(program)?;
    ownership_flow::validate(program)?;
    binding_contract::validate(program)?;
    typed_contract::validate(program)?;
    validate::validate_resolved_hir(program)
}
