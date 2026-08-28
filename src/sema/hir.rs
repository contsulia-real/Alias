//! typed HIR facade: model, lowering, validation, capture analysis and type traversal.

mod capture;
mod lower;
mod model;
mod validate;
mod visit;

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod tests;

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
