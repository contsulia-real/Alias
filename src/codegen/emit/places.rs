use super::arrays::checked_array_element_addr;
use super::cells::binding_storage_addr;
use super::expr::emit_expr;
use crate::codegen::abi::{cl_type, norm_load, norm_store, VTy};
use crate::codegen::{invariant_violation, native_err, Compiler, Frame};
use crate::sema::hir::Place;
use crate::sema::types::Ty;
use crate::AliasResult;
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;

/// 已解析 struct field 的物理存储查询 owner。sema/HIR 已决定 field_index；这里仅把
/// 静态 struct 类型映射到当前 ABI layout。接口只依赖最终 `Ty`，避免 Place owner 被迫
/// 携带任意 HIR Expr 并在递归 Place projection 中重新耦合表达式形状。
pub(super) fn field_storage<M: Module>(
    c: &Compiler<M>,
    recv_ty: &Ty,
    field_index: usize,
) -> AliasResult<(VTy, i32)> {
    if let VTy::Struct(struct_name) = c.vty(recv_ty) {
        if let Some(layout) = c.struct_layouts.get(&struct_name) {
            if let Some(field) = layout.fields.get(field_index) {
                return Ok((field.vty.clone(), field.offset));
            }
        }
    }
    invariant_violation("字段访问索引必须由 sema/HIR 解析到结构体布局")
}

pub(super) fn emit_place_value<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    place: &Place,
) -> AliasResult<(Value, VTy)> {
    let (addr, vty) = emit_place_addr(c, bcx, frame, place)?;
    let raw = bcx.ins().load(cl_type(&vty), MemFlagsData::new(), addr, 0);
    Ok((norm_load(bcx, raw, &vty), vty))
}

/// resolved Place → 当前真实 storage address 的唯一物理 lowering 入口。
///
/// 这里不决定 Place 合法性、ownership 或 borrow；这些都必须已经由 sema/HIR 固化。
/// Local 物化 canonical binding cell；Field/Index 递归读取 base Place 的语言值后应用唯一
/// field/index layout owner。后续 borrow/refer 必须复用这里，不能重建 projection 规则。
pub(super) fn emit_place_addr<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    place: &Place,
) -> AliasResult<(Value, VTy)> {
    match place {
        Place::Local { binding_id, .. } => {
            let vty = c.vty(place.ty());
            let Some(addr) = binding_storage_addr(c, bcx, frame, *binding_id) else {
                return Err(native_err(place.span(), "内部: Place BindingId 无存储"));
            };
            Ok((addr, vty))
        }
        Place::Field {
            base, field_index, ..
        } => {
            let (base_value, _) = emit_place_value(c, bcx, frame, base)?;
            let (field_vty, offset) = field_storage(c, base.ty(), *field_index)?;
            let addr = bcx.ins().iadd_imm_s(base_value, offset as i64);
            Ok((addr, field_vty))
        }
        Place::Index { base, index, .. } => {
            let (array, base_vty) = emit_place_value(c, bcx, frame, base)?;
            let VTy::Array(elem_vty) = base_vty else {
                invariant_violation("Place::Index base 必须保留 array VTy")
            };
            let index_word = emit_expr(c, bcx, frame, index)?;
            let index_word = index_word.into_scalar("Place index 必须是 scalar i32 expression");
            let addr = checked_array_element_addr(c, bcx, array, index_word, place.span())?;
            Ok((addr, *elem_vty))
        }
    }
}

/// 当前 resolved Place 的唯一物理写入入口。调用者必须先完整求值 RHS，再进入这里；
/// 本函数只消费已解析 Place 地址与 ABI storage type，不重新判断 target identity/type。
pub(super) fn emit_place_write<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    target: &Place,
    value: Value,
) -> AliasResult<()> {
    let (addr, vty) = emit_place_addr(c, bcx, frame, target)?;
    let stored = norm_store(bcx, value, &vty);
    bcx.ins().store(MemFlagsData::new(), stored, addr, 0);
    Ok(())
}
