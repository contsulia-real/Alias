use super::cells::{cell_addr, materialize_cell_addr};
use super::expr::emit_expr;
use crate::codegen::abi::{norm_store, VTy};
use crate::codegen::{invariant_violation, native_err, Compiler, Frame};
use crate::sema::hir::{Expr, Place};
use crate::AliasResult;
use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;

/// 已解析 struct field 的物理存储查询 owner。sema/HIR 已决定 field_index；这里仅把
/// 静态 struct 类型映射到当前 ABI layout。若字段读和字段写各自查 layout，后续 aggregate
/// layout 迁移会让两条 Place 路径产生不同的 offset/type 解释。
pub(super) fn field_storage<M: Module>(
    c: &Compiler<M>,
    recv: &Expr,
    field_index: usize,
) -> AliasResult<(VTy, i32)> {
    if let VTy::Struct(struct_name) = c.vty(recv.ty()) {
        if let Some(layout) = c.struct_layouts.get(&struct_name) {
            if let Some(field) = layout.fields.get(field_index) {
                return Ok((field.vty.clone(), field.offset));
            }
        }
    }
    invariant_violation("字段访问索引必须由 sema/HIR 解析到结构体布局")
}

/// resolved Place → 当前真实 storage address 的唯一物理 lowering 入口。
///
/// 这里不决定 Place 合法性、ownership 或 borrow；这些都必须已经由 sema/HIR 固化。
/// 它只把 Local 的 canonical binding cell 与 Field 的 canonical struct offset 物化成地址，
/// 供 replacement、后续 borrow/refer 等 storage operation 复用。
pub(super) fn emit_place_addr<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    place: &Place,
) -> AliasResult<(Value, VTy)> {
    match place {
        Place::Local { binding_id, .. } => {
            let vty = c.vty(place.ty());
            let Some(addr) = cell_addr(c, frame, *binding_id) else {
                return Err(native_err(place.span(), "内部: Place BindingId 无存储"));
            };
            Ok((materialize_cell_addr(bcx, frame, &addr), vty))
        }
        Place::Field {
            recv, field_index, ..
        } => {
            let (field_vty, offset) = field_storage(c, recv, *field_index)?;
            let recv_value = emit_expr(c, bcx, frame, recv)?;
            let addr = bcx.ins().iadd_imm(recv_value, offset as i64);
            Ok((addr, field_vty))
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
