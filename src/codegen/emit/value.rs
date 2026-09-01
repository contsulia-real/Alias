//! Cranelift SSA values for one resolved Alias expression.
//!
//! `ValueAbi` owns the lane types; this type carries the corresponding emitted values. Keeping
//! the carrier explicit prevents scalar-only emitters from silently accepting the first lane of
//! a pointer capability and discarding provenance or bounds.

use crate::codegen::abi::{norm_load, norm_store, VTy, ValueAbi};
use crate::codegen::invariant_violation;
use cranelift_codegen::ir::{BlockArg, InstBuilder, MemFlagsData, Type, Value};
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
        self.lanes.iter().copied().map(BlockArg::Value).collect()
    }

    pub(crate) fn load(
        bcx: &mut FunctionBuilder,
        base: Value,
        base_offset: i32,
        vty: &VTy,
    ) -> Self {
        let abi = vty.abi();
        let storage = abi.storage_lanes();
        if abi.expression_types().len() == 1 && storage.len() == 1 {
            let lane = storage[0];
            let raw = bcx.ins().load(
                lane.ty(),
                MemFlagsData::new(),
                base,
                storage_offset(base_offset, lane.offset()),
            );
            return Self::scalar(norm_load(bcx, raw, vty));
        }
        Self::load_aggregate(bcx, base, base_offset, &abi)
    }

    fn load_aggregate(
        bcx: &mut FunctionBuilder,
        base: Value,
        base_offset: i32,
        abi: &ValueAbi,
    ) -> Self {
        let storage = abi.storage_lanes();
        validate_aggregate_contract(abi);
        // Every aggregate lane must cross the storage boundary independently. Loading only the
        // address lane of a pointer capability would discard provenance and bounds while still
        // producing superficially valid machine IR.
        Self::from_lanes(
            storage
                .iter()
                .map(|lane| {
                    bcx.ins().load(
                        lane.ty(),
                        MemFlagsData::new(),
                        base,
                        storage_offset(base_offset, lane.offset()),
                    )
                })
                .collect(),
        )
    }

    pub(crate) fn store(
        &self,
        bcx: &mut FunctionBuilder,
        base: Value,
        base_offset: i32,
        vty: &VTy,
    ) {
        let abi = vty.abi();
        self.assert_types(
            bcx,
            abi.expression_types(),
            "expression value 与 canonical ABI lane 类型不一致",
        );
        let storage = abi.storage_lanes();
        if self.lanes.len() == 1 && storage.len() == 1 {
            let lane = storage[0];
            let stored = norm_store(bcx, self.lanes[0], vty);
            bcx.ins().store(
                MemFlagsData::new(),
                stored,
                base,
                storage_offset(base_offset, lane.offset()),
            );
            return;
        }
        self.store_aggregate(bcx, base, base_offset, &abi);
    }

    fn store_aggregate(
        &self,
        bcx: &mut FunctionBuilder,
        base: Value,
        base_offset: i32,
        abi: &ValueAbi,
    ) {
        let storage = abi.storage_lanes();
        validate_aggregate_contract(abi);
        if self.lanes.len() != storage.len()
            || self
                .lanes
                .iter()
                .zip(storage)
                .any(|(value, lane)| bcx.func.dfg.value_type(*value) != lane.ty())
        {
            invariant_violation("aggregate expression/storage lane contract 不一致")
        }
        for (value, lane) in self.lanes.iter().zip(storage) {
            bcx.ins().store(
                MemFlagsData::new(),
                *value,
                base,
                storage_offset(base_offset, lane.offset()),
            );
        }
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

fn validate_aggregate_contract(abi: &ValueAbi) {
    if abi.expression_types().len() != abi.storage_lanes().len()
        || abi
            .expression_types()
            .iter()
            .zip(abi.storage_lanes())
            .any(|(expression, lane)| *expression != lane.ty())
    {
        invariant_violation("aggregate expression/storage lane contract 不一致")
    }
}

fn storage_offset(base: i32, lane: i32) -> i32 {
    base.checked_add(lane)
        .unwrap_or_else(|| invariant_violation("storage lane offset 溢出 i32"))
}

#[cfg(test)]
mod tests {
    use super::ExprValue;
    use crate::codegen::abi::PtrLayout;
    use crate::target::TARGET_TRIPLE;
    use cranelift_codegen::ir::{
        types, Function, InstBuilder, Signature, StackSlotData, StackSlotKind, UserFuncName,
    };
    use cranelift_codegen::{settings, Context};
    use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
    use std::str::FromStr;

    #[test]
    fn pointer_storage_round_trip_emits_all_four_capability_lanes() {
        let triple = target_lexicon::Triple::from_str(TARGET_TRIPLE).unwrap();
        let isa = cranelift_codegen::isa::lookup(triple)
            .unwrap()
            .finish(settings::Flags::new(settings::builder()))
            .unwrap();
        let abi = PtrLayout::for_current_target(isa.pointer_type())
            .unwrap()
            .value_abi();
        let mut ctx = Context::new();
        ctx.func = Function::with_name_signature(
            UserFuncName::user(0xfd, 0),
            Signature::new(isa.default_call_conv()),
        );
        let mut fbctx = FunctionBuilderContext::new();
        let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        let entry = bcx.create_block();
        bcx.switch_to_block(entry);
        bcx.seal_block(entry);
        let slot =
            bcx.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 32, 3));
        let base = bcx.ins().stack_addr(types::I64, slot, 0);
        let source = ExprValue::from_lanes(
            [11, 22, 33, 44]
                .into_iter()
                .map(|value| bcx.ins().iconst(types::I64, value))
                .collect(),
        );
        source.store_aggregate(&mut bcx, base, 0, &abi);
        let loaded = ExprValue::load_aggregate(&mut bcx, base, 0, &abi);
        loaded.assert_types(
            &bcx,
            &[types::I64; 4],
            "pointer storage round-trip 必须保留四个 I64 lane",
        );
        bcx.ins().return_(&[]);
        bcx.finalize(isa.frontend_config());
        ctx.verify(isa.as_ref()).unwrap();
    }
}
