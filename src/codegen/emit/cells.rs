use super::*;

pub(crate) enum CellAddr {
    Reg(Variable),
    EnvLoad(usize),
    GlobalOff(usize),
}

pub(crate) fn cell_addr<M: Module>(
    c: &Compiler<M>,
    frame: &Frame,
    id: BindingId,
) -> Option<CellAddr> {
    for scope in frame.scopes.iter().rev() {
        if let Some(s) = scope.get(&id) {
            return Some(match s {
                Slot::Local(v) => CellAddr::Reg(*v),
                Slot::Global(off) => CellAddr::GlobalOff(*off),
            });
        }
    }
    if let Some(idx) = frame.caps.get(&id) {
        return Some(CellAddr::EnvLoad(*idx));
    }
    if frame.init_ctx {
        return None;
    }
    c.globals_final
        .get(&id)
        .map(|(off, _)| CellAddr::GlobalOff(*off))
}

pub(crate) fn read_cell(
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    addr: &CellAddr,
    vty: &VTy,
) -> Value {
    let t = cl_type(vty);
    let raw = match addr {
        CellAddr::Reg(v) => {
            let cp = bcx.use_var(*v);
            bcx.ins().load(t, MemFlagsData::new(), cp, 0)
        }
        CellAddr::EnvLoad(i) => {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            let cell = bcx
                .ins()
                .load(types::I64, MemFlagsData::new(), base, value_word_offset(*i));
            bcx.ins().load(t, MemFlagsData::new(), cell, 0)
        }
        CellAddr::GlobalOff(off) => {
            let base = bcx.use_var(frame.globals);
            bcx.ins().load(t, MemFlagsData::new(), base, *off as i32)
        }
    };
    norm_load(bcx, raw, vty)
}

pub(crate) fn write_cell(
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    addr: &CellAddr,
    v: Value,
    vty: &VTy,
) {
    let sv = norm_store(bcx, v, vty);
    match addr {
        CellAddr::Reg(v) => {
            let cp = bcx.use_var(*v);
            bcx.ins().store(MemFlagsData::new(), sv, cp, 0);
        }
        CellAddr::EnvLoad(i) => {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            let cell = bcx
                .ins()
                .load(types::I64, MemFlagsData::new(), base, value_word_offset(*i));
            bcx.ins().store(MemFlagsData::new(), sv, cell, 0);
        }
        CellAddr::GlobalOff(off) => {
            let base = bcx.use_var(frame.globals);
            bcx.ins().store(MemFlagsData::new(), sv, base, *off as i32);
        }
    }
}

pub(crate) fn emit_local_cell<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    word: Value,
    vty: VTy,
    id: BindingId,
) -> AliasResult<Variable> {
    let (sz, _) = size_align(&vty);
    let szw = bcx.ins().iconst(types::I64, sz as i64);
    let cell = c.call_rt(bcx, "alias.cell.new", &[szw])?;
    let sv = norm_store(bcx, word, &vty);
    bcx.ins().store(MemFlagsData::new(), sv, cell, 0);
    let var = bcx.declare_var(types::I64);
    bcx.def_var(var, cell);
    frame
        .scopes
        .last_mut()
        .unwrap_or_else(|| invariant_violation("作用域栈非空"))
        .insert(id, Slot::Local(var));
    frame
        .locals_vty
        .last_mut()
        .unwrap_or_else(|| invariant_violation("作用域栈非空"))
        .insert(id, vty);
    Ok(var)
}

pub(crate) fn first_result(bcx: &FunctionBuilder, inst: cranelift_codegen::ir::Inst) -> Value {
    match bcx.inst_results(inst) {
        [v] => *v,
        _ => invariant_violation("单返回值签名"),
    }
}

pub(crate) fn ensure_current(bcx: &mut FunctionBuilder, frame: &mut Frame) {
    if frame.terminated {
        let dead = bcx.create_block();
        bcx.switch_to_block(dead);
        bcx.seal_block(dead);
        frame.terminated = false;
    }
}

pub(crate) fn coerce_ret(bcx: &mut FunctionBuilder, frame: &Frame, v: Value) -> Value {
    match &frame.ret_vty {
        Some(vty) => norm_store(bcx, v, vty),
        None => v,
    }
}

pub(crate) fn push_scope(frame: &mut Frame) {
    frame.scopes.push(HashMap::new());
    frame.locals_vty.push(HashMap::new());
}

pub(crate) fn pop_scope(frame: &mut Frame) {
    frame.scopes.pop();
    frame.locals_vty.pop();
}
