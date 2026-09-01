use super::{declare_runtime_shim, NativeExterns};
use crate::codegen::emit::cells::first_result;
use crate::codegen::{native_err, Compiler};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::{
    types, Function, InstBuilder, MemFlagsData, StackSlotData, StackSlotKind, TrapCode,
    UserFuncName, Value,
};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{Linkage, Module};
use std::collections::HashMap;

/// packed runtime 诊断记录：little-endian u32 行号后接 u32 列号。
/// producer 与 abort shim 必须共享这些偏移；单独修改任一侧都会破坏诊断坐标。
const SPAN_LINE_OFFSET: i32 = 0;
const SPAN_COL_OFFSET: i32 = 4;
const SPAN_RECORD_BYTES: i64 = 8;

pub(crate) fn define_span_data<M: Module>(
    c: &mut Compiler<'_, M>,
    table: &[(u32, u32)],
) -> AliasResult<()> {
    let id = c
        .module
        .declare_data("alias_span_table", Linkage::Local, false, false)
        .map_err(|e| native_err(Span::default(), format!("内部: span 段声明失败 {e}")))?;
    let mut bytes = Vec::with_capacity(table.len() * SPAN_RECORD_BYTES as usize);
    for (line, col) in table {
        bytes.extend_from_slice(&line.to_le_bytes());
        bytes.extend_from_slice(&col.to_le_bytes());
    }
    let mut desc = cranelift_module::DataDescription::new();
    desc.define(bytes.into());
    c.module
        .define_data(id, &desc)
        .map_err(|e| native_err(Span::default(), format!("内部: span 段定义失败 {e}")))
}

fn emit_span_abort<M: Module>(
    c: &mut Compiler<'_, M>,
    name: &str,
    ext: &NativeExterns,
    span_data: cranelift_module::DataId,
    static_ids: &HashMap<&str, cranelift_module::DataId>,
    suffix: &str,
) -> AliasResult<()> {
    let (fid, sig, _) = declare_runtime_shim(c, name)?;
    let mut ctx = Context::new();
    ctx.func = Function::with_name_signature(UserFuncName::user(0x77, fid.as_u32()), sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut bcx = FunctionBuilder::new(&mut ctx.func, &mut fbc);
    let entry = bcx.create_block();
    bcx.append_block_params_for_function_params(entry);
    bcx.switch_to_block(entry);
    bcx.seal_block(entry);
    let a: Vec<Value> = bcx.block_params(entry).to_vec();

    let base = {
        let gv = c.module.declare_data_in_func(span_data, bcx.func);
        bcx.ins().symbol_value(c.machine_ptr_ty, gv)
    };
    let id64 = bcx.ins().uextend(types::I64, a[0]);
    let off = bcx.ins().imul_imm_s(id64, SPAN_RECORD_BYTES);
    let laddr = bcx.ins().iadd(base, off);
    let line = bcx
        .ins()
        .load(types::I32, MemFlagsData::new(), laddr, SPAN_LINE_OFFSET);
    let col = bcx
        .ins()
        .load(types::I32, MemFlagsData::new(), laddr, SPAN_COL_OFFSET);

    macro_rules! w_str {
        ($bcx:expr, $err:expr, $data:expr, $len:expr) => {{
            let __wa =
                $bcx.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
            let __wa_addr = $bcx.ins().stack_addr(c.machine_ptr_ty, __wa, 0);
            let __null = $bcx.ins().iconst(c.machine_ptr_ty, 0);
            let __gv = c
                .module
                .declare_data_in_func(static_ids[$data], &mut $bcx.func);
            let __addr = $bcx.ins().symbol_value(c.machine_ptr_ty, __gv);
            let __len = $bcx.ins().iconst(types::I32, $len);
            let __wf = c
                .module
                .declare_func_in_func(ext.write_file, &mut $bcx.func);
            $bcx.ins()
                .call(__wf, &[$err, __addr, __len, __wa_addr, __null]);
        }};
    }
    macro_rules! w_dec {
        ($bcx:expr, $err:expr, $v:expr) => {{
            let __f = c.import_runtime("rt.write.dec")?;
            let __r = c.module.declare_func_in_func(__f, &mut $bcx.func);
            let __args = [$err, bcx.ins().uextend(types::I64, $v)];
            $bcx.ins().call(__r, &__args);
        }};
    }
    let err_args = [bcx.ins().iconst(types::I32, -12)];
    let err = {
        let r = c.module.declare_func_in_func(ext.get_std_handle, bcx.func);
        let inst = bcx.ins().call(r, &err_args);
        first_result(&bcx, inst)
    };
    w_str!(
        bcx,
        err,
        "rt_msg_prefix",
        super::driver::runtime_static_len("rt_msg_prefix")
    );
    w_dec!(bcx, err, line);
    w_str!(
        bcx,
        err,
        "rt_colon",
        super::driver::runtime_static_len("rt_colon")
    );
    w_dec!(bcx, err, col);
    w_str!(bcx, err, suffix, super::driver::runtime_static_len(suffix));
    let code1 = bcx.ins().iconst(types::I32, 1);
    let ep = c.module.declare_func_in_func(ext.exit_process, bcx.func);
    bcx.ins().call(ep, &[code1]);
    // ExitProcess 在语义上不返回，但 Cranelift 不推断外部合同。即使 OS 调用异常返回，
    // trap 仍保证 block 已终止，runtime abort 绝不能穿透到用户代码。
    bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);

    bcx.finalize(c.module.target_config());
    c.define_verified_function(fid, &mut ctx, name)
}

pub(super) fn emit_abort_runtime<M: Module>(
    c: &mut Compiler<'_, M>,
    ext: &NativeExterns,
    span_data: cranelift_module::DataId,
    static_ids: &HashMap<&str, cranelift_module::DataId>,
) -> AliasResult<()> {
    for (symbol, suffix) in [
        ("alias.abort_div", "rt_msg_suffix"),
        ("alias.abort_oob", "rt_oob_suffix"),
        ("alias.abort_pop", "rt_pop_suffix"),
        ("alias.abort_conv", "rt_conv_suffix"),
        ("alias.abort_overflow", "rt_overflow_suffix"),
        ("alias.abort_iter", "rt_iter_suffix"),
    ] {
        emit_span_abort(c, symbol, ext, span_data, static_ids, suffix)?;
    }
    Ok(())
}
