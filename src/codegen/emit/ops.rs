use super::strings::{call_str_cmp, display_typed};
use crate::codegen::abi::{cl_type, ir_type_bits, VTy};
use crate::codegen::{invariant_violation, Compiler, Frame};
use crate::sema::hir::{BinOp, Expr};
use crate::sema::types::FloatW;
use crate::{AliasResult, Span};
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, InstBuilder, TrapCode, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;

pub(crate) fn emit_binary<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    input: (BinOp, &Expr, Value, Value, Span),
) -> AliasResult<Value> {
    let (op, lhs, l, r, span) = input;
    let lt = c.vty(lhs.ty());
    emit_binary_values(c, bcx, frame, (op, &lt, l, r, span))
}

pub(crate) fn emit_binary_values<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    input: (BinOp, &VTy, Value, Value, Span),
) -> AliasResult<Value> {
    let (op, lt, l, r, span) = input;
    use BinOp::*;
    match op {
        Add | Sub | Mul | Div | Rem => match lt {
            VTy::F(_) => match op {
                Add => Ok(bcx.ins().fadd(l, r)),
                Sub => Ok(bcx.ins().fsub(l, r)),
                Mul => Ok(bcx.ins().fmul(l, r)),
                Div => Ok(bcx.ins().fdiv(l, r)),
                Rem => invariant_violation("浮点余数已被 sema 拒绝"),
                _ => unreachable!(),
            },
            VTy::I(w) => {
                let wt = cl_type(&VTy::I(*w));
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Add | Sub | Mul => {
                        emit_checked_int_binary(c, bcx, frame, (op, li, ri, true, span))?
                    }
                    Div => emit_divrem_guard(c, bcx, frame, (li, ri, true, w.bits(), span, false))?,
                    Rem => emit_divrem_guard(c, bcx, frame, (li, ri, true, w.bits(), span, true))?,
                    _ => unreachable!(),
                };
                Ok(widen_signed(bcx, v, wt))
            }
            VTy::U(w) => {
                let wt = cl_type(&VTy::U(*w));
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Add | Sub | Mul => {
                        emit_checked_int_binary(c, bcx, frame, (op, li, ri, false, span))?
                    }
                    Div => {
                        emit_divrem_guard(c, bcx, frame, (li, ri, false, w.bits(), span, false))?
                    }
                    Rem => emit_divrem_guard(c, bcx, frame, (li, ri, false, w.bits(), span, true))?,
                    _ => unreachable!(),
                };
                Ok(widen_unsigned(bcx, v, wt))
            }
            _ => invariant_violation("算术操作数为数值族 (sema 已校验)"),
        },
        BitAnd | BitXor | BitOr => match lt {
            VTy::I(w) => {
                let wt = cl_type(lt);
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    BitAnd => bcx.ins().band(li, ri),
                    BitXor => bcx.ins().bxor(li, ri),
                    BitOr => bcx.ins().bor(li, ri),
                    _ => unreachable!(),
                };
                Ok(widen_signed(bcx, v, wt))
            }
            VTy::U(w) => {
                let wt = cl_type(lt);
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    BitAnd => bcx.ins().band(li, ri),
                    BitXor => bcx.ins().bxor(li, ri),
                    BitOr => bcx.ins().bor(li, ri),
                    _ => unreachable!(),
                };
                Ok(widen_unsigned(bcx, v, wt))
            }
            _ => invariant_violation("位运算操作数为整数 (sema 已校验)"),
        },
        Shl | Shr => match lt {
            VTy::I(w) => {
                let wt = cl_type(lt);
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Shl => emit_checked_shl(c, bcx, frame, (li, ri, true, w.bits(), span))?,
                    Shr => bcx.ins().sshr(li, ri),
                    _ => unreachable!(),
                };
                Ok(widen_signed(bcx, v, wt))
            }
            VTy::U(w) => {
                let wt = cl_type(lt);
                let li = narrow(bcx, l, w.bits());
                let ri = narrow(bcx, r, w.bits());
                let v = match op {
                    Shl => emit_checked_shl(c, bcx, frame, (li, ri, false, w.bits(), span))?,
                    Shr => bcx.ins().ushr(li, ri),
                    _ => unreachable!(),
                };
                Ok(widen_unsigned(bcx, v, wt))
            }
            _ => invariant_violation("移位操作数为整数 (sema 已校验)"),
        },
        Lt | Le | Gt | Ge | EqEq | NotEq => {
            use cranelift_codegen::ir::condcodes::FloatCC;
            let b = match lt {
                VTy::Str => {
                    let ord = call_str_cmp(c, bcx, l, r)?;
                    bcx.ins().icmp_imm_s(int_cc(op, true), ord, 0)
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
                _ => bcx.ins().icmp(int_cc(op, true), l, r),
            };
            Ok(bcx.ins().uextend(types::I64, b))
        }
        And | Or => invariant_violation("短路逻辑运算由 emit_short_circuit 发射"),
    }
}

