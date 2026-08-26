// ---------------------------------------------------------------------------
// emit — 表达式/语句发射域: 单元格访问、体/循环、表达式、调用与内建。
// 归 codegen/mod.rs 的 Compiler 状态所驱动的纯发射逻辑; 无独立状态。
// ---------------------------------------------------------------------------
use super::*;
use crate::ast::*;
use crate::codegen::{
    invariant_violation, native_err, Compiler, Slot,
};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{
    Block, BlockArg, InstBuilder, MemFlagsData, Value,
};
use cranelift_frontend::{FunctionBuilder, Variable};
use super::{Frame, VTy, decl_vty};
use cranelift_module::{Linkage, Module};
// ---------------------------------------------------------------------------
// 单元格与全局槽位访问 — 存储按类型定尺寸, 在途值按规范形 (模块头值模型)
// ---------------------------------------------------------------------------

/// 名字 → 单元格位置。解析顺序: 词法作用域链 → 本函数捕获表 (env 加载)
/// → 顶层槽位。捕获表命中返回 env 派生地址 (引用捕获: 读写穿透到定义帧)。
pub(crate) enum CellAddr {
    Reg(Variable),
    EnvLoad(usize),
    GlobalOff(usize),
}

pub(crate) fn cell_addr<M: Module>(c: &Compiler<M>, frame: &Frame, name: &str) -> Option<CellAddr> {
    for scope in frame.scopes.iter().rev() {
        if let Some(s) = scope.get(name) {
            return Some(match s {
                Slot::Local(v) => CellAddr::Reg(*v),
                Slot::Global(off) => CellAddr::GlobalOff(*off),
            });
        }
    }
    if let Some(idx) = frame.caps.get(name) {
        return Some(CellAddr::EnvLoad(*idx));
    }
    if frame.init_ctx {
        return None; // 初始化器只见表内已插入项 — 前向引用按未定义处理
    }
    c.globals_final.get(name).map(|(off, _)| CellAddr::GlobalOff(*off))
}

/// 读出规范化 (存储原始值 → 规范在途形): 有符号 sext 到 I64,
/// 无符号 zext 到 I64, 浮点保持原生宽度, 字类型原样。
pub(crate) fn norm_load(bcx: &mut FunctionBuilder, raw: Value, vty: &VTy) -> Value {
    match vty {
        VTy::I(w) => match w {
            IntW::W64 => raw,
            _ => bcx.ins().sextend(types::I64, raw),
        },
        VTy::U(w) => match w {
            UIntW::U64 => raw,
            _ => bcx.ins().uextend(types::I64, raw),
        },
        _ => raw,
    }
}

/// 写入规范化 (规范在途形 → 存储类型): 窄宽截断到声明宽度,
/// f64→f32 槽位舍入降档, 其余原样。
pub(crate) fn norm_store(bcx: &mut FunctionBuilder, v: Value, vty: &VTy) -> Value {
    match vty {
        VTy::I(w) => match w.bits() {
            64 => v,
            b => bcx.ins().ireduce(ir_type_bits(b), v),
        },
        VTy::U(w) => match w.bits() {
            64 => v,
            b => bcx.ins().ireduce(ir_type_bits(b), v),
        },
        VTy::F(FloatW::F32) => match bcx.func.dfg.value_type(v) {
            types::F64 => bcx.ins().fdemote(types::F32, v),
            _ => v,
        },
        VTy::F(FloatW::F64) => match bcx.func.dfg.value_type(v) {
            types::F32 => bcx.ins().fpromote(types::F64, v),
            _ => v,
        },
        _ => v,
    }
}

fn ir_type_bits(b: u32) -> cranelift_codegen::ir::Type {
    match b {
        8 => types::I8,
        16 => types::I16,
        32 => types::I32,
        _ => types::I64,
    }
}

/// 规范在途形 → 8 字节存储字 (result 载荷槽 / 数组元素字通道):
/// 浮点位转换进 I64, 整数已为规范 I64, 字类型原样。
pub(crate) fn storage_word(bcx: &mut FunctionBuilder, v: Value, vty: &VTy) -> Value {
    match vty {
        VTy::F(_) => bcx.ins().bitcast(types::I64, MemFlagsData::new(), v),
        VTy::U(w) => match w.bits() {
            64 => v,
            b => {
                let red = narrow(bcx, v, b);
                bcx.ins().uextend(types::I64, red)
            }
        },
        VTy::I(w) => match w.bits() {
            64 => v,
            b => {
                let red = narrow(bcx, v, b);
                bcx.ins().sextend(types::I64, red)
            }
        },
        _ => v,
    }
}

/// 8 字节存储字 → 规范在途形: 浮点位转回原生; 整数/指针载荷槽
/// 存储的即规范 I64 字 — 直通 (窄型已在写入端 norm_store 截断)。
pub(crate) fn restore_word(bcx: &mut FunctionBuilder, raw: Value, vty: &VTy) -> Value {
    match vty {
        VTy::F(w) => bcx.ins().bitcast(cl_type(&VTy::F(*w)), MemFlagsData::new(), raw),
        _ => raw,
    }
}

/// 数组元素写入: 存储字的低 esize 字节落缓冲
fn store_elem(bcx: &mut FunctionBuilder, w: Value, addr: Value, elem_vty: &VTy) {
    match elem_vty {
        VTy::F(FloatW::F32) => {
            let f = bcx.ins().bitcast(types::F32, MemFlagsData::new(), w);
            bcx.ins().store(MemFlagsData::new(), f, addr, 0);
        }
        VTy::F(FloatW::F64) => {
            let f = bcx.ins().bitcast(types::F64, MemFlagsData::new(), w);
            bcx.ins().store(MemFlagsData::new(), f, addr, 0);
        }
        VTy::I(IntW::W8) | VTy::U(UIntW::U8) => {
            let b = bcx.ins().ireduce(types::I8, w);
            bcx.ins().store(MemFlagsData::new(), b, addr, 0);
        }
        VTy::I(IntW::W16) | VTy::U(UIntW::U16) => {
            let h = bcx.ins().ireduce(types::I16, w);
            bcx.ins().store(MemFlagsData::new(), h, addr, 0);
        }
        VTy::I(IntW::W32) | VTy::U(UIntW::U32) => {
            let x = bcx.ins().ireduce(types::I32, w);
            bcx.ins().store(MemFlagsData::new(), x, addr, 0);
        }
        _ => {
            bcx.ins().store(MemFlagsData::new(), w, addr, 0);
        }
    }
}

pub(crate) fn read_cell(bcx: &mut FunctionBuilder, frame: &Frame, addr: &CellAddr, vty: &VTy) -> Value {
    let t = cl_type(vty);
    let raw = match addr {
        CellAddr::Reg(v) => {
            let cp = bcx.use_var(*v);
            bcx.ins().load(t, MemFlagsData::new(), cp, 0)
        }
        CellAddr::EnvLoad(i) => {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            let cell = bcx.ins().load(types::I64, MemFlagsData::new(), base, ((*i as i64) * 8) as i32);
            bcx.ins().load(t, MemFlagsData::new(), cell, 0)
        }
        CellAddr::GlobalOff(off) => {
            let base = bcx.use_var(frame.globals);
            bcx.ins().load(t, MemFlagsData::new(), base, *off as i32)
        }
    };
    norm_load(bcx, raw, vty)
}

pub(crate) fn write_cell(bcx: &mut FunctionBuilder, frame: &Frame, addr: &CellAddr, v: Value, vty: &VTy) {
    let sv = norm_store(bcx, v, vty);
    match addr {
        CellAddr::Reg(v) => {
            let cp = bcx.use_var(*v);
            bcx.ins().store(MemFlagsData::new(), sv, cp, 0);
        }
        CellAddr::EnvLoad(i) => {
            let base = bcx.use_var(frame.env.unwrap_or_else(|| invariant_violation("env 存在")));
            let cell = bcx.ins().load(types::I64, MemFlagsData::new(), base, ((*i as i64) * 8) as i32);
            bcx.ins().store(MemFlagsData::new(), sv, cell, 0);
        }
        CellAddr::GlobalOff(off) => {
            let base = bcx.use_var(frame.globals);
            bcx.ins().store(MemFlagsData::new(), sv, base, *off as i32);
        }
    }
}

