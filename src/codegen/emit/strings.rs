use super::expr::emit_expr;
use crate::codegen::abi::VTy;
use crate::codegen::layout::RESULT_TAG_OFFSET;
use crate::codegen::{native_err, Compiler, Frame};
use crate::sema::hir::{Expr, StrPart};
use crate::sema::types::{FloatW, IntW, UIntW};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{Linkage, Module};

pub(crate) fn emit_str<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    parts: &[StrPart],
) -> AliasResult<Value> {
    let z8 = bcx.ins().iconst(types::I64, 0);
    let z4 = bcx.ins().iconst(types::I32, 0);
    let empty = c.call_rt(bcx, "alias.str.new", &[z8, z4])?;
    let mut acc = empty;
    for p in parts {
        let piece = match p {
            StrPart::Lit(s) => str_literal_handle(c, bcx, s)?,
            StrPart::Hole(h) => {
                // sema 已在 hole HIR 中固化 contextual conversion；插值后端这里只显示
                // 最终值。若再次按目标 string 推断，会在 codegen 制造第二套转换语义。
                let value = emit_expr(c, bcx, frame, h)?;
                display_word(c, bcx, h, value)?
            }
        };
        acc = c.call_rt(bcx, "alias.str.concat", &[acc, piece])?;
    }
    Ok(acc)
}

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
    let gv = c.module.declare_data_in_func(data_id, bcx.func);
    let addr = bcx.ins().symbol_value(c.machine_ptr_ty, gv);
    let len = bcx.ins().iconst(types::I32, s.len() as i64);
    c.call_rt(bcx, "alias.str.new", &[addr, len])
}

pub(crate) fn display_word<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    e: &Expr,
    w: Value,
) -> AliasResult<Value> {
    let vty = c.vty(e.ty());
    display_typed(c, bcx, &vty, w, e.span())
}

pub(crate) fn display_typed<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    vty: &VTy,
    w: Value,
    span: Span,
) -> AliasResult<Value> {
    match vty {
        VTy::I(IntW::W64) => c.call_rt(bcx, "alias.display.i64", &[w]),
        VTy::I(_) => {
            let t = bcx.ins().ireduce(types::I32, w);
            c.call_rt(bcx, "alias.display.int", &[t])
        }
        VTy::U(UIntW::U8) | VTy::U(UIntW::U16) => {
            let t = bcx.ins().ireduce(types::I32, w);
            c.call_rt(bcx, "alias.display.int", &[t])
        }
        VTy::U(_) => c.call_rt(bcx, "alias.display.u64", &[w]),
        VTy::F(FloatW::F32) => c.call_rt(bcx, "alias.display.f32", &[w]),
        VTy::F(FloatW::F64) => c.call_rt(bcx, "alias.display.f64", &[w]),
        VTy::Bool => {
            let t = bcx.ins().ireduce(types::I32, w);
            c.call_rt(bcx, "alias.display.bool", &[t])
        }
        VTy::Str => c.call_rt(bcx, "alias.display.str", &[w]),
        VTy::Unit => Err(native_err(span, "内部: unit 无返回值表达式进入 display")),
        VTy::Borrowed(_) => Err(native_err(
            span,
            "内部: borrowed return ABI lane 不能作为语言值进入 display",
        )),
        VTy::Func(..) | VTy::FuncPoly => c.call_rt(bcx, "alias.display.func", &[]),
        VTy::Struct(_) => c.call_rt(bcx, "alias.display.struct", &[]),
        VTy::Array(_) => c.call_rt(bcx, "alias.display.array", &[]),
        VTy::Iterator(_) => str_literal_handle(c, bcx, "<iterator>"),
        VTy::Result(..) => {
            let tag = bcx
                .ins()
                .load(types::I64, MemFlagsData::new(), w, RESULT_TAG_OFFSET);
            let t = bcx.ins().ireduce(types::I32, tag);
            c.call_rt(bcx, "alias.display.result", &[t])
        }
        VTy::Unknown => Err(native_err(span, "原生后端无法发射未确定类型的显示值")),
    }
}

pub(crate) fn call_str_cmp<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    l: Value,
    r: Value,
) -> AliasResult<Value> {
    c.call_rt(bcx, "alias.str.cmp", &[l, r])
}