fn emit_checked_int_binary<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    input: (BinOp, Value, Value, bool, Span),
) -> AliasResult<Value> {
    let (op, l, r, signed, span) = input;
    let (result, overflow) = match (signed, op) {
        (true, BinOp::Add) => bcx.ins().sadd_overflow(l, r),
        (true, BinOp::Sub) => bcx.ins().ssub_overflow(l, r),
        (true, BinOp::Mul) => bcx.ins().smul_overflow(l, r),
        (false, BinOp::Add) => bcx.ins().uadd_overflow(l, r),
        (false, BinOp::Sub) => bcx.ins().usub_overflow(l, r),
        (false, BinOp::Mul) => bcx.ins().umul_overflow(l, r),
        _ => invariant_violation("checked 整数算术仅用于加减乘"),
    };
    emit_abort_branch(c, bcx, frame, overflow, "alias.abort_overflow", span)?;
    Ok(result)
}

fn emit_checked_shl<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    input: (Value, Value, bool, u32, Span),
) -> AliasResult<Value> {
    let (value, shift, signed, bits, span) = input;
    let count_bad = bcx
        .ins()
        .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, shift, bits as i64);
    let result = bcx.ins().ishl(value, shift);
    let restored = if signed {
        bcx.ins().sshr(result, shift)
    } else {
        bcx.ins().ushr(result, shift)
    };
    let lost = bcx.ins().icmp(IntCC::NotEqual, restored, value);
    let overflow = bcx.ins().bor(count_bad, lost);
    emit_abort_branch(c, bcx, frame, overflow, "alias.abort_overflow", span)?;
    Ok(result)
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
        _ => invariant_violation("比较谓词仅用于比较运算符"),
    }
}

pub(crate) fn narrow(bcx: &mut FunctionBuilder, v: Value, bits: u32) -> Value {
    let ty = ir_type_bits(bits);
    if ty == types::I64 {
        v
    } else {
        bcx.ins().ireduce(ty, v)
    }
}

pub(crate) fn widen_signed(
    bcx: &mut FunctionBuilder,
    v: Value,
    to: cranelift_codegen::ir::Type,
) -> Value {
    if to == types::I64 {
        v
    } else {
        bcx.ins().sextend(types::I64, v)
    }
}

pub(crate) fn widen_unsigned(
    bcx: &mut FunctionBuilder,
    v: Value,
    to: cranelift_codegen::ir::Type,
) -> Value {
    if to == types::I64 {
        v
    } else {
        bcx.ins().uextend(types::I64, v)
    }
}

pub(crate) fn emit_convert<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    span: Span,
    v: Value,
    src: &VTy,
    dst: &VTy,
) -> AliasResult<Value> {
    match dst {
        VTy::Str => display_typed(c, bcx, src, v, span),
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
            emit_convert_to_int(c, bcx, frame, (span, v, src, true, bits, wt))
        }
        VTy::U(w) => {
            let bits = w.bits();
            let wt = ir_type_bits(bits);
            emit_convert_to_int(c, bcx, frame, (span, v, src, false, bits, wt))
        }
        _ => invariant_violation("转换目标为数值族或 string"),
    }
}

