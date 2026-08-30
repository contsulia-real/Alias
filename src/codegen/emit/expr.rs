use super::arrays::{checked_array_element_addr, emit_array_lit};
use super::calls::{emit_call, emit_method_call};
use super::cells::{
    binding_storage_addr, coerce_ret, emit_local_cell, ensure_current, pop_scope, push_scope,
};
use super::clone::emit_deep_clone_place;
use super::control::emit_stmt;
use super::ops::{
    emit_abort_branch, emit_binary, emit_convert, narrow, widen_signed, widen_unsigned,
};
use super::places::{emit_place_addr, emit_place_value, field_storage};
use super::strings::{call_str_cmp, emit_str, str_literal_handle};
use crate::codegen::abi::{cl_type, norm_load, norm_store, restore_word, storage_word, VTy};
use crate::codegen::funcgen::emit_funclit_value;
use crate::codegen::layout::{
    result_tag, RESULT_ERR_TAG, RESULT_PAYLOAD_OFFSET, RESULT_TAG_OFFSET,
};
use crate::codegen::{bound_vty, invariant_violation, native_err, Compiler, Frame};
use crate::sema::hir::{
    ArmBody, BinOp, CtorKind, Expr, MatchArm, Pattern, ResolvedConversion, Stmt,
};
use crate::sema::types::FloatW;
use crate::AliasResult;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{Block, BlockArg, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;

pub(crate) fn emit_expr<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    e: &Expr,
) -> AliasResult<Value> {
    match e {
        Expr::Int(n, ..) => Ok(bcx.ins().iconst(types::I64, *n as i64)),
        Expr::Float(v, ..) => match c.vty(e.ty()) {
            VTy::F(FloatW::F32) => Ok(bcx.ins().f32const(*v as f32)),
            VTy::F(FloatW::F64) => Ok(bcx.ins().f64const(*v)),
            _ => invariant_violation("浮点字面量携带浮点静态类型"),
        },
        Expr::Bool(b, ..) => Ok(bcx.ins().iconst(types::I64, *b as i64)),
        Expr::Str(parts, ..) => emit_str(c, bcx, frame, parts),
        Expr::Ident(name, id, span, _) => {
            let id = id.unwrap_or_else(|| {
                panic!("内部代码生成不变式被破坏: 可求值标识符 '{name}' 缺少 BindingId")
            });
            match binding_storage_addr(c, bcx, frame, id) {
                Some(addr) => {
                    let vty = bound_vty(c, frame, id);
                    let raw = bcx.ins().load(cl_type(&vty), MemFlagsData::new(), addr, 0);
                    Ok(norm_load(bcx, raw, &vty))
                }
                None => Err(native_err(
                    *span,
                    format!("内部: BindingId {:?} 无存储", id),
                )),
            }
        }
        Expr::This(span, _) => {
            let fid = frame
                .this_fid
                .ok_or_else(|| native_err(*span, "this 只能出现在 func 体内"))?;
            let fref = c.module.declare_func_in_func(fid, bcx.func);
            let code = bcx.ins().func_addr(c.ptr_ty, fref);
            let env = frame
                .env
                .map(|var| bcx.use_var(var))
                .unwrap_or_else(|| bcx.ins().iconst(types::I64, 0));
            c.call_rt(bcx, "alias.closure.new", &[code, env])
        }
        Expr::Move { source, .. } => {
            emit_place_value(c, bcx, frame, source).map(|(value, _)| value)
        }
        Expr::Borrow { source, .. } => {
            emit_place_addr(c, bcx, frame, source).map(|(address, _)| address)
        }
        Expr::ReadPlace { source, plan, .. } => emit_deep_clone_place(c, bcx, frame, source, plan),
        Expr::Cast { expr, span, .. } => {
            let dst = c.vty(e.ty());
            let src = c.vty(expr.ty());
            let value = emit_expr(c, bcx, frame, expr)?;
            emit_convert(c, bcx, *span, value, &src, &dst)
        }
        Expr::Convert {
            expr, mode, span, ..
        } => match mode {
            ResolvedConversion::Identity => emit_expr(c, bcx, frame, expr),
            ResolvedConversion::Convert => {
                let src = c.vty(expr.ty());
                let dst = c.vty(e.ty());
                let value = emit_expr(c, bcx, frame, expr)?;
                emit_convert(c, bcx, *span, value, &src, &dst)
            }
        },
        Expr::Typeof { type_name, .. } => str_literal_handle(c, bcx, type_name),
        Expr::Neg { expr, span, .. } => {
            // 负整数字面量已按最终 HIR 类型完成范围检查；必须直接发射其有符号位模式。
            // 若复用运行期 unary-neg overflow 检查，每种整数宽度的 INT_MIN 字面量都会
            // 因“对正 magnitude 取负”而被错误拒绝。
            if let Expr::Int(magnitude, ..) = expr.as_ref() {
                if matches!(c.vty(e.ty()), VTy::I(_)) {
                    return Ok(bcx
                        .ins()
                        .iconst(types::I64, 0u64.wrapping_sub(*magnitude) as i64));
                }
            }
            let v = emit_expr(c, bcx, frame, expr)?;
            let t = c.vty(expr.ty());
            match t {
                VTy::I(w) => {
                    let wt = cl_type(&VTy::I(w));
                    let red = narrow(bcx, v, w.bits());
                    let min = bcx.ins().iconst(
                        wt,
                        match w.bits() {
                            8 => i8::MIN as i64,
                            16 => i16::MIN as i64,
                            32 => i32::MIN as i64,
                            _ => i64::MIN,
                        },
                    );
                    let overflow = bcx.ins().icmp(IntCC::Equal, red, min);
                    emit_abort_branch(c, bcx, overflow, "alias.abort_overflow", *span)?;
                    let n = bcx.ins().ineg(red);
                    Ok(widen_signed(bcx, n, wt))
                }
                VTy::F(_) => Ok(bcx.ins().fneg(v)),
                _ => invariant_violation("取负操作数为有符号整数或浮点 (sema 已校验)"),
            }
        }
        Expr::Not { expr, .. } => {
            let v = emit_expr(c, bcx, frame, expr)?;
            Ok(emit_bool_not(bcx, v))
        }
        Expr::BitNot { expr, .. } => {
            let vty = c.vty(e.ty());
            let v = emit_expr(c, bcx, frame, expr)?;
            emit_bit_not_typed(bcx, v, &vty)
        }
        Expr::Binary {
            op: BinOp::And,
            lhs,
            rhs,
            ..
        } => emit_short_circuit(c, bcx, frame, false, lhs, rhs),
        Expr::Binary {
            op: BinOp::Or,
            lhs,
            rhs,
            ..
        } => emit_short_circuit(c, bcx, frame, true, lhs, rhs),
        Expr::Binary {
            op, lhs, rhs, span, ..
        } => {
            let l = emit_expr(c, bcx, frame, lhs)?;
            let r = emit_expr(c, bcx, frame, rhs)?;
            emit_binary(c, bcx, (*op, lhs, l, r, *span))
        }
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
            ..
        } => {
            let result = c.vty(e.ty());
            emit_ternary(c, bcx, frame, cond, then_expr, else_expr, &result)
        }
        Expr::Call {
            callee,
            args,
            target,
            span,
            ..
        } => emit_call(c, bcx, frame, callee, args, target, *span),
        Expr::MethodCall {
            recv,
            args,
            target,
            span,
            ..
        } => emit_method_call(c, bcx, frame, recv, args, target, *span),
        Expr::Field {
            recv, field_index, ..
        } => {
            let p = emit_expr(c, bcx, frame, recv)?;
            let (fvty, offset) = field_storage(c, recv.ty(), *field_index)?;
            let raw = bcx
                .ins()
                .load(cl_type(&fvty), MemFlagsData::new(), p, offset);
            Ok(norm_load(bcx, raw, &fvty))
        }
        Expr::Index {
            recv, idx, span, ..
        } => {
            let array = emit_expr(c, bcx, frame, recv)?;
            let idxw = emit_expr(c, bcx, frame, idx)?;
            let elem_vty = match c.vty(recv.ty()) {
                VTy::Array(inner) => (*inner).clone(),
                _ => invariant_violation("下标主语为 array (sema 已校验)"),
            };
            let addr = checked_array_element_addr(c, bcx, array, idxw, *span)?;
            let raw = bcx
                .ins()
                .load(cl_type(&elem_vty), MemFlagsData::new(), addr, 0);
            Ok(norm_load(bcx, raw, &elem_vty))
        }
        Expr::ArrayLit { elems, .. } => {
            let VTy::Array(elem_vty) = c.vty(e.ty()) else {
                invariant_violation("数组字面量携带 array 类型")
            };
            emit_array_lit(c, bcx, frame, elems, &elem_vty)
        }
        Expr::FuncLit {
            params,
            body,
            captures,
            ..
        } => emit_funclit_value(c, bcx, frame, params, body, captures, e.ty()),
        Expr::Match { subject, arms, .. } => {
            let result_vty = c.vty(e.ty());
            emit_match(c, bcx, frame, subject, arms, &result_vty)
        }
        Expr::Propagate { expr, .. } => {
            let subj = emit_expr(c, bcx, frame, expr)?;
            let tag = bcx
                .ins()
                .load(types::I64, MemFlagsData::new(), subj, RESULT_TAG_OFFSET);
            let is_err = bcx.ins().icmp_imm_s(IntCC::Equal, tag, RESULT_ERR_TAG);
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
            let pvty = match c.vty(expr.ty()) {
                VTy::Result(t, _) => *t,
                _ => invariant_violation("? 操作数携带 result 类型"),
            };
            let raw = bcx
                .ins()
                .load(types::I64, MemFlagsData::new(), subj, RESULT_PAYLOAD_OFFSET);
            Ok(restore_word(bcx, raw, &pvty))
        }
    }
}