/// 绑定 → 新鲜定尺寸单元格 + 登记 SSA 变量与静态类型。
pub(crate) fn emit_local_cell<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    word: Value,
    vty: VTy,
    name: &str,
) -> AliasResult<Variable> {
    let (sz, _) = size_align(&vty);
    let szw = bcx.ins().iconst(types::I64, sz as i64);
    let cell = c.call_rt(bcx, "alias.cell.new", &[types::I64], Some(types::I64), &[szw])?;
    let sv = norm_store(bcx, word, &vty);
    bcx.ins().store(MemFlagsData::new(), sv, cell, 0);
    let var = bcx.declare_var(types::I64);
    bcx.def_var(var, cell);
    let scope = frame.scopes.last_mut().unwrap_or_else(|| invariant_violation("作用域栈非空"));
    scope.insert(name.to_string(), Slot::Local(var));
    frame.locals_vty.last_mut().unwrap_or_else(|| invariant_violation("作用域栈非空"))
        .insert(name.to_string(), vty);
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

/// 返回值桥接 (存储规范化裁决①): emit 层整型值为 I64 规范字,
/// ret_block 块参数为声明宽度 — jump 前按宽度截断; 浮点原生通道直通;
/// 窄整型读回由 norm_load 符号/零扩展。
fn coerce_ret(bcx: &mut FunctionBuilder, frame: &Frame, v: Value) -> Value {
    match &frame.ret_vty {
        Some(vty) => norm_store(bcx, v, vty),
        None => v,
    }
}

// ---------------------------------------------------------------------------
// 体发射
// ---------------------------------------------------------------------------

pub(crate) fn emit_body<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    body: &Body,
    ret_block: Block,
) -> AliasResult<()> {
    match body {
        Body::ArrowExpr(e) => {
            let v = emit_expr(c, bcx, frame, e)?;
            let v = coerce_ret(bcx, frame, v);
            bcx.ins().jump(ret_block, &[BlockArg::Value(v)]);
            frame.terminated = true;
        }
        Body::Block(stmts) => {
            for s in stmts {
                ensure_current(bcx, frame);
                emit_stmt(c, bcx, frame, s, ret_block)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn emit_stmt<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    s: &Stmt,
    ret_block: Block,
) -> AliasResult<()> {
    match s {
        Stmt::Binding(b) => {
            if b.kind == BindKind::Func {
                c.fn_ret_by_name.insert(b.name.clone(), decl_vty(&b.ty, &c.struct_layouts));
            }
            // insert-after-eval: 初始化器先求值, 名字后可见 (Q⑥ 顺序语义)
            let v = emit_expr(c, bcx, frame, &b.value)?;
            if b.kind == BindKind::Func {
                // func 绑定: 值即闭包对象, 存入局部单元格
                emit_local_cell(c, bcx, frame, v, VTy::Func, &b.name)?;
            } else {
                emit_local_cell(c, bcx, frame, v, decl_vty(&b.ty, &c.struct_layouts), &b.name)?;
            }
            Ok(())
        }
        Stmt::FieldAssign { recv, field, value, .. } => {
            // 先值后目标 — 与简名赋值同序 (黄金记录冻结的求值顺序)
            let v = emit_expr(c, bcx, frame, value)?;
            let p = emit_expr(c, bcx, frame, recv)?;
            let fvty = field_vty(c, frame, recv, field)?;
            let off = field_offset(c, frame, recv, field)?;
            let sv = norm_store(bcx, v, &fvty);
            bcx.ins().store(MemFlagsData::new(), sv, p, off);
            Ok(())
        }
        Stmt::Assign { target, value, .. } => {
            // 先值后目标 — 黄金记录冻结的求值顺序
            let v = emit_expr(c, bcx, frame, value)?;
            match cell_addr(c, frame, target) {
                Some(addr) => {
                    let tvty = vty_of_name(c, frame, target);
                    write_cell(bcx, frame, &addr, v, &tvty);
                    Ok(())
                }
                None => Err(native_err(
                    Span::default(),
                    format!("赋值目标 '{target}' 未定义"),
                )),
            }
        }
        Stmt::ExprStmt { expr, .. } => {
            emit_expr(c, bcx, frame, expr)?;
            Ok(())
        }
        Stmt::Return { value, .. } => {
            let v = match value {
                Some(e) => emit_expr(c, bcx, frame, e)?,
                None => bcx.ins().iconst(types::I64, 0),
            };
            let v = coerce_ret(bcx, frame, v);
            bcx.ins().jump(ret_block, &[BlockArg::Value(v)]);
            frame.terminated = true;
            Ok(())
        }
        Stmt::For { cond, body, .. } | Stmt::While { cond, body, .. } => {
            emit_loop(c, bcx, frame, cond, body, ret_block)
        }
    }
}

/// 循环: 条件每迭代求值; 体在子作用域发射且绑定分配新鲜单元格 —
/// 跨迭代捕获看到逐迭代值 (每迭代子作用域, spec-notes §附录三)。
pub(crate) fn emit_loop<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    cond: &Expr,
    body: &[Stmt],
    ret_block: Block,
) -> AliasResult<()> {
    ensure_current(bcx, frame);
    let header = bcx.create_block();
    let body_b = bcx.create_block();
    let end_b = bcx.create_block();
    bcx.ins().jump(header, &[]);
    frame.terminated = true;

    // header 不封口: 回边是其第二前驱, 待体末尾跳回后再封
    bcx.switch_to_block(header);
    frame.terminated = false;
    let cv = emit_expr(c, bcx, frame, cond)?;
    bcx.ins().brif(cv, body_b, &[], end_b, &[]);
    frame.terminated = true;
    bcx.seal_block(body_b);
    bcx.seal_block(end_b);

    bcx.switch_to_block(body_b);
    frame.terminated = false;
    frame.scopes.push(HashMap::new());
    frame.locals_vty.push(HashMap::new());
    for s in body {
        ensure_current(bcx, frame);
        emit_stmt(c, bcx, frame, s, ret_block)?;
    }
    frame.scopes.pop();
    frame.locals_vty.pop();
    if !frame.terminated {
        bcx.ins().jump(header, &[]);
    }
    bcx.seal_block(header); // 前驱齐备: 入口跳转 (+ 回边)
    bcx.switch_to_block(end_b);
    frame.terminated = false;
    Ok(())
}
// ---------------------------------------------------------------------------
// 表达式发射 — 一切结果为 64 位规范字
// ---------------------------------------------------------------------------

pub(crate) fn emit_expr<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    e: &Expr,
) -> AliasResult<Value> {
    match e {
        Expr::Int(n, _) => {
            // 字面量按 i32 语义承载 (槽位统一在 sema/绑定层完成, 存入即规范化)
            Ok(bcx.ins().iconst(types::I64, *n as i32 as i64))
        }
        Expr::Float(v, _) => Ok(bcx.ins().f64const(*v)),
        Expr::Bool(b, _) => Ok(bcx.ins().iconst(types::I64, *b as i64)),
        Expr::Unit(_) => Ok(bcx.ins().iconst(types::I64, 0)),
        Expr::Str(parts, _) => emit_str(c, bcx, frame, parts),
        Expr::Ident(name, span) => match cell_addr(c, frame, name) {
            Some(addr) => {
                let vty = vty_of_name(c, frame, name);
                Ok(read_cell(bcx, frame, &addr, &vty))
            }
            None => Err(native_err(*span, format!("未定义的绑定 '{name}'"))),
        },
        Expr::Neg { expr, .. } => {
            let v = emit_expr(c, bcx, frame, expr)?;
            let t = static_vty(c, frame, expr);
            match t {
                // 窄宽取负在声明宽度 wrapping (裁决①); 浮点原生 fneg
                VTy::I(w) => {
                    let wt = cl_type(&VTy::I(w));
                    let red = narrow(bcx, v, w.bits());
                    let n = bcx.ins().ineg(red);
                    Ok(widen_signed(bcx, n, wt))
                }
                VTy::F(_) => Ok(bcx.ins().fneg(v)),
                _ => invariant_violation("取负操作数为有符号整数或浮点 (sema 已校验)"),
            }
        }
        Expr::Binary { op, lhs, rhs, span } => {
            // lhs-before-rhs — 黄金记录冻结的求值序
            let l = emit_expr(c, bcx, frame, lhs)?;
            let r = emit_expr(c, bcx, frame, rhs)?;
            emit_binary(c, bcx, frame, *op, lhs, l, r, *span)
        }
        Expr::Call { callee, args, span } => emit_call(c, bcx, frame, callee, args, *span),
        // Phase 2c: 静态分派 — 接收者字为首个实参, 直调内部函数;
        // 内建字符串方法落运行时符号 (双后端同契约)
        Expr::MethodCall { recv, name, args, span } => {
            emit_method_call(c, bcx, frame, recv, name, args, *span)
        }
        // 字段读取 (Phase 2a): recv 求值 → 实例指针 → 偏移加载 (类型化宽度)。
        // recv 非结构体在 sema 已拒绝 — 此处按不变式直接回查布局
        Expr::Field { recv, name, .. } => {
            let p = emit_expr(c, bcx, frame, recv)?;
            let fvty = field_vty(c, frame, recv, name)?;
            let off = field_offset(c, frame, recv, name)?;
            let raw = bcx.ins().load(cl_type(&fvty), MemFlagsData::new(), p, off);
            Ok(norm_load(bcx, raw, &fvty))
        }
        // 下标读 (Phase 2d): 主语与下标按序求值 → I32 域越界守卫
        // (i<0 或 i>=len → span-ID 中止存根) → 元素缓冲按元素步长加载
        Expr::Index { recv, idx, span } => {
            let arr = emit_expr(c, bcx, frame, recv)?;
            let idxw = emit_expr(c, bcx, frame, idx)?;
            let elem_vty = match static_vty(c, frame, recv) {
                VTy::Array(inner) => (*inner).clone(),
                _ => invariant_violation("下标主语为 array (sema 已校验)"),
            };
            // P3a 布局简化裁决: 缓冲统一 8 字节步长 — 窄元素由 norm_store/
            // norm_load 在槽内规范化, 免除 shim 的运行时宽度分派
            let esize = 8usize;
            let idx32 = bcx.ins().ireduce(types::I32, idxw);
            let len64 = bcx.ins().load(types::I64, MemFlagsData::new(), arr, 8);
            let len32 = bcx.ins().ireduce(types::I32, len64);
            emit_index_guard(c, bcx, idx32, len32, *span)?;
            let dp = bcx.ins().load(types::I64, MemFlagsData::new(), arr, 0);
            let idx64 = bcx.ins().sextend(types::I64, idx32);
            let off = bcx.ins().imul_imm_s(idx64, esize as i64);
            let addr = bcx.ins().iadd(dp, off);
            let raw = bcx.ins().load(cl_type(&elem_vty), MemFlagsData::new(), addr, 0);
            Ok(norm_load(bcx, raw, &elem_vty))
        }
        // 数组字面量 (Phase 2d): 头块分配 → 元素按书写序求值逐个入缓冲
        // (lhs-to-rhs 黄金冻结约定) → 回填 len; 元素步长按元素类型
        Expr::ArrayLit { elems, .. } => {
            let n = elems.len() as i64;
            let elem_vty = elems
                .first()
                .map(|el| static_vty(c, frame, el))
                .unwrap_or(VTy::Other);
            // P3a 布局简化裁决: 缓冲统一 8 字节步长 — 窄元素由 norm_store/
            // norm_load 在槽内规范化, 免除 shim 的运行时宽度分派
            let esize = 8usize;
            let cap = bcx.ins().iconst(types::I32, n);
            let eszw = bcx.ins().iconst(types::I32, esize as i64);
            let hdr = c.call_rt(
                bcx,
                "alias.arr.new",
                &[types::I32, types::I32],
                Some(types::I64),
                &[cap, eszw],
            )?;
            for (i, el) in elems.iter().enumerate() {
                let v = emit_expr(c, bcx, frame, el)?;
                let dp = bcx.ins().load(types::I64, MemFlagsData::new(), hdr, 0);
                let addr = bcx.ins().iadd_imm_s(dp, (i as i64) * esize as i64);
                let sv = storage_word(bcx, v, &elem_vty);
                store_elem(bcx, sv, addr, &elem_vty);
            }
            let lenw = bcx.ins().iconst(types::I64, n);
            bcx.ins().store(MemFlagsData::new(), lenw, hdr, 8);
            Ok(hdr)
        }
        Expr::FuncLit { params, body, .. } => {
            emit_funclit_value(c, bcx, frame, params, body)
        }
        // match 降级 (Phase 2b): 载入 tag → brif 分臂 → join 块参数汇合。
        // never 臂 (return 收尾) 直接跳函数返回块, 不进 join —
        // 双臂皆 never 时无 join, 匹配值取哑字 (sema 已判其类型不可用)。
        Expr::Match { subject, arms, .. } => {
            let subj = emit_expr(c, bcx, frame, subject)?;
            let tag = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 0);
            let is_ok = bcx.ins().icmp_imm_s(IntCC::Equal, tag, 0);
            let ok_b = bcx.create_block();
            let err_b = bcx.create_block();
            let join_b = bcx.create_block();
            let jv = bcx.append_block_param(join_b, types::I64);
            bcx.ins().brif(is_ok, ok_b, &[], err_b, &[]);
            bcx.seal_block(ok_b);
            bcx.seal_block(err_b);

            let ok_arm = arms
                .iter()
                .find(|a| a.ctor == CtorKind::Ok)
                .unwrap_or_else(|| invariant_violation("match ok 臂存在 (sema 已校验)"));
            let err_arm = arms
                .iter()
                .find(|a| a.ctor == CtorKind::Err)
                .unwrap_or_else(|| invariant_violation("match err 臂存在 (sema 已校验)"));

            let bind_vtys = match static_vty(c, frame, subject) {
                VTy::Result(t, e) => (
                    vty_of_type_name(&c.struct_layouts, &t),
                    vty_of_type_name(&c.struct_layouts, &e),
                ),
                _ => (VTy::Other, VTy::Other),
            };

            bcx.switch_to_block(ok_b);
            frame.terminated = false;
            let ok_joined =
                emit_match_arm(c, bcx, frame, ok_arm, bind_vtys.0, subj, join_b)?;
            bcx.switch_to_block(err_b);
            frame.terminated = false;
            let err_joined =
                emit_match_arm(c, bcx, frame, err_arm, bind_vtys.1, subj, join_b)?;

            if ok_joined || err_joined {
                bcx.seal_block(join_b);
                bcx.switch_to_block(join_b);
                frame.terminated = false;
                Ok(jv)
            } else {
                ensure_current(bcx, frame);
                Ok(bcx.ins().iconst(types::I64, 0))
            }
        }
        // expr? 脱糖 (P6): tag==1 → return err(载荷) — 即原样返回主语块
        // (tag 已为 1, 与重包一块可观察等价); 否则值 = 载荷。无需 join。
        Expr::Propagate { expr, .. } => {
            let subj = emit_expr(c, bcx, frame, expr)?;
            let tag = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 0);
            let is_err = bcx.ins().icmp_imm_s(IntCC::Equal, tag, 1);
            let err_b = bcx.create_block();
            let ok_b = bcx.create_block();
            bcx.ins().brif(is_err, err_b, &[], ok_b, &[]);
            bcx.seal_block(err_b);
            bcx.seal_block(ok_b);

            bcx.switch_to_block(err_b);
            let rb = frame
                .ret_block
                .unwrap_or_else(|| invariant_violation("? 仅在函数体内可达 (sema 已校验)"));
            bcx.ins().jump(rb, &[BlockArg::Value(subj)]);
            frame.terminated = true;

            bcx.switch_to_block(ok_b);
            frame.terminated = false;
            let pvty = match static_vty(c, frame, expr) {
                VTy::Result(t, _) => vty_of_type_name(&c.struct_layouts, &t),
                _ => VTy::Other,
            };
            let raw = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 8);
            Ok(restore_word(bcx, raw, &pvty))
        }
    }
}