fn emit_convert_to_int<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    input: (Span, Value, &VTy, bool, u32, cranelift_codegen::ir::Type),
) -> AliasResult<Value> {
    let (span, v, src, signed, bits, wt) = input;
    use cranelift_codegen::ir::condcodes::FloatCC;
    if matches!(src, VTy::F(_)) {
        let f64v = match bcx.func.dfg.value_type(v) {
            types::F32 => bcx.ins().fpromote(types::F64, v),
            _ => v,
        };
        let (lo, hi) = if signed {
            (-(2f64).powi(bits as i32 - 1), (2f64).powi(bits as i32 - 1))
        } else {
            (0.0f64, (2f64).powi(bits as i32))
        };
        let nan = bcx.ins().fcmp(FloatCC::NotEqual, f64v, f64v);
        let lo_c = bcx.ins().f64const(lo);
        let hi_c = bcx.ins().f64const(hi);
        let below = bcx.ins().fcmp(FloatCC::LessThan, f64v, lo_c);
        let above = bcx.ins().fcmp(FloatCC::GreaterThanOrEqual, f64v, hi_c);
        let bad_lo = bcx.ins().bor(nan, below);
        let bad = bcx.ins().bor(bad_lo, above);
        emit_abort_branch(c, bcx, frame, bad, "alias.abort_conv", span)?;
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
        let no = bcx.ins().iconst(types::I8, 0);
        let bad = match src {
            VTy::I(source_w) if signed => {
                if bits >= source_w.bits() {
                    no
                } else {
                    let min = -(1i128 << (bits - 1)) as i64;
                    let max = ((1u128 << (bits - 1)) - 1) as i64;
                    let below = bcx.ins().icmp_imm_s(IntCC::SignedLessThan, v, min);
                    let above = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, v, max);
                    bcx.ins().bor(below, above)
                }
            }
            VTy::I(_) => {
                let negative = bcx.ins().icmp_imm_s(IntCC::SignedLessThan, v, 0);
                if bits == 64 {
                    negative
                } else {
                    let max = ((1u128 << bits) - 1) as i64;
                    let above = bcx.ins().icmp_imm_s(IntCC::SignedGreaterThan, v, max);
                    bcx.ins().bor(negative, above)
                }
            }
            VTy::U(_) if signed => {
                let max = ((1u128 << (bits - 1)) - 1) as u64 as i64;
                bcx.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, v, max)
            }
            VTy::U(source_w) => {
                if bits >= source_w.bits() {
                    no
                } else {
                    let max = ((1u128 << bits) - 1) as u64 as i64;
                    bcx.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, v, max)
                }
            }
            _ => invariant_violation("整数转换源为整数 (sema 已校验)"),
        };
        emit_abort_branch(c, bcx, frame, bad, "alias.abort_conv", span)?;
        let red = narrow(bcx, v, bits);
        Ok(if signed {
            widen_signed(bcx, red, wt)
        } else {
            widen_unsigned(bcx, red, wt)
        })
    }
}

pub(crate) fn emit_abort_branch<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    _frame: &Frame,
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
    c.call_rt_void(bcx, sym, &[aid])?;
    bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);

    bcx.switch_to_block(ok_b);
    Ok(())
}

pub(crate) fn new_span_id<M: Module>(c: &mut Compiler<M>, span: Span) -> i32 {
    c.span_table.push((span.line, span.col));
    c.span_table.len() as i32 - 1
}

pub(crate) fn emit_divrem_guard<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    input: (Value, Value, bool, u32, Span, bool),
) -> AliasResult<Value> {
    let (l, r, signed, bits, span, remainder) = input;
    let wt = ir_type_bits(bits);
    let zero = bcx.ins().iconst(wt, 0);
    let by_zero = bcx.ins().icmp(IntCC::Equal, r, zero);
    emit_abort_branch(c, bcx, frame, by_zero, "alias.abort_div", span)?;
    if signed {
        let m1 = bcx.ins().iconst(wt, -1);
        let mini = bcx.ins().iconst(
            wt,
            match bits {
                8 => i8::MIN as i64,
                16 => i16::MIN as i64,
                32 => i32::MIN as i64,
                _ => i64::MIN,
            },
        );
        let by_m1 = bcx.ins().icmp(IntCC::Equal, r, m1);
        let is_min = bcx.ins().icmp(IntCC::Equal, l, mini);
        let overflow = bcx.ins().band(by_m1, is_min);
        emit_abort_branch(c, bcx, frame, overflow, "alias.abort_overflow", span)?;
    }
    Ok(match (signed, remainder) {
        (true, false) => bcx.ins().sdiv(l, r),
        (false, false) => bcx.ins().udiv(l, r),
        (true, true) => bcx.ins().srem(l, r),
        (false, true) => bcx.ins().urem(l, r),
    })
}

pub(crate) fn emit_index_guard<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    _frame: &Frame,
    idx32: Value,
    len32: Value,
    span: Span,
) -> AliasResult<()> {
    let span_id = new_span_id(c, span);
    let zero = bcx.ins().iconst(types::I32, 0);
    let neg = bcx.ins().icmp(IntCC::SignedLessThan, idx32, zero);
    let oob_hi = bcx
        .ins()
        .icmp(IntCC::SignedGreaterThanOrEqual, idx32, len32);
    let trap = bcx.ins().bor(neg, oob_hi);

    let abort_b = bcx.create_block();
    let ok_b = bcx.create_block();
    bcx.ins().brif(trap, abort_b, &[], ok_b, &[]);
    bcx.seal_block(abort_b);
    bcx.seal_block(ok_b);

    bcx.switch_to_block(abort_b);
    let aid = bcx.ins().iconst(types::I32, span_id as i64);
    c.call_rt_void(bcx, "alias.abort_oob", &[aid])?;
    bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);

    bcx.switch_to_block(ok_b);
    Ok(())
}
