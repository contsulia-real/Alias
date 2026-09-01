//! Cranelift SSA values for one resolved Alias expression.
//!
//! `ValueAbi` owns the lane types; this type carries the corresponding emitted values. Keeping
//! the carrier explicit prevents scalar-only emitters from silently accepting the first lane of
//! a pointer capability and discarding provenance or bounds.

use crate::codegen::invariant_violation;
use cranelift_codegen::ir::{BlockArg, Type, Value};
use cranelift_frontend::FunctionBuilder;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExprValue {
    lanes: Vec<Value>,
}

impl ExprValue {
    pub(crate) fn scalar(value: Value) -> Self {
        Self { lanes: vec![value] }
    }

    pub(crate) fn from_lanes(lanes: Vec<Value>) -> Self {
        if lanes.is_empty() {
            invariant_violation("expression value 至少包含一个 SSA lane")
        }
        Self { lanes }
    }

    pub(crate) fn block_args(&self) -> Vec<BlockArg> {
        self.lanes
            .iter()
            .copied()
            .map(BlockArg::Value)
            .collect()
    }

    pub(crate) fn assert_types(
        &self,
        bcx: &FunctionBuilder,
        expected: &[Type],
        context: &'static str,
    ) {
        if self.lanes.len() != expected.len()
            || self
                .lanes
                .iter()
                .zip(expected)
                .any(|(value, expected)| bcx.func.dfg.value_type(*value) != *expected)
        {
            invariant_violation(context)
        }
    }

    pub(crate) fn into_scalar(self, context: &'static str) -> Value {
        match self.lanes.as_slice() {
            [value] => *value,
            _ => invariant_violation(context),
        }
    }
}