/// 二元运算发射: 按静态类型族分派 — 整数窄宽运算 (声明宽度 wrapping)、
/// 浮点原生指令 (IEEE, 除零无守卫)、比较产出 Bool 规范字。
fn emit_binary<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    op: BinOp,
    lhs: &Expr,
    l: Value,
    r: Value,
    span: Span,
) -> AliasResult<Value> {
    use BinOp::*;
    let lt = static_vty(c, frame, lhs);
    match op {
        Add | Sub | Mul | Div => match &lt {
            VTy::F(_) => {
                // 浮点双通道: 原生 F32/F64 指令; 除零为 IEEE ±inf (裁决⑤, 无守卫)
                match op {
                    Add => Ok(bcx.ins().fadd(l, r)),
                    Sub => Ok(bcx.ins().fsub(l, r)),
                    Mul => Ok(bcx.ins().fmul(l, r)),
                    _ => Ok(bcx.ins().fdiv(l, r)),
                }
            }
            VTy::I(w) => {
                let wt = cl_type(&VTy::I(*w));
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Add => bcx.ins().iadd(li, ri),
                    Sub => bcx.ins().isub(li, ri),
                    Mul => bcx.ins().imul(li, ri),
                    _ => emit_div_guard(c, bcx, li, ri, true, w.bits(), span)?,
                };
                Ok(widen_signed(bcx, v, wt))
            }
            VTy::U(w) => {
                let wt = cl_type(&VTy::U(*w));
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Add => bcx.ins().iadd(li, ri),
                    Sub => bcx.ins().isub(li, ri),
                    Mul => bcx.ins().imul(li, ri),
                    _ => emit_div_guard(c, bcx, li, ri, false, w.bits(), span)?,
                };
                Ok(widen_unsigned(bcx, v, wt))
            }
            _ => invariant_violation("算术操作数为数值族 (sema 已校验)"),
        },
        Lt | Le | Gt | Ge | EqEq | NotEq => {
            use cranelift_codegen::ir::condcodes::FloatCC;
            let b = match &lt {
                VTy::Str => {
                    let ord = call_str_cmp(c, bcx, l, r)?;
                    let cc = int_cc(op, true);
                    bcx.ins().icmp_imm_s(cc, ord, 0)
                }
                VTy::F(_) => {
                    let cc = match op {
                        Lt => FloatCC::LessThan,
                        Le => FloatCC::LessThanOrEqual,
                        Gt => FloatCC::GreaterThan,
                        Ge => FloatCC::GreaterThanOrEqual,
                        EqEq => FloatCC::Equal,
                        _ => FloatCC::NotEqual,
                    };
                    bcx.ins().fcmp(cc, l, r)
                }
                VTy::I(w) => {
                    let li = narrow(bcx, l, w.bits());
                    let ri = narrow(bcx, r, w.bits());
                    bcx.ins().icmp(int_cc(op, true), li, ri)
                }
                VTy::U(w) => {
                    let li = narrow(bcx, l, w.bits());
                    let ri = narrow(bcx, r, w.bits());
                    bcx.ins().icmp(int_cc(op, false), li, ri)
                }
                _ => {
                    // bool 相等性 (Q①: 有序比较已被 sema 拒绝)
                    bcx.ins().icmp(int_cc(op, true), l, r)
                }
            };
            Ok(bcx.ins().uextend(types::I64, b))
        }
    }
}