fn emit_bool_not(bcx: &mut FunctionBuilder, v: Value) -> Value {
    let b = bcx.ins().icmp_imm_s(IntCC::Equal, v, 0);
    bcx.ins().uextend(types::I64, b)
}

fn emit_bit_not_typed(bcx: &mut FunctionBuilder, v: Value, vty: &VTy) -> AliasResult<Value> {
    match vty {
        VTy::I(w) => {
            let wt = cl_type(vty);
            let red = narrow(bcx, v, w.bits());
            let n = bcx.ins().bnot(red);
            Ok(widen_signed(bcx, n, wt))
        }
        VTy::U(w) => {
            let wt = cl_type(vty);
            let red = narrow(bcx, v, w.bits());
            let n = bcx.ins().bnot(red);
            Ok(widen_unsigned(bcx, n, wt))
        }
        _ => invariant_violation("位非操作数为整数 (sema 已校验)"),
    }
}

fn emit_short_circuit<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    is_or: bool,
    lhs: &Expr,
    rhs: &Expr,
) -> AliasResult<Value> {
    let l = emit_expr(c, bcx, frame, lhs)?;
    let rhs_b = bcx.create_block();
    let join_b = bcx.create_block();
    let out = bcx.append_block_param(join_b, types::I64);
    let short = bcx.ins().iconst(types::I64, if is_or { 1 } else { 0 });
    if is_or {
        bcx.ins()
            .brif(l, join_b, &[BlockArg::Value(short)], rhs_b, &[]);
    } else {
        bcx.ins()
            .brif(l, rhs_b, &[], join_b, &[BlockArg::Value(short)]);
    }
    bcx.seal_block(rhs_b);
    bcx.switch_to_block(rhs_b);
    let r = emit_expr(c, bcx, frame, rhs)?;
    bcx.ins().jump(join_b, &[BlockArg::Value(r)]);
    bcx.seal_block(join_b);
    bcx.switch_to_block(join_b);
    frame.terminated = false;
    Ok(out)
}

