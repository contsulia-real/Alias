use super::{validate_native_runtime_coverage, NativeExterns};
use crate::codegen::{invariant_violation, native_err, Compiler};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::types;
use cranelift_module::{Linkage, Module};
use std::collections::HashMap;

pub(super) const RUNTIME_STATICS: &[(&str, &[u8])] = &[
    ("rt_nl", b"\n"),
    ("rt_true", b"true"),
    ("rt_false", b"false"),
    ("rt_func", b"<func>"),
    ("rt_struct", b"<struct>"),
    ("rt_array", b"<array>"),
    ("rt_ok", b"<ok>"),
    ("rt_err", b"<err>"),
    ("rt_nan", b"NaN"),
    ("rt_inf", b"inf"),
    ("rt_ninf", b"-inf"),
    ("rt_zero", b"0"),
    ("rt_msg_prefix", "错误 @ ".as_bytes()),
    ("rt_colon", b":"),
    ("rt_msg_suffix", " — 除以零\n".as_bytes()),
    ("rt_oob_suffix", " — 下标越界\n".as_bytes()),
    ("rt_pop_suffix", " — pop 空数组\n".as_bytes()),
    ("rt_conv_suffix", " — 转换越界\n".as_bytes()),
    ("rt_overflow_suffix", " — 整数溢出\n".as_bytes()),
    ("rt_iter_suffix", " — 遍历期间集合结构已修改\n".as_bytes()),
];

pub(super) fn runtime_static_len(name: &str) -> i64 {
    RUNTIME_STATICS
        .iter()
        .find_map(|(candidate, bytes)| (*candidate == name).then_some(bytes.len() as i64))
        .unwrap_or_else(|| invariant_violation("runtime 静态消息必须登记"))
}

pub(crate) fn emit_native_runtime<M: Module>(c: &mut Compiler<'_, M>) -> AliasResult<()> {
    let ext = NativeExterns {
        get_std_handle: c.import_external("GetStdHandle", &[types::I32], Some(c.machine_ptr_ty))?,
        write_file: c.import_external(
            "WriteFile",
            &[
                c.machine_ptr_ty,
                c.machine_ptr_ty,
                types::I32,
                c.machine_ptr_ty,
                c.machine_ptr_ty,
            ],
            Some(types::I32),
        )?,
        exit_process: c.import_external("ExitProcess", &[types::I32], None)?,
    };
    let heap_alloc = c.import_external(
        "HeapAlloc",
        &[c.machine_ptr_ty, types::I32, types::I64],
        Some(c.machine_ptr_ty),
    )?;
    let get_process_heap =
        c.import_external("GetProcessHeap", &[], Some(c.machine_ptr_ty))?;
    let rtl_move_memory =
        c.import_external(
            "RtlMoveMemory",
            &[c.machine_ptr_ty, c.machine_ptr_ty, types::I64],
            None,
        )?;

    let span_data = c
        .module
        .declare_data("alias_span_table", Linkage::Local, false, false)
        .map_err(|e| native_err(Span::default(), format!("内部: span 段声明失败 {e}")))?;

    let mut static_ids: HashMap<&str, cranelift_module::DataId> = HashMap::new();
    for &(name, bytes) in RUNTIME_STATICS {
        let id = c
            .module
            .declare_data(name, Linkage::Local, false, false)
            .map_err(|e| native_err(Span::default(), format!("内部: 数据段声明失败 {e}")))?;
        let mut desc = cranelift_module::DataDescription::new();
        desc.define(bytes.to_vec().into());
        c.module
            .define_data(id, &desc)
            .map_err(|e| native_err(Span::default(), format!("内部: 数据段定义失败 {e}")))?;
        static_ids.insert(name, id);
    }

    super::alloc::emit_alloc_runtime(c, &ext, heap_alloc, get_process_heap)?;
    super::strings::emit_string_runtime(c, rtl_move_memory)?;
    super::arrays::emit_array_runtime(c, rtl_move_memory)?;
    super::display::emit_display_runtime(c, &ext, &static_ids)?;
    super::io::emit_io_runtime(c, &ext, &static_ids)?;
    super::abort::emit_abort_runtime(c, &ext, span_data, &static_ids)?;
    validate_native_runtime_coverage(c)
}