fn int_cc(op: BinOp, signed: bool) -> IntCC {
    match (op, signed) {
        (BinOp::Lt, true) => IntCC::SignedLessThan,
        (BinOp::Le, true) => IntCC::SignedLessThanOrEqual,
        (BinOp::Gt, true) => IntCC::SignedGreaterThan,
        (BinOp::Ge, true) => IntCC::SignedGreaterThanOrEqual,
        (BinOp::Lt, false) => IntCC::UnsignedLessThan,
        (BinOp::Le, false) => IntCC::UnsignedLessThanOrEqual,
        (BinOp::Gt, false) => IntCC::UnsignedGreaterThan,
        (BinOp::Ge, false) => IntCC::UnsignedGreaterThanOrEqual,
        (BinOp::EqEq, _) => IntCC::Equal,
        (BinOp::NotEq, _) => IntCC::NotEqual,
        // 算术运算符不经此表 (emit_binary 先行分派)
        _ => invariant_violation("比较谓词仅用于比较运算符"),
    }
}
/// 窄宽化: 规范 I64 → 声明宽度截断 (W64 原样)
fn narrow(bcx: &mut FunctionBuilder, v: Value, bits: u32) -> Value {
    match bits {
        8 => bcx.ins().ireduce(types::I8, v),
        16 => bcx.ins().ireduce(types::I16, v),
        32 => bcx.ins().ireduce(types::I32, v),
        _ => v,
    }
}

fn widen_signed(bcx: &mut FunctionBuilder, v: Value, to: cranelift_codegen::ir::Type) -> Value {
    if to == types::I64 { v } else { bcx.ins().sextend(types::I64, v) }
}

fn widen_unsigned(bcx: &mut FunctionBuilder, v: Value, to: cranelift_codegen::ir::Type) -> Value {
    if to == types::I64 { v } else { bcx.ins().uextend(types::I64, v) }
}

/// 单臂发射: 绑定 = 新鲜单元格持载荷 (val 语义); 返回是否跳入了 join
/// (false = never 流, 已跳函数返回块)。载荷槽恒 8 字节 — 浮点载荷经
/// 位转换进出 (存储层重解释, 寄存器内仍为原生浮点)。
pub(crate) fn emit_match_arm<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    arm: &MatchArm,
    bind_vty: VTy,
    subj: Value,
    join_b: Block,
) -> AliasResult<bool> {
    let raw = bcx.ins().load(types::I64, MemFlagsData::new(), subj, 8);
    let payload = restore_word(bcx, raw, &bind_vty);
    frame.scopes.push(HashMap::new());
    frame.locals_vty.push(HashMap::new());
    emit_local_cell(c, bcx, frame, payload, bind_vty, &arm.binding)?;
    let joined = match &arm.body {
        ArmBody::Value(e) => {
            let v = emit_expr(c, bcx, frame, e)?;
            bcx.ins().jump(join_b, &[BlockArg::Value(v)]);
            true
        }
        ArmBody::Ret(e) => {
            let v = emit_expr(c, bcx, frame, e)?;
            let rb = frame
                .ret_block
                .unwrap_or_else(|| invariant_violation("never 臂仅在函数体内可达 (sema 已校验)"));
            bcx.ins().jump(rb, &[BlockArg::Value(v)]);
            frame.terminated = true;
            false
        }
        ArmBody::Block(stmts) => {
            let rb = frame
                .ret_block
                .unwrap_or_else(|| invariant_violation("臂内 return 仅在函数体内可达 (sema 已校验)"));
            let n = stmts.len();
            let mut tail: Option<Value> = None;
            for (i, s) in stmts.iter().enumerate() {
                ensure_current(bcx, frame);
                if i + 1 == n {
                    if let Stmt::ExprStmt { expr, .. } = s {
                        tail = Some(emit_expr(c, bcx, frame, expr)?);
                        continue;
                    }
                }
                emit_stmt(c, bcx, frame, s, rb)?;
            }
            if frame.terminated {
                false
            } else {
                // 尾表达式 = 臂值; 其余收尾 (unit 臂) 规范字 0
                let v = tail.unwrap_or_else(|| bcx.ins().iconst(types::I64, 0));
                bcx.ins().jump(join_b, &[BlockArg::Value(v)]);
                true
            }
        }
    };
    frame.scopes.pop();
    frame.locals_vty.pop();
    Ok(joined)
}