fn emit_ternary<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    cond: &Expr,
    then_expr: &Expr,
    else_expr: &Expr,
    result_vty: &VTy,
) -> AliasResult<Value> {
    let cv = emit_expr(c, bcx, frame, cond)?;
    let then_b = bcx.create_block();
    let else_b = bcx.create_block();
    let join_b = bcx.create_block();
    let out = bcx.append_block_param(join_b, cl_type(result_vty));
    bcx.ins().brif(cv, then_b, &[], else_b, &[]);
    bcx.seal_block(then_b);
    bcx.seal_block(else_b);

    bcx.switch_to_block(then_b);
    frame.terminated = false;
    let a = emit_expr(c, bcx, frame, then_expr)?;
    if !frame.terminated {
        let a = norm_store(bcx, a, result_vty);
        bcx.ins().jump(join_b, &[BlockArg::Value(a)]);
    }

    bcx.switch_to_block(else_b);
    frame.terminated = false;
    let b = emit_expr(c, bcx, frame, else_expr)?;
    if !frame.terminated {
        let b = norm_store(bcx, b, result_vty);
        bcx.ins().jump(join_b, &[BlockArg::Value(b)]);
    }

    bcx.seal_block(join_b);
    bcx.switch_to_block(join_b);
    frame.terminated = false;
    Ok(norm_load(bcx, out, result_vty))
}

