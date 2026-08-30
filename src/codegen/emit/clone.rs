use super::arrays::{array_element_addr, array_len, array_raw, wrap_array};
use super::expr::emit_expr;
use super::places::emit_place_value;
use crate::codegen::abi::{norm_load, norm_store, restore_word, storage_word, VTy};
use crate::codegen::layout::{
    RESULT_OK_TAG, RESULT_PAYLOAD_OFFSET, RESULT_TAG_OFFSET, RESULT_WORDS, STRING_BYTES,
    STRING_DATA_OFFSET, STRING_LEN_OFFSET,
};
use crate::codegen::{invariant_violation, Compiler, Frame};
use crate::sema::hir::{DeepClonePlan, Expr, Place};
use crate::AliasResult;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, BlockArg, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;

pub(crate) fn emit_deep_clone<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    source: &Expr,
    plan: &DeepClonePlan,
) -> AliasResult<Value> {
    let value = emit_expr(c, bcx, frame, source)?;
    let vty = c.vty(source.ty());
    clone_value(c, bcx, value, &vty, plan)
}

pub(crate) fn emit_deep_clone_place<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    source: &Place,
    plan: &DeepClonePlan,
) -> AliasResult<Value> {
    let (value, vty) = emit_place_value(c, bcx, frame, source)?;
    clone_value(c, bcx, value, &vty, plan)
}

fn clone_value<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    source: Value,
    vty: &VTy,
    plan: &DeepClonePlan,
) -> AliasResult<Value> {
    match plan {
        DeepClonePlan::Inline => match vty {
            VTy::I(_) | VTy::U(_) | VTy::F(_) | VTy::Bool => Ok(source),
            _ => invariant_violation("DeepClonePlan::Inline 必须对应 inline VTy"),
        },
        DeepClonePlan::String => {
            if *vty != VTy::Str {
                invariant_violation("DeepClonePlan::String 必须对应 string VTy");
            }
            clone_string(c, bcx, source)
        }
        DeepClonePlan::Struct { name, fields } => {
            let VTy::Struct(vname) = vty else {
                invariant_violation("DeepClonePlan::Struct 必须对应 struct VTy");
            };
            if vname != name {
                invariant_violation("DeepClonePlan::Struct 名称与 VTy 不一致");
            }
            clone_struct(c, bcx, source, name, fields)
        }
        DeepClonePlan::Array(elem_plan) => {
            let VTy::Array(elem_vty) = vty else {
                invariant_violation("DeepClonePlan::Array 必须对应 array VTy");
            };
            clone_array(c, bcx, source, elem_vty, elem_plan)
        }
        DeepClonePlan::Result { ok, err } => {
            let VTy::Result(ok_vty, err_vty) = vty else {
                invariant_violation("DeepClonePlan::Result 必须对应 result VTy");
            };
            clone_result(c, bcx, source, ok_vty, err_vty, ok, err)
        }
    }
}

fn clone_string<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    source: Value,
) -> AliasResult<Value> {
    let data = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), source, STRING_DATA_OFFSET);
    let len = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), source, STRING_LEN_OFFSET);
    let size = bcx.ins().iconst(types::I64, STRING_BYTES);
    let out_block = c.call_rt(bcx, "rt.heap.alloc", &[size])?;

    // 空字符串允许 data=null；只有正长度分支才能复制字节或对 data 做地址运算。
    let has_data = bcx
        .ins()
        .icmp_imm_s(IntCC::SignedGreaterThan, len, 0);
    let copy_b = bcx.create_block();
    let empty_b = bcx.create_block();
    let end_b = bcx.create_block();
    let out_data = bcx.append_block_param(end_b, types::I64);
    bcx.ins().brif(has_data, copy_b, &[], empty_b, &[]);
    bcx.seal_block(copy_b);
    bcx.seal_block(empty_b);

    bcx.switch_to_block(copy_b);
    let copied = c.call_rt(bcx, "rt.heap.alloc", &[len])?;
    let index = bcx.declare_var(types::I64);
    let zero = bcx.ins().iconst(types::I64, 0);
    bcx.def_var(index, zero);
    let loop_b = bcx.create_block();
    let body_b = bcx.create_block();
    let done_b = bcx.create_block();
    bcx.ins().jump(loop_b, &[]);
    bcx.switch_to_block(loop_b);
    let current = bcx.use_var(index);
    let more = bcx.ins().icmp(IntCC::UnsignedLessThan, current, len);
    bcx.ins().brif(more, body_b, &[], done_b, &[]);
    bcx.seal_block(body_b);
    bcx.switch_to_block(body_b);
    let src_addr = bcx.ins().iadd(data, current);
    let byte = bcx
        .ins()
        .load(types::I8, MemFlagsData::new(), src_addr, 0);
    let dst_addr = bcx.ins().iadd(copied, current);
    bcx.ins().store(MemFlagsData::new(), byte, dst_addr, 0);
    let next = bcx.ins().iadd_imm_s(current, 1);
    bcx.def_var(index, next);
    bcx.ins().jump(loop_b, &[]);
    bcx.seal_block(loop_b);
    bcx.seal_block(done_b);
    bcx.switch_to_block(done_b);
    bcx.ins().jump(end_b, &[BlockArg::Value(copied)]);

    bcx.switch_to_block(empty_b);
    let null = bcx.ins().iconst(types::I64, 0);
    bcx.ins().jump(end_b, &[BlockArg::Value(null)]);

    bcx.switch_to_block(end_b);
    bcx.seal_block(end_b);
    bcx.ins()
        .store(MemFlagsData::new(), out_data, out_block, STRING_DATA_OFFSET);
    bcx.ins()
        .store(MemFlagsData::new(), len, out_block, STRING_LEN_OFFSET);
    Ok(out_block)
}