/// 插值/字符串字面量: 各部分 display 成串后左折叠 concat。
pub(crate) fn emit_str<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    parts: &[StrPartAst],
) -> AliasResult<Value> {
    let z8 = bcx.ins().iconst(types::I64, 0);
    let z4 = bcx.ins().iconst(types::I32, 0);
    let empty = c.call_rt(bcx, "alias.str.new", &[types::I64, types::I32], Some(types::I64), &[z8, z4])?;
    let mut acc = empty;
    for p in parts {
        let piece = match p {
            StrPartAst::Lit(s) => str_literal_handle(c, bcx, s)?,
            StrPartAst::Hole(h) => {
                let w = emit_expr(c, bcx, frame, h)?;
                display_word(c, bcx, frame, h, w)?
            }
        };
        acc = c.call_rt(bcx, "alias.str.concat", &[types::I64, types::I64], Some(types::I64), &[acc, piece])?;
    }
    Ok(acc)
}

/// 字面量字节经数据段内嵌; 块 = 数据段地址的复制块。
pub(crate) fn str_literal_handle<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    s: &str,
) -> AliasResult<Value> {
    let data_id = match c.str_data.get(s) {
        Some(id) => *id,
        None => {
            let dname = format!("str{}", c.str_data.len());
            let id = c
                .module
                .declare_data(&dname, Linkage::Local, false, false)
                .map_err(|e| native_err(Span::default(), format!("内部: 数据段声明失败 {e}")))?;
            let mut desc = cranelift_module::DataDescription::new();
            desc.define(s.as_bytes().to_vec().into());
            c.module
                .define_data(id, &desc)
                .map_err(|e| native_err(Span::default(), format!("内部: 数据段定义失败 {e}")))?;
            c.str_data.insert(s.to_string(), id);
            id
        }
    };
    let gv = c.module.declare_data_in_func(data_id, &mut bcx.func);
    let addr = bcx.ins().symbol_value(c.ptr_ty, gv);
    let len = bcx.ins().iconst(types::I32, s.len() as i64);
    c.call_rt(bcx, "alias.str.new", &[c.ptr_ty, types::I32], Some(types::I64), &[addr, len])
}

/// 按静态类型把在途值 display 成字符串块 (Value::display 逐字节规则)。
/// 数值族: 窄宽整数复用 i32 通道 (规范形已在值域内); u32/u64 走无符号
/// 通道; 浮点双后端同一定点格式 (6 位小数去尾零 — 消除跨后端打印不对称)。
pub(crate) fn display_word<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    e: &Expr,
    w: Value,
) -> AliasResult<Value> {
    match static_vty(c, frame, e) {
        VTy::I(IntW::W64) => {
            c.call_rt(bcx, "alias.display.i64", &[types::I64], Some(types::I64), &[w])
        }
        VTy::I(_) => {
            let t = bcx.ins().ireduce(types::I32, w);
            c.call_rt(bcx, "alias.display.int", &[types::I32], Some(types::I64), &[t])
        }
        // u8/u16 规范形 ≤ 65535, i32 通道安全; u32/u64 需无符号十进制
        VTy::U(UIntW::U8) | VTy::U(UIntW::U16) => {
            let t = bcx.ins().ireduce(types::I32, w);
            c.call_rt(bcx, "alias.display.int", &[types::I32], Some(types::I64), &[t])
        }
        VTy::U(_) => {
            c.call_rt(bcx, "alias.display.u64", &[types::I64], Some(types::I64), &[w])
        }
        VTy::F(FloatW::F32) => {
            c.call_rt(bcx, "alias.display.f32", &[types::F32], Some(types::I64), &[w])
        }
        VTy::F(FloatW::F64) => {
            c.call_rt(bcx, "alias.display.f64", &[types::F64], Some(types::I64), &[w])
        }
        VTy::Bool => {
            let t = bcx.ins().ireduce(types::I32, w);
            c.call_rt(bcx, "alias.display.bool", &[types::I32], Some(types::I64), &[t])
        }
        VTy::Str => c.call_rt(bcx, "alias.display.str", &[types::I64], Some(types::I64), &[w]),
        VTy::Unit => c.call_rt(bcx, "alias.display.unit", &[], Some(types::I64), &[]),
        VTy::Func => c.call_rt(bcx, "alias.display.func", &[], Some(types::I64), &[]),
        // 结构体值永不泄露内部布局 — 固定 "<struct>" (与 <func> 同规约)
        VTy::Struct(_) => c.call_rt(bcx, "alias.display.struct", &[], Some(types::I64), &[]),
        // 数组值永不泄露元素 — 固定 "<array>" (与 <struct> 对称, Phase 2d)
        VTy::Array(_) => c.call_rt(bcx, "alias.display.array", &[], Some(types::I64), &[]),
        // result 值按运行时 tag 显示 <ok>/<err> — 静态类型名不参与
        VTy::Result(..) => {
            let tag = bcx.ins().load(types::I64, MemFlagsData::new(), w, 0);
            let t = bcx.ins().ireduce(types::I32, tag);
            c.call_rt(bcx, "alias.display.result", &[types::I32], Some(types::I64), &[t])
        }
        VTy::Other => Err(native_err(
            e.span(),
            "原生后端无法推断该表达式的显示类型",
        )),
    }
}

pub(crate) fn call_str_cmp<M: Module>(c: &mut Compiler<M>, bcx: &mut FunctionBuilder, l: Value, r: Value) -> AliasResult<Value> {
    c.call_rt(bcx, "alias.str.cmp", &[types::I64, types::I64], Some(types::I32), &[l, r])
}

// ---------------------------------------------------------------------------
// 调用 / 内建 / 闭包创建 / 捕获扫描
// ---------------------------------------------------------------------------

