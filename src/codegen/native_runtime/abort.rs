use super::*;

pub(crate) fn define_span_data<M: Module>(
    c: &mut Compiler<'_, M>,
    table: &[(u32, u32)],
) -> AliasResult<()> {
    let id = c
        .module
        .declare_data("alias_span_table", Linkage::Local, false, false)
        .map_err(|e| native_err(Span::default(), format!("内部: span 段声明失败 {e}")))?;
    let mut bytes = Vec::with_capacity(table.len() * 8);
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
    suffix_len: i64,
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
        let gv = c.module.declare_data_in_func(span_data, &mut bcx.func);
        bcx.ins().symbol_value(c.ptr_ty, gv)
    };
    let id64 = bcx.ins().uextend(types::I64, a[0]);
    let off = bcx.ins().imul_imm_s(id64, 8);
    let laddr = bcx.ins().iadd(base, off);
    let line = bcx.ins().load(types::I32, MemFlagsData::new(), laddr, 0);
    let col = bcx.ins().load(types::I32, MemFlagsData::new(), laddr, 4);

    macro_rules! w_str {
        ($bcx:expr, $err:expr, $data:expr, $len:expr) => {{
            let __wa =
                $bcx.create_sized_stack_slot(StackSlotData::new(StackSlotKind::ExplicitSlot, 8, 3));
            let __wa_addr = $bcx.ins().stack_addr(c.ptr_ty, __wa, 0);
            let __null = $bcx.ins().iconst(c.ptr_ty, 0);
            let __gv = c
                .module
                .declare_data_in_func(static_ids[$data], &mut $bcx.func);
            let __addr = $bcx.ins().symbol_value(c.ptr_ty, __gv);
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
        let r = c
            .module
            .declare_func_in_func(ext.get_std_handle, &mut bcx.func);
        let inst = bcx.ins().call(r, &err_args);
        first_result(&bcx, inst)
    };
    w_str!(bcx, err, "rt_msg_prefix", 9);
    w_dec!(bcx, err, line);
    w_str!(bcx, err, "rt_colon", 1);
    w_dec!(bcx, err, col);
    w_str!(bcx, err, suffix, suffix_len);
    let code1 = bcx.ins().iconst(types::I32, 1);
    let ep = c
        .module
        .declare_func_in_func(ext.exit_process, &mut bcx.func);
    bcx.ins().call(ep, &[code1]);
    bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);

    bcx.finalize(c.module.target_config());
    c.module
        .define_function(fid, &mut ctx)
        .map_err(|e| native_err(Span::default(), format!("内部: shim 定义失败 {e}")))
}

pub(super) fn emit_abort_runtime<M: Module>(
    c: &mut Compiler<'_, M>,
    ext: &NativeExterns,
    span_data: cranelift_module::DataId,
    static_ids: &HashMap<&str, cranelift_module::DataId>,
) -> AliasResult<()> {
    for (symbol, suffix, len) in [
        ("alias.abort_div", "rt_msg_suffix", 15i64),
        ("alias.abort_oob", "rt_oob_suffix", 18),
        ("alias.abort_pop", "rt_pop_suffix", 19),
        ("alias.abort_conv", "rt_conv_suffix", 18),
        ("alias.abort_overflow", "rt_overflow_suffix", 18),
    ] {
        emit_span_abort(c, symbol, ext, span_data, static_ids, suffix, len)?;
    }
    Ok(())
}
