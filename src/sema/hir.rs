//! typed HIR facade: model, lowering, validation, capture analysis and type traversal.
#![allow(dead_code)]

mod capture;
mod lower;
mod model;
mod validate;
mod visit;

#[cfg(test)]
mod tests;

pub(crate) use model::*;

pub(super) fn lower(
    program: crate::ast::Program,
    facts: LowerFacts,
) -> crate::AliasResult<CheckedProgram> {
    lower::lower(program, facts)
}