pub(crate) fn emit_call<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    callee: &Expr,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    // 内建特判仅限裸 Ident 被调方 — 黄金记录冻结的分派规则
    if let Expr::Ident(name, _) = callee {
        if name == "increase" || name == "decrease" {
            return emit_incdec(c, bcx, frame, name, args, span);
        }
        if name == "println" || name == "print" {
            return emit_print(c, bcx, frame, name, args, span);
        }
        // 转换内建 (Phase 3a): 发射层实现 — f→int 越界中止, int→int wrapping
        if let Some(target) = conv_target_vty(name) {
            let [arg] = args else {
                invariant_violation("转换内建元数 (sema 已校验)")
            };
            let v = emit_expr(c, bcx, frame, &arg.value)?;
            let src = static_vty(c, frame, &arg.value);
            return emit_convert(c, bcx, span, v, &src, &target);
        }
        // typeof 内建 (Phase 3a): 求值取副作用, 返回静态类型名字面量块
        if name == "typeof" {
            let [arg] = args else {
                invariant_violation("typeof 元数 (sema 已校验)")
            };
            emit_expr(c, bcx, frame, &arg.value)?;
            let tn = static_vty(c, frame, &arg.value).display_name();
            return str_literal_handle(c, bcx, &tn);
        }
    }
    // 匿名立即调用: 字面量参数表即签名
    if let Expr::FuncLit { params, body, .. } = callee {
        let clo = emit_funclit_value(c, bcx, frame, params, body)?;
        let param_vtys: Vec<VTy> = params.iter().map(|p| decl_vty(&p.ty, &c.struct_layouts)).collect();
        let ret_vty = infer_ret_vty(c, frame, params, body);
        return call_closure(c, bcx, frame, clo, &param_vtys, &ret_vty, args);
    }
    let clo = match callee {
        Expr::Ident(name, _) => {
            // 结构体构造 (Phase 2a): 名字非绑定且已登记 — 与 sema 的
            // 分派规则逐点镜像 (遮蔽时走普通调用路径)
            if c.struct_layouts.contains_key(name) && cell_addr(c, frame, name).is_none() {
                return emit_construct(c, bcx, frame, name, args);
            }
            // result 构造器 (Phase 2b): 遮蔽镜像规则同上
            if (name == "ok" || name == "err") && cell_addr(c, frame, name).is_none() {
                return emit_result_ctor(c, bcx, frame, name, args);
            }
            match cell_addr(c, frame, name) {
                Some(addr) => read_cell(bcx, frame, &addr, &VTy::Func),
                None => {
                    return Err(native_err(
                        span,
                        format!("未定义的绑定 '{name}'"),
                    ))
                }
            }
        },
        _ => return Err(native_err(span, "函数值尚未接入原生后端 (Phase 3)")),
    };
    // 具名被调方: 签名已知 → 混合调用; 否则多态退化全字约定
    if let Expr::Ident(name, _) = callee {
        let known = c.fn_sig_by_name.get(name).cloned();
        return match known {
            Some((param_vtys, ret_vty)) => {
                call_closure(c, bcx, frame, clo, &param_vtys, &ret_vty, args)
            }
            None => {
                let code = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 0);
                let env = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 8);
                let mut words: Vec<Value> = Vec::with_capacity(args.len() + 2);
                words.push(bcx.use_var(frame.globals));
                words.push(env);
                for a in args {
                    words.push(emit_expr(c, bcx, frame, &a.value)?);
                }
                let sig_ref = bcx.func.import_signature(c.user_sig(args.len()));
                let inst = bcx.ins().call_indirect(sig_ref, code, &words);
                Ok(first_result(bcx, inst))
            }
        };
    }
    invariant_violation("被调方形态 (上方已分派)")
}

/// 经闭包对象的混合签名间接调用: globals/env 前导 + 实参按参数类型
/// 降档到 ABI 宽度; 返回值规范化到规范在途形。
fn call_closure<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    clo: Value,
    param_vtys: &[VTy],
    ret_vty: &VTy,
    args: &[CallArg],
) -> AliasResult<Value> {
    let mut words: Vec<Value> = Vec::with_capacity(args.len() + 2);
    for (a, pt) in args.iter().zip(param_vtys) {
        let v = emit_expr(c, bcx, frame, &a.value)?;
        words.push(norm_store(bcx, v, pt));
    }
    let code = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 0);
    let env = bcx.ins().load(types::I64, MemFlagsData::new(), clo, 8);
    words.insert(0, env);
    words.insert(0, bcx.use_var(frame.globals));
    let mut sig = Signature::new(c.cc);
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    for pt in param_vtys {
        sig.params.push(AbiParam::new(cl_type(pt)));
    }
    sig.returns.push(AbiParam::new(cl_type(ret_vty)));
    let sig_ref = bcx.func.import_signature(sig);
    let inst = bcx.ins().call_indirect(sig_ref, code, &words);
    let raw = first_result(bcx, inst);
    Ok(norm_load(bcx, raw, ret_vty))
}

/// 转换内建名 → 目标静态投影 (与 sema conv_builtin_ty 同名单集)
pub(crate) fn conv_target_vty(name: &str) -> Option<VTy> {
    let t = match name {
        "to_i8" => VTy::I(IntW::W8),
        "to_i16" => VTy::I(IntW::W16),
        "to_i32" => VTy::I(IntW::W32),
        "to_i64" => VTy::I(IntW::W64),
        "to_u8" => VTy::U(UIntW::U8),
        "to_u16" => VTy::U(UIntW::U16),
        "to_u32" => VTy::U(UIntW::U32),
        "to_u64" => VTy::U(UIntW::U64),
        "to_f32" => VTy::F(FloatW::F32),
        "to_f64" => VTy::F(FloatW::F64),
        _ => return None,
    };
    Some(t)
}

/// 转换发射 (裁决⑤): int→int 窄化 wrapping; int→float 精确 sitofp/uitofp;
/// float→float 升降档舍入; float→int 先域检查 — NaN 或截断后越界即
/// 「转换越界」span-ID 中止 (不用饱和变体 — 用户裁决拒绝静默饱和)。
fn emit_convert<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    span: Span,
    v: Value,
    src: &VTy,
    dst: &VTy,
) -> AliasResult<Value> {
    match dst {
        VTy::F(w) => {
            let t = cl_type(dst);
            let base = match src {
                VTy::I(_) => bcx.ins().fcvt_from_sint(t, v),
                VTy::U(_) => bcx.ins().fcvt_from_uint(t, v),
                VTy::F(_) => match w {
                    FloatW::F32 => match bcx.func.dfg.value_type(v) {
                        types::F64 => bcx.ins().fdemote(types::F32, v),
                        _ => v,
                    },
                    FloatW::F64 => match bcx.func.dfg.value_type(v) {
                        types::F32 => bcx.ins().fpromote(types::F64, v),
                        _ => v,
                    },
                },
                _ => invariant_violation("转换源为数值族 (sema 已校验)"),
            };
            Ok(base)
        }
        VTy::I(w) => {
            let bits = w.bits();
            let wt = ir_type_bits(bits);
            emit_convert_to_int(c, bcx, span, v, src, true, bits, wt)
        }
        VTy::U(w) => {
            let bits = w.bits();
            let wt = ir_type_bits(bits);
            emit_convert_to_int(c, bcx, span, v, src, false, bits, wt)
        }
        _ => invariant_violation("转换目标为数值族"),
    }
}

/// float→int 域检查 (f64 域进行, f32 先升档 — 比较常量精确) +
/// 截断转换; int→int 规范 I64 截断即 wrapping。
fn emit_convert_to_int<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    span: Span,
    v: Value,
    src: &VTy,
    signed: bool,
    bits: u32,
    wt: cranelift_codegen::ir::Type,
) -> AliasResult<Value> {
    use cranelift_codegen::ir::condcodes::FloatCC;
    if matches!(src, VTy::F(_)) {
        // 域检查在 f64 域进行 (f32 先升档 — 比较常量精确)
        let f64v = match bcx.func.dfg.value_type(v) {
            types::F32 => bcx.ins().fpromote(types::F64, v),
            _ => v,
        };
        let (lo, hi) = if signed {
            // max+1 = ±2^(k-1) 边界常量, f64 精确表示 (k ≤ 64)
            let lo = -(2f64).powi(bits as i32 - 1);
            let hi = (2f64).powi(bits as i32 - 1);
            (lo, hi)
        } else {
            let hi = (2f64).powi(bits as i32);
            (0.0f64, hi)
        };
        let nan = bcx.ins().fcmp(FloatCC::NotEqual, f64v, f64v);
        let lo_c = bcx.ins().f64const(lo);
        let hi_c = bcx.ins().f64const(hi);
        let below = bcx.ins().fcmp(FloatCC::LessThan, f64v, lo_c);
        let above = bcx.ins().fcmp(FloatCC::GreaterThanOrEqual, f64v, hi_c);
        let bad_lo = bcx.ins().bor(nan, below);
        let bad = bcx.ins().bor(bad_lo, above);
        emit_abort_branch(c, bcx, bad, "alias.abort_conv", span)?;
        let sat = if signed {
            bcx.ins().fcvt_to_sint(types::I64, f64v)
        } else {
            bcx.ins().fcvt_to_uint(types::I64, f64v)
        };
        let red = narrow(bcx, sat, bits);
        Ok(if signed {
            widen_signed(bcx, red, wt)
        } else {
            widen_unsigned(bcx, red, wt)
        })
    } else {
        // int→int: 规范 I64 截断到目标宽度即 wrapping (裁决⑤)
        let red = narrow(bcx, v, bits);
        Ok(if signed {
            widen_signed(bcx, red, wt)
        } else {
            widen_unsigned(bcx, red, wt)
        })
    }
}

