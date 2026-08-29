use super::cells::{cell_addr, write_cell};
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

/// 当前 resolved Place 的唯一物理写入入口。Local 必须继续通过 cell owner 写入，
/// 否则 capture env/global/local 三种 slot 会被错误地扁平化；Field 则复用 canonical
/// struct layout 查询。调用者先求值 RHS，再进入这里，保持现行 assignment 顺序。
pub(super) fn emit_place_write<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    target: &Place,
    value: Value,
) -> AliasResult<()> {
    match target {
        Place::Local { binding_id, .. } => {
            let vty = c.vty(target.ty());
            let Some(addr) = cell_addr(c, frame, *binding_id) else {
                return Err(native_err(target.span(), "内部: 赋值目标 BindingId 无存储"));
            };
            write_cell(bcx, frame, &addr, value, &vty);
            Ok(())
        }
        Place::Field {
            recv, field_index, ..
        } => {
            let (field_vty, offset) = field_storage(c, recv, *field_index)?;
            let recv_value = emit_expr(c, bcx, frame, recv)?;
            let stored = norm_store(bcx, value, &field_vty);
            bcx.ins()
                .store(MemFlagsData::new(), stored, recv_value, offset);
            Ok(())
        }
    }
}