fn emit_match<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    subject: &Expr,
    arms: &[MatchArm],
    result_vty: &VTy,
) -> AliasResult<Value> {
    let subject_vty = c.vty(subject.ty());
    let subj = emit_expr(c, bcx, frame, subject)?;
    let join_b = bcx.create_block();
    let jv = if *result_vty == VTy::Unit {
        None
    } else {
        Some(bcx.append_block_param(join_b, types::I64))
    };
    let mut any_join = false;

    for (idx, arm) in arms.iter().enumerate() {
        let arm_b = bcx.create_block();
        let last = idx + 1 == arms.len();
        let next_b = if last { None } else { Some(bcx.create_block()) };

        if let Some(next_b) = next_b {
            let matched = emit_pattern_test(c, bcx, &arm.pattern, subj)?;
            bcx.ins().brif(matched, arm_b, &[], next_b, &[]);
            frame.terminated = true;
            bcx.seal_block(arm_b);
            bcx.seal_block(next_b);
        } else {
            bcx.ins().jump(arm_b, &[]);
            frame.terminated = true;
            bcx.seal_block(arm_b);
        }

        bcx.switch_to_block(arm_b);
        frame.terminated = false;
        any_join |= emit_match_arm(c, bcx, frame, (arm, &subject_vty, result_vty, subj, join_b))?;

        if let Some(next_b) = next_b {
            bcx.switch_to_block(next_b);
            frame.terminated = false;
        }
    }

    if any_join {
        bcx.seal_block(join_b);
        bcx.switch_to_block(join_b);
        frame.terminated = false;
        Ok(match jv {
            Some(value) => restore_word(bcx, value, result_vty),
            None => subj,
        })
    } else {
        Ok(subj)
    }
}

fn emit_pattern_test<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    pattern: &Pattern,
    subj: Value,
) -> AliasResult<Value> {
    Ok(match pattern {
        Pattern::Wildcard { .. } | Pattern::Binding { .. } => bcx.ins().iconst(types::I64, 1),
        Pattern::Int { value, .. } => {
            let rhs = bcx.ins().iconst(types::I64, *value as i64);
            bcx.ins().icmp(IntCC::Equal, subj, rhs)
        }
        Pattern::Bool { value, .. } => bcx.ins().icmp_imm_s(IntCC::Equal, subj, *value as i64),
        Pattern::Str { value, .. } => {
            let rhs = str_literal_handle(c, bcx, value)?;
            let ord = call_str_cmp(c, bcx, subj, rhs)?;
            bcx.ins().icmp_imm_s(IntCC::Equal, ord, 0)
        }
        Pattern::Constructor { ctor, .. } => {
            let tag = bcx
                .ins()
                .load(types::I64, MemFlagsData::new(), subj, RESULT_TAG_OFFSET);
            bcx.ins().icmp_imm_s(IntCC::Equal, tag, result_tag(*ctor))
        }
    })
}