/// 条件中止存根 (转换越界共用 div 的 span-ID 机制): trap 为真 → 调 abort 符号。
fn emit_abort_branch<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    trap: Value,
    sym: &str,
    span: Span,
) -> AliasResult<()> {
    let span_id = new_span_id(c, span);
    let abort_b = bcx.create_block();
    let ok_b = bcx.create_block();
    bcx.ins().brif(trap, abort_b, &[], ok_b, &[]);
    bcx.seal_block(abort_b);
    bcx.seal_block(ok_b);

    bcx.switch_to_block(abort_b);
    let aid = bcx.ins().iconst(types::I32, span_id as i64);
    let af = c.import_fn(sym, &[types::I32], None)?;
    let aref = c.module.declare_func_in_func(af, &mut bcx.func);
    bcx.ins().call(aref, &[aid]);
    bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO); // 不可达兜底

    bcx.switch_to_block(ok_b);
    Ok(())
}

/// 结构体构造发射: 泄漏定尺寸槽区 (alias.cell.new(bytes)), 字段按声明序
/// 求值写入各自偏移 (显式命名实参优先, 缺省取声明默认值), 值规范化到
/// 字段类型。全覆盖由 sema 保证。
pub(crate) fn emit_construct<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
) -> AliasResult<Value> {
    let layout = c.struct_layouts[name].clone();
    let bytes = bcx.ins().iconst(types::I64, layout.size as i64);
    let ptr = c.call_rt(bcx, "alias.cell.new", &[types::I64], Some(types::I64), &[bytes])?;
    for (fname, default, fvty, off) in &layout.fields {
        let expr = args
            .iter()
            .find(|a| a.label.as_deref() == Some(fname.as_str()))
            .map(|a| &a.value)
            .or_else(|| default.as_ref())
            .unwrap_or_else(|| invariant_violation("构造字段全覆盖 (sema 已校验)"));
        let v = emit_expr(c, bcx, frame, expr)?;
        let sv = norm_store(bcx, v, fvty);
        bcx.ins().store(MemFlagsData::new(), sv, ptr, *off);
    }
    Ok(ptr)
}

/// result 构造发射 (Phase 2b): 泄漏 2×8 块 {tag, payload} —
/// tag 0=ok / 1=err; 载荷经存储字通道落槽 (浮点位转换, 存储层重解释)。
pub(crate) fn emit_result_ctor<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
) -> AliasResult<Value> {
    let [arg] = args else {
        return Err(native_err(
            Span::default(),
            format!("{name} 构造恰好接受 1 个参数"),
        ));
    };
    let payload = emit_expr(c, bcx, frame, &arg.value)?;
    let pvty = static_vty(c, frame, &arg.value);
    let pw = storage_word(bcx, payload, &pvty);
    let n2 = bcx.ins().iconst(types::I32, 2);
    let blk = c.call_rt(bcx, "alias.env.new", &[types::I32], Some(types::I64), &[n2])?;
    let tag = if name == "ok" { 0i64 } else { 1i64 };
    let tagw = bcx.ins().iconst(types::I64, tag);
    bcx.ins().store(MemFlagsData::new(), tagw, blk, 0);
    bcx.ins().store(MemFlagsData::new(), pw, blk, 8);
    Ok(blk)
}

/// 方法调用发射 (Phase 2c): 接收者先求值 → 静态类型定接收者名 →
/// 用户方法直调 (统一约定 fn(globals, env, self, args...), env 传哑字 0 —
/// 方法无捕获, 自由名经 globals 可达); 内建字符串方法落运行时符号。
/// 接收者静态投影不可知属后端已知缺口 (sema 全知) — 编译期拒绝不 panic。
pub(crate) fn emit_method_call<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    recv: &Expr,
    name: &str,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    let rv = emit_expr(c, bcx, frame, recv)?;
    let svt = static_vty(c, frame, recv);
    // 数组内建三件套 (Phase 2d): len/push/pop 落运行时符号 (双后端同契约);
    // 元素步长按元素类型; pop 的空数组守卫在发射层 (span-ID 中止存根)
    if let VTy::Array(elem) = &svt {
        // 统一 8 字节步长 (P3a 布局简化裁决, 同 ArrayLit)
        return match name {
            "len" => {
                let t = c.call_rt(bcx, "alias.arr.len", &[types::I64], Some(types::I32), &[rv])?;
                Ok(bcx.ins().sextend(types::I64, t))
            }
            "push" => {
                let [a] = args else {
                    invariant_violation("push 元数 (sema 已校验)")
                };
                let v = emit_expr(c, bcx, frame, &a.value)?;
                let sw = storage_word(bcx, v, elem);
                c.call_rt(bcx, "alias.arr.push", &[types::I64, types::I64], None, &[rv, sw])?;
                Ok(bcx.ins().iconst(types::I64, 0))
            }
            "pop" => {
                let len = bcx.ins().load(types::I64, MemFlagsData::new(), rv, 8);
                let empty = bcx.ins().icmp_imm_s(IntCC::Equal, len, 0);
                let span_id = new_span_id(c, span);
                let abort_b = bcx.create_block();
                let ok_b = bcx.create_block();
                bcx.ins().brif(empty, abort_b, &[], ok_b, &[]);
                bcx.seal_block(abort_b);
                bcx.seal_block(ok_b);
                bcx.switch_to_block(abort_b);
                let aid = bcx.ins().iconst(types::I32, span_id as i64);
                let af = c.import_fn("alias.abort_pop", &[types::I32], None)?;
                let aref = c.module.declare_func_in_func(af, &mut bcx.func);
                bcx.ins().call(aref, &[aid]);
                bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO); // 不可达兜底
                bcx.switch_to_block(ok_b);
                let raw = c.call_rt(bcx, "alias.arr.pop", &[types::I64], Some(types::I64), &[rv])?;
                Ok(restore_word(bcx, raw, elem))
            }
            _ => invariant_violation("数组内建方法存在性 (sema 已校验)"),
        };
    }
    let tname = match &svt {
        VTy::Str => "string".to_string(),
        VTy::I(_) => "i32".to_string(),
        VTy::Bool => "bool".to_string(),
        VTy::Struct(s) => s.clone(),
        _ => {
            return Err(native_err(
                span,
                "原生后端无法推断该表达式的接收者类型",
            ))
        }
    };
    if let Some((param_vtys, ret_vty)) = c.method_sigs.get(&(tname.clone(), name.to_string())).cloned() {
        let fid = c.methods[&(tname, name.to_string())];
        let fref = c.module.declare_func_in_func(fid, &mut bcx.func);
        let mut words: Vec<Value> = Vec::with_capacity(args.len() + 3);
        words.push(bcx.use_var(frame.globals));
        words.push(bcx.ins().iconst(types::I64, 0));
        words.push(norm_store(bcx, rv, &param_vtys[0]));
        for (a, pt) in args.iter().zip(param_vtys.iter().skip(1)) {
            let v = emit_expr(c, bcx, frame, &a.value)?;
            words.push(norm_store(bcx, v, pt));
        }
        let inst = bcx.ins().call(fref, &words);
        let raw = first_result(bcx, inst);
        return Ok(norm_load(bcx, raw, &ret_vty));
    }
    if tname == "string" {
        match name {
            "len" => {
                let t = c.call_rt(bcx, "alias.str.len", &[types::I64], Some(types::I32), &[rv])?;
                return Ok(bcx.ins().sextend(types::I64, t));
            }
            "upper" => {
                return c.call_rt(bcx, "alias.str.upper", &[types::I64], Some(types::I64), &[rv])
            }
            "lower" => {
                return c.call_rt(bcx, "alias.str.lower", &[types::I64], Some(types::I64), &[rv])
            }
            "trim" => {
                return c.call_rt(bcx, "alias.str.trim", &[types::I64], Some(types::I64), &[rv])
            }
            _ => invariant_violation("字符串方法存在性 (sema 已校验)"),
        }
    }
    invariant_violation("方法存在性 (sema 已校验)")
}

