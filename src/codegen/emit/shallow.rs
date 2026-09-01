use super::expr::emit_expr;
use crate::codegen::abi::{cl_type, norm_load, norm_store, VTy};
use crate::codegen::layout::{result_layout, RESULT_OK_TAG, RESULT_TAG_OFFSET};
use crate::codegen::{invariant_violation, Compiler, Frame};
use crate::sema::hir::{Expr, ShallowClonePlan};
use crate::AliasResult;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;

pub(crate) fn emit_shallow_clone<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    source: &Expr,
    plan: &ShallowClonePlan,
) -> AliasResult<Value> {
    let value = emit_expr(c, bcx, frame, source)?;
    let value = value.into_scalar("shallow clone 尚未支持 multi-lane source");
    let vty = c.vty(source.ty());
    shallow_value(c, bcx, value, &vty, plan)
}

fn shallow_value<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    source: Value,
    vty: &VTy,
    plan: &ShallowClonePlan,
) -> AliasResult<Value> {
    match plan {
        ShallowClonePlan::Inline => match vty {
            VTy::I(_) | VTy::U(_) | VTy::F(_) | VTy::Bool => Ok(source),
            _ => invariant_violation("ShallowClonePlan::Inline 必须对应 inline VTy"),
        },
        ShallowClonePlan::Struct { name, fields } => {
            let VTy::Struct(vname) = vty else {
                invariant_violation("ShallowClonePlan::Struct 必须对应 struct VTy");
            };
            if vname != name {
                invariant_violation("ShallowClonePlan::Struct 名称与 VTy 不一致");
            }
            shallow_struct(c, bcx, source, name, fields)
        }
        ShallowClonePlan::Result { ok, err } => {
            let VTy::Result(ok_vty, err_vty) = vty else {
                invariant_violation("ShallowClonePlan::Result 必须对应 result VTy");
            };
            shallow_result(c, bcx, source, ok_vty, err_vty, ok, err)
        }
    }
}

fn shallow_struct<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    source: Value,
    name: &str,
    plans: &[ShallowClonePlan],
) -> AliasResult<Value> {
    let layout = c
        .struct_layouts
        .get(name)
        .cloned()
        .unwrap_or_else(|| invariant_violation("ShallowClone struct layout 必须存在"));
    if layout.fields.len() != plans.len() {
        invariant_violation("ShallowClone struct field plan 数量与 layout 不一致");
    }
    let bytes = bcx.ins().iconst(types::I64, layout.size as i64);
    let out = c.call_rt(bcx, "alias.cell.new", &[bytes])?;
    for (field, plan) in layout.fields.iter().zip(plans) {
        let raw = bcx
            .ins()
            .load(cl_type(&field.vty), MemFlagsData::new(), source, field.offset);
        let value = norm_load(bcx, raw, &field.vty);
        // 当前 struct/result 物理表示仍以 heap pointer 承载 aggregate root。即使语义上的
        // shallow-safe child 未来可 inline，当前后端也必须为它创建独立 root；直接复制旧
        // pointer 会在目标 ownership 模型中制造两个 owner 指向同一资源。
        let copied = shallow_value(c, bcx, value, &field.vty, plan)?;
        let stored = norm_store(bcx, copied, &field.vty);
        bcx.ins()
            .store(MemFlagsData::new(), stored, out, field.offset);
    }
    Ok(out)
}

fn shallow_result<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    source: Value,
    ok_vty: &VTy,
    err_vty: &VTy,
    ok_plan: &ShallowClonePlan,
    err_plan: &ShallowClonePlan,
) -> AliasResult<Value> {
    let tag = bcx
        .ins()
        .load(types::I64, MemFlagsData::new(), source, RESULT_TAG_OFFSET);
    let layout = result_layout(ok_vty, err_vty);
    let bytes = bcx.ins().iconst(types::I64, layout.size as i64);
    let out = c.call_rt(bcx, "alias.cell.new", &[bytes])?;
    bcx.ins()
        .store(MemFlagsData::new(), tag, out, RESULT_TAG_OFFSET);

    let is_ok = bcx.ins().icmp_imm_s(IntCC::Equal, tag, RESULT_OK_TAG);
    let ok_b = bcx.create_block();
    let err_b = bcx.create_block();
    let end_b = bcx.create_block();
    bcx.ins().brif(is_ok, ok_b, &[], err_b, &[]);
    bcx.seal_block(ok_b);
    bcx.seal_block(err_b);

    bcx.switch_to_block(ok_b);
    let ok_value = super::value::ExprValue::load(bcx, source, layout.payload_offset, ok_vty)
        .into_scalar("result shallow clone 尚未支持 multi-lane payload");
    let ok_copy = shallow_value(c, bcx, ok_value, ok_vty, ok_plan)?;
    super::value::ExprValue::scalar(ok_copy).store(
        bcx,
        out,
        layout.payload_offset,
        ok_vty,
    );
    bcx.ins().jump(end_b, &[]);

    bcx.switch_to_block(err_b);
    let err_value = super::value::ExprValue::load(bcx, source, layout.payload_offset, err_vty)
        .into_scalar("result shallow clone 尚未支持 multi-lane payload");
    let err_copy = shallow_value(c, bcx, err_value, err_vty, err_plan)?;
    super::value::ExprValue::scalar(err_copy).store(
        bcx,
        out,
        layout.payload_offset,
        err_vty,
    );
    bcx.ins().jump(end_b, &[]);

    bcx.switch_to_block(end_b);
    bcx.seal_block(end_b);
    Ok(out)
}
