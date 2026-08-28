//! typed HIR facade: model, lowering, validation, capture analysis and type traversal.

mod binding_contract;
mod capture;
mod lower;
mod model;
mod typed_contract;
mod validate;
mod visit;

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod typed_contract_tests;

pub(crate) use model::{
    ArmBody, BinOp, BindKind, Binding, BindingId, BindingOwner, Body, BuiltinCall, CallArg,
    CallTarget, CheckedProgram, CtorKind, Expr, ExprInfo, Item, LowerFacts, MatchArm, MethodId,
    MethodTarget, Param, Pattern, ResolvedConversion, Stmt, StrPart, StructDef, StructField,
};

pub(super) fn lower(
    program: crate::ast::Program,
    facts: LowerFacts,
    main_id: BindingId,
) -> crate::AliasResult<CheckedProgram> {
    lower::lower(program, facts, main_id)
}

/// codegen 前唯一 final-HIR gate。stable BindingId graph、局部 typed-node 方程和其余
/// resolved cross-reference 分责验证，但只允许经这一入口共同通过。
pub(super) fn validate_resolved_hir(program: &CheckedProgram) -> crate::AliasResult<()> {
    binding_contract::validate(program)?;
    typed_contract::validate(program)?;
    validate::validate_resolved_hir(program)
}
