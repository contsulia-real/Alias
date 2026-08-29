use crate::codegen::abi::VTy;
use crate::codegen::{invariant_violation, Compiler};
use crate::sema::hir::Expr;
use crate::AliasResult;
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
