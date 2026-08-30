use crate::codegen::abi::{cl_type, norm_load, norm_store, size_align, value_word_offset, VTy};
use crate::codegen::{invariant_violation, Compiler, Frame, Slot};
use crate::sema::hir::BindingId;
use crate::AliasResult;
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::{FunctionBuilder, Variable};
use cranelift_module::Module;
use std::collections::HashMap;

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
    // 词法局部必须先于捕获和全局解析；否则 shadowing 会读写到错误的 cell。
    // init_ctx 初始化顶层时只能看到已经登记到当前 frame 的槽位，禁止前向落回
    // globals_final，否则顶层初始化顺序会被无意绕过。
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

/// 把已经解析的 binding cell 位置统一物化成实际 machine address。
///
/// local slot 自身已经保存 cell pointer；capture env 保存共享 cell pointer；global slot
/// 则是 globals block 内的固定 offset。borrow/refer 等 Place-address 操作必须复用这里，
/// 不能各自重新解释三种 binding storage 形态。
pub(crate) fn materialize_cell_addr(
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    addr: &CellAddr,
) -> Value {
    match addr {
        CellAddr::Reg(v) => bcx.use_var(*v),
        CellAddr::EnvLoad(i) => {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            bcx.ins()
                .load(types::I64, MemFlagsData::new(), base, value_word_offset(*i))
        }
        CellAddr::GlobalOff(off) => {
            let base = bcx.use_var(frame.globals);
            bcx.ins().iadd_imm_s(base, *off as i64)
        }
    }
}

pub(crate) fn read_cell(
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    addr: &CellAddr,
    vty: &VTy,
) -> Value {
    let t = cl_type(vty);
    let cell = materialize_cell_addr(bcx, frame, addr);
    let raw = bcx.ins().load(t, MemFlagsData::new(), cell, 0);
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
    let cell = materialize_cell_addr(bcx, frame, addr);
    bcx.ins().store(MemFlagsData::new(), sv, cell, 0);
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
        // Cranelift 不允许在已有 terminator 的 block 后继续追加指令。这里创建并封闭一个
        // 不可达 dead block，仅为了承接源代码中已经不可达的后续语句并保持 builder 合法；
        // 它绝不能被接回可达 CFG，否则会把源级死代码重新变成可执行路径。
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