pub(crate) fn emit_match_arm<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    input: (&MatchArm, &VTy, &VTy, Value, Block),
) -> AliasResult<bool> {
    let (arm, subject_vty, result_vty, subj, join_b) = input;
    push_scope(frame);

    match (&arm.pattern, subject_vty, arm.binding_id) {
        (Pattern::Binding { .. }, _, Some(binding_id)) => {
            emit_local_cell(
                c,
                bcx,
                frame,
                subj,
                subject_vty.clone(),
                binding_id,
                Some(crate::sema::hir::StorageRelation::Owning),
            )?;
        }
        (
            Pattern::Constructor {
                ctor,
                binding: Some(_),
                ..
            },
            VTy::Result(ok, err),
            Some(binding_id),
        ) => {
            let bind_vty = match ctor {
                CtorKind::Ok => (**ok).clone(),
                CtorKind::Err => (**err).clone(),
            };
            let raw = bcx
                .ins()
                .load(types::I64, MemFlagsData::new(), subj, RESULT_PAYLOAD_OFFSET);
            let payload = restore_word(bcx, raw, &bind_vty);
            emit_local_cell(
                c,
                bcx,
                frame,
                payload,
                bind_vty,
                binding_id,
                Some(crate::sema::hir::StorageRelation::Owning),
            )?;
        }
        (Pattern::Binding { .. }, _, None)
        | (
            Pattern::Constructor {
                binding: Some(_), ..
            },
            _,
            None,
        ) => invariant_violation("Pattern 绑定必须携带 BindingId"),
        _ => {}
    }

    let joined = match &arm.body {
        ArmBody::Value(e) => {
            let v = emit_expr(c, bcx, frame, e)?;
            if *result_vty == VTy::Unit {
                bcx.ins().jump(join_b, &[]);
            } else {
                let word = storage_word(bcx, v, result_vty);
                bcx.ins().jump(join_b, &[BlockArg::Value(word)]);
            }
            frame.terminated = true;
            true
        }
        ArmBody::Ret(e) => {
            let v = emit_expr(c, bcx, frame, e)?;
            let v = coerce_ret(bcx, frame, v);
            let rb = frame
                .ret_block
                .unwrap_or_else(|| invariant_violation("never 臂仅在函数体内可达 (sema 已校验)"));
            bcx.ins().jump(rb, &[BlockArg::Value(v)]);
            frame.terminated = true;
            false
        }
        ArmBody::Block(stmts) => {
            let rb = frame.ret_block.unwrap_or_else(|| {
                invariant_violation("臂内 return 仅在函数体内可达 (sema 已校验)")
            });
            let n = stmts.len();
            let mut tail: Option<Value> = None;
            for (i, s) in stmts.iter().enumerate() {
                ensure_current(bcx, frame);
                if i + 1 == n {
                    if let Stmt::Expr { expr } = s {
                        tail = Some(emit_expr(c, bcx, frame, expr)?);
                        continue;
                    }
                }
                emit_stmt(c, bcx, frame, s, rb)?;
            }
            if frame.terminated {
                false
            } else {
                if *result_vty == VTy::Unit {
                    bcx.ins().jump(join_b, &[]);
                } else {
                    let v = tail.unwrap_or_else(|| {
                        invariant_violation("产值 match 块必须具有尾表达式 (sema 已校验)")
                    });
                    let word = storage_word(bcx, v, result_vty);
                    bcx.ins().jump(join_b, &[BlockArg::Value(word)]);
                }
                frame.terminated = true;
                true
            }
        }
    };
    pop_scope(frame);
    Ok(joined)
}