/// 字段偏移回查: recv 静态类型给出结构体名 → 布局表定位字节偏移。
/// recv 非结构体/未知字段在 sema 已拒绝 — 违例即编译器不变式破坏。
pub(crate) fn field_offset<M: Module>(
    c: &Compiler<M>,
    frame: &Frame,
    recv: &Expr,
    field: &str,
) -> AliasResult<i32> {
    Ok(field_entry(c, frame, recv, field)?.1)
}

/// 字段静态类型回查 (规范化方向所需) — 与偏移同一布局表来源。
pub(crate) fn field_vty<M: Module>(
    c: &Compiler<M>,
    frame: &Frame,
    recv: &Expr,
    field: &str,
) -> AliasResult<VTy> {
    Ok(field_entry(c, frame, recv, field)?.0)
}

fn field_entry<M: Module>(
    c: &Compiler<M>,
    frame: &Frame,
    recv: &Expr,
    field: &str,
) -> AliasResult<(VTy, i32)> {
    if let VTy::Struct(s) = static_vty(c, frame, recv) {
        if let Some(layout) = c.struct_layouts.get(&s) {
            if let Some((_, _, fvty, off)) =
                layout.fields.iter().find(|(n, ..)| n == field)
            {
                return Ok((fvty.clone(), *off));
            }
        }
    }
    invariant_violation("字段访问目标为结构体实例 (sema 已校验)");
}

pub(crate) fn emit_incdec<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    let [arg] = args else {
        return Err(native_err(span, format!("{name} 恰好接受 1 个参数")));
    };
    // 非标识符实参不求值 — 黄金记录冻结的求值规则
    let Expr::Ident(target, tspan) = &arg.value else {
        return Err(native_err(span, format!("{name} 的参数必须是可变绑定名")));
    };
    let Some(addr) = cell_addr(c, frame, target) else {
        return Err(native_err(*tspan, format!("'{target}' 未定义")));
    };
    let cur = read_cell(bcx, frame, &addr, &VTy::I(IntW::W32));
    let delta = if name == "increase" { 1i64 } else { -1i64 };
    let cur32 = bcx.ins().ireduce(types::I32, cur);
    let next = bcx.ins().iadd_imm_s(cur32, delta);
    let nextw = bcx.ins().sextend(types::I64, next);
    write_cell(bcx, frame, &addr, nextw, &VTy::I(IntW::W32));
    Ok(bcx.ins().iconst(types::I64, 0))
}

pub(crate) fn emit_print<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    name: &str,
    args: &[CallArg],
    span: Span,
) -> AliasResult<Value> {
    let [arg] = args else {
        return Err(native_err(span, format!("{name} 恰好接受 1 个参数")));
    };
    let v = emit_expr(c, bcx, frame, &arg.value)?;
    match static_vty(c, frame, &arg.value) {
        // i32/u8/u16 走快速通道 (值域非负或 i32 原生); 其余经 display 成块
        VTy::I(IntW::W32) | VTy::U(UIntW::U8) | VTy::U(UIntW::U16) => {
            let t = bcx.ins().ireduce(types::I32, v);
            let h = if name == "println" { "alias.println.i32" } else { "alias.print.i32" };
            c.call_rt(bcx, h, &[types::I32], None, &[t])?;
        }
        // Str/Unit/Func/Struct/Result/Array/其余数值族均经 display 成块后走字符串通道
        _ => {
            let s = display_word(c, bcx, frame, &arg.value, v)?;
            let h = if name == "println" { "alias.println.str" } else { "alias.print.str" };
            c.call_rt(bcx, h, &[types::I64], None, &[s])?;
        }
    }
    Ok(bcx.ins().iconst(types::I64, 0))
}

/// span-ID 登记: 守卫点行:列入表, 中止存根按 ID 回查 (div/越界/pop 共用)。
pub(crate) fn new_span_id<M: Module>(c: &mut Compiler<M>, span: Span) -> i32 {
    c.span_table.push((span.line, span.col));
    c.span_table.len() as i32 - 1
}

/// 除法守卫泛化 (Phase 3a): 有符号各宽 = 除零 OR (除数 -1 AND 被除数
/// MIN_width); 无符号仅除零 (无 MIN/-1 溢出形态)。中止存根 span-ID
/// 回查原始行:列; 商在声明宽度内产出。
pub(crate) fn emit_div_guard<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    l: Value,
    r: Value,
    signed: bool,
    bits: u32,
    span: Span,
) -> AliasResult<Value> {
    let span_id = new_span_id(c, span);
    let wt = ir_type_bits(bits);
    let zero = bcx.ins().iconst(wt, 0);
    let by_zero = bcx.ins().icmp(IntCC::Equal, r.clone(), zero);
    let trap = if signed {
        let m1 = bcx.ins().iconst(wt, -1);
        let mini = bcx.ins().iconst(wt, match bits {
            8 => i8::MIN as i64,
            16 => i16::MIN as i64,
            32 => i32::MIN as i64,
            _ => i64::MIN,
        });
        let by_m1 = bcx.ins().icmp(IntCC::Equal, r, m1);
        let is_min = bcx.ins().icmp(IntCC::Equal, l.clone(), mini);
        let m1_min = bcx.ins().band(by_m1, is_min);
        bcx.ins().bor(by_zero, m1_min)
    } else {
        by_zero
    };

    let abort_b = bcx.create_block();
    let ok_b = bcx.create_block();
    bcx.ins().brif(trap, abort_b, &[], ok_b, &[]);
    bcx.seal_block(abort_b);
    bcx.seal_block(ok_b);

    bcx.switch_to_block(abort_b);
    let aid = bcx.ins().iconst(types::I32, span_id as i64);
    let af = c.import_fn("alias.abort_div", &[types::I32], None)?;
    let aref = c.module.declare_func_in_func(af, &mut bcx.func);
    bcx.ins().call(aref, &[aid]); // 运行时侧 process exit(1)/ExitProcess, 不返回
    // 块终结 + 不可达兜底: 正常控制流永不至此, 若抵达则主动中止
    bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);

    bcx.switch_to_block(ok_b);
    Ok(if signed { bcx.ins().sdiv(l, r) } else { bcx.ins().udiv(l, r) })
}

/// 下标越界守卫 → 中止存根 (Phase 2d): i < 0 或 i >= len 即中止,
/// 与 div 守卫同一 span-ID 机制; 正常路径返回元素字。
fn emit_index_guard<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    idx32: Value,
    len32: Value,
    span: Span,
) -> AliasResult<()> {
    let span_id = new_span_id(c, span);
    let zero = bcx.ins().iconst(types::I32, 0);
    let neg = bcx.ins().icmp(IntCC::SignedLessThan, idx32, zero);
    let oob_hi = bcx.ins().icmp(IntCC::SignedGreaterThanOrEqual, idx32, len32);
    let trap = bcx.ins().bor(neg, oob_hi);

    let abort_b = bcx.create_block();
    let ok_b = bcx.create_block();
    bcx.ins().brif(trap, abort_b, &[], ok_b, &[]);
    bcx.seal_block(abort_b);
    bcx.seal_block(ok_b);

    bcx.switch_to_block(abort_b);
    let aid = bcx.ins().iconst(types::I32, span_id as i64);
    let af = c.import_fn("alias.abort_oob", &[types::I32], None)?;
    let aref = c.module.declare_func_in_func(af, &mut bcx.func);
    bcx.ins().call(aref, &[aid]);
    bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO); // 不可达兜底

    bcx.switch_to_block(ok_b);
    Ok(())
}