fn clone_struct<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    source: Value,
    name: &str,
    plans: &[DeepClonePlan],
) -> AliasResult<Value> {
    let layout = c
        .struct_layouts
        .get(name)
        .cloned()
        .unwrap_or_else(|| invariant_violation("DeepClone struct layout 必须存在"));
    if layout.fields.len() != plans.len() {
        invariant_violation("DeepClone struct field plan 数量与 layout 不一致");
    }
    let bytes = bcx.ins().iconst(types::I64, layout.size as i64);
    let out = c.call_rt(bcx, "alias.cell.new", &[bytes])?;
    for (field, plan) in layout.fields.iter().zip(plans) {
        let raw = bcx
            .ins()
            .load(field.vty.abi().storage, MemFlagsData::new(), source, field.offset);
        let value = norm_load(bcx, raw, &field.vty);
        let cloned = clone_value(c, bcx, value, &field.vty, plan)?;
        let stored = norm_store(bcx, cloned, &field.vty);
        bcx.ins()
            .store(MemFlagsData::new(), stored, out, field.offset);
    }
    Ok(out)
}

fn clone_array<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    source: Value,
    elem_vty: &VTy,
    elem_plan: &DeepClonePlan,
) -> AliasResult<Value> {
    let source_raw = array_raw(bcx, source);
    let len = array_len(bcx, source_raw);
    // 不把 I64 len 缩成 arr.new 的 I32 cap；从空 backing 开始逐元素 push，避免对旧
    // universal-word runtime 的 capacity ABI 制造新的截断前提。
    let zero_cap = bcx.ins().iconst(types::I32, 0);
    let out_raw = c.call_rt(bcx, "alias.arr.new", &[zero_cap])?;

    let index = bcx.declare_var(types::I64);
    let zero = bcx.ins().iconst(types::I64, 0);
    bcx.def_var(index, zero);
    let loop_b = bcx.create_block();
    let body_b = bcx.create_block();
    let done_b = bcx.create_block();
    bcx.ins().jump(loop_b, &[]);
    bcx.switch_to_block(loop_b);
    let current = bcx.use_var(index);
    let more = bcx.ins().icmp(IntCC::UnsignedLessThan, current, len);
    bcx.ins().brif(more, body_b, &[], done_b, &[]);
    bcx.seal_block(body_b);
    bcx.switch_to_block(body_b);
    let addr = array_element_addr(bcx, source_raw, current);
    let raw = bcx
        .ins()
        .load(elem_vty.abi().storage, MemFlagsData::new(), addr, 0);
    let value = norm_load(bcx, raw, elem_vty);
    let cloned = clone_value(c, bcx, value, elem_vty, elem_plan)?;
    let word = storage_word(bcx, cloned, elem_vty);
    c.call_rt_void(bcx, "alias.arr.push", &[out_raw, word])?;
    let next = bcx.ins().iadd_imm_s(current, 1);
    bcx.def_var(index, next);
    bcx.ins().jump(loop_b, &[]);
    bcx.seal_block(loop_b);
    bcx.seal_block(done_b);
    bcx.switch_to_block(done_b);
    wrap_array(c, bcx, out_raw)
}

fn clone_result<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    source: Value,
    ok_vty: &VTy,
    err_vty: &VTy,
    ok_plan: &DeepClonePlan,
    err_plan: &DeepClonePlan,
) -> AliasResult<Value> {
    let tag = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), source, RESULT_TAG_OFFSET);
    let payload = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), source, RESULT_PAYLOAD_OFFSET);
    let words = bcx.ins().iconst(types::I32, RESULT_WORDS);
    let out = c.call_rt(bcx, "alias.env.new", &[words])?;
    bcx.ins()
        .store(MemFlagsData::new(), tag, out, RESULT_TAG_OFFSET);

    let is_ok = bcx.ins().icmp_imm_s(IntCC::Equal, tag, RESULT_OK_TAG);
    let ok_b = bcx.create_block();
    let err_b = bcx.create_block();
    let end_b = bcx.create_block();
    let cloned_word = bcx.append_block_param(end_b, types::I64);
    bcx.ins().brif(is_ok, ok_b, &[], err_b, &[]);
    bcx.seal_block(ok_b);
    bcx.seal_block(err_b);

    bcx.switch_to_block(ok_b);
    let ok_value = restore_word(bcx, payload, ok_vty);
    let ok_clone = clone_value(c, bcx, ok_value, ok_vty, ok_plan)?;
    let ok_word = storage_word(bcx, ok_clone, ok_vty);
    bcx.ins().jump(end_b, &[BlockArg::Value(ok_word)]);

    bcx.switch_to_block(err_b);
    let err_value = restore_word(bcx, payload, err_vty);
    let err_clone = clone_value(c, bcx, err_value, err_vty, err_plan)?;
    let err_word = storage_word(bcx, err_clone, err_vty);
    bcx.ins().jump(end_b, &[BlockArg::Value(err_word)]);

    bcx.switch_to_block(end_b);
    bcx.seal_block(end_b);
    bcx.ins().store(
        MemFlagsData::new(),
        cloned_word,
        out,
        RESULT_PAYLOAD_OFFSET,
    );
    Ok(out)
}
