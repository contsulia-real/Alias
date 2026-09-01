//! Alias 值 ABI 与内存布局的唯一真相源。

use crate::sema::hir::{CheckedProgram, Expr, Item};
use crate::sema::types::{FloatW, IntW, ReturnEffect, Ty, UIntW};
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlagsData, Signature, Type, Value};
use cranelift_frontend::FunctionBuilder;
use std::collections::HashMap;

/// array/result/env 载荷统一使用一个 64-bit word；所有元素步长必须引用此 owner。
pub(crate) const VALUE_WORD_BYTES: i64 = 8;

pub(crate) const fn value_word_offset(index: usize) -> i32 {
    (index as i64 * VALUE_WORD_BYTES) as i32
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum VTy {
    I(IntW),
    U(UIntW),
    F(FloatW),
    Bool,
    Str,
    Unit,
    Func(Vec<VTy>, Box<VTy>),
    /// Machine-level return lane for a semantic borrowed result. The pointee type remains
    /// available for validation, but the caller/callee ABI carries the referent address as I64.
    Borrowed(Box<VTy>),
    FuncPoly,
    Struct(String),
    Result(Box<VTy>, Box<VTy>),
    Array(Box<VTy>),
    Iterator(Box<VTy>),
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WordRepr {
    Signed,
    Unsigned,
    F32Bits,
    F64Bits,
    Direct,
}

/// 一个静态值跨 Cranelift 表达式、内存/调用边界和通用 word 容器时的物理合同。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ValueAbi {
    /// 后端表达式内部的规范寄存器类型；窄整数统一提升到 I64，减少算术路径分叉。
    pub(crate) register: Type,
    /// cell/struct field 等真实存储宽度。
    pub(crate) storage: Type,
    pub(crate) storage_bytes: usize,
    pub(crate) align_bytes: usize,
    /// 用户函数机器签名仍保留声明宽度，不能把内部 I64 规范化泄漏到 ABI。
    pub(crate) param: Type,
    pub(crate) ret: Type,
    /// 写入 array/result/env 等 64-bit 通用槽时采用的编码。
    pub(crate) word: WordRepr,
}

impl VTy {
    pub(crate) fn abi(&self) -> ValueAbi {
        match self {
            VTy::I(w) => integer_abi(w.bits(), true),
            VTy::U(w) => integer_abi(w.bits(), false),
            VTy::F(FloatW::F32) => ValueAbi {
                register: types::F32,
                storage: types::F32,
                storage_bytes: 4,
                align_bytes: 4,
                param: types::F32,
                ret: types::F32,
                word: WordRepr::F32Bits,
            },
            VTy::F(FloatW::F64) => ValueAbi {
                register: types::F64,
                storage: types::F64,
                storage_bytes: 8,
                align_bytes: 8,
                param: types::F64,
                ret: types::F64,
                word: WordRepr::F64Bits,
            },
            VTy::Unit => panic!("内部 ABI 不变式被破坏: unit 没有值 ABI"),
            VTy::Unknown => panic!("内部 ABI 不变式被破坏: 未确定类型没有值 ABI"),
            VTy::Bool
            | VTy::Str
            | VTy::Func(..)
            | VTy::Borrowed(_)
            | VTy::FuncPoly
            | VTy::Struct(_)
            | VTy::Result(..)
            | VTy::Array(_)
            | VTy::Iterator(_) => ValueAbi {
                register: types::I64,
                storage: types::I64,
                storage_bytes: 8,
                align_bytes: 8,
                param: types::I64,
                ret: types::I64,
                word: WordRepr::Direct,
            },
        }
    }

    pub(crate) fn is_numeric(&self) -> bool {
        matches!(self, VTy::I(_) | VTy::U(_) | VTy::F(_))
    }
}

fn integer_abi(bits: u32, signed: bool) -> ValueAbi {
    let storage = ir_type_bits(bits);
    // 算术/比较发射统一处理 I64 形式，但 memory 与函数边界必须保持源码声明宽度；
    // 否则 i8/i16/i32 的布局和 Windows 调用签名会被内部规范化意外扩大。
    ValueAbi {
        register: types::I64,
        storage,
        storage_bytes: (bits / 8) as usize,
        align_bytes: (bits / 8) as usize,
        param: storage,
        ret: storage,
        word: if signed {
            WordRepr::Signed
        } else {
            WordRepr::Unsigned
        },
    }
}

pub(crate) fn ir_type_bits(bits: u32) -> Type {
    match bits {
        8 => types::I8,
        16 => types::I16,
        32 => types::I32,
        64 => types::I64,
        _ => panic!("内部 ABI 不变式被破坏: 非法整数宽度 {bits}"),
    }
}

pub(crate) fn cl_type(vty: &VTy) -> Type {
    vty.abi().storage
}

pub(crate) fn size_align(vty: &VTy) -> (usize, usize) {
    let abi = vty.abi();
    (abi.storage_bytes, abi.align_bytes)
}

pub(crate) fn align_to(off: usize, align: usize) -> usize {
    off.div_ceil(align) * align
}

pub(crate) fn user_signature(
    cc: cranelift_codegen::isa::CallConv,
    params: &[VTy],
    ret: &VTy,
) -> Signature {
    let mut sig = Signature::new(cc);
    // 所有用户函数统一先接收 globals 与 closure env 两个隐藏 I64 参数；直接函数、
    // 闭包和方法调用都依赖这一固定前缀，任何一侧自行增删都会造成 call ABI 错位。
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params
        .extend(params.iter().map(|p| AbiParam::new(p.abi().param)));
    if *ret != VTy::Unit {
        sig.returns.push(AbiParam::new(ret.abi().ret));
    }
    sig
}

#[derive(Clone, Debug)]
pub(crate) struct StructFieldLayout {
    pub(crate) default: Option<Expr>,
    pub(crate) vty: VTy,
    pub(crate) offset: i32,
}

#[derive(Clone, Debug)]
pub(crate) struct StructLayout {
    pub(crate) fields: Vec<StructFieldLayout>,
    pub(crate) size: i32,
    pub(crate) align: usize,
}

pub(crate) type StructTable = HashMap<String, StructLayout>;
pub(crate) type ProjectionTable = HashMap<Ty, VTy>;

pub(crate) fn project_ty(program: &CheckedProgram) -> ProjectionTable {
    let mut table = ProjectionTable::new();
    program.for_each_ty(&mut |ty| insert_projection(ty, &mut table));
    table
}

fn insert_projection(ty: &Ty, table: &mut ProjectionTable) {
    if table.contains_key(ty) {
        return;
    }
    let projected = match ty {
        Ty::Int(width) => VTy::I(*width),
        Ty::UInt(width) => VTy::U(*width),
        Ty::Float(width) => VTy::F(*width),
        Ty::Bool => VTy::Bool,
        Ty::Str => VTy::Str,
        Ty::Unit => VTy::Unit,
        Ty::Func {
            params,
            ret,
            return_effect,
            ..
        } => {
            for param in params {
                insert_projection(param, table);
            }
            insert_projection(ret, table);
            let return_vty = table[ret.as_ref()].clone();
            let return_vty = if matches!(return_effect, Some(ReturnEffect::Borrowed(_))) {
                VTy::Borrowed(Box::new(return_vty))
            } else {
                return_vty
            };
            VTy::Func(
                params.iter().map(|param| table[param].clone()).collect(),
                Box::new(return_vty),
            )
        }
        Ty::FuncPoly => VTy::FuncPoly,
        Ty::Struct(name) => VTy::Struct(name.clone()),
        Ty::Result(ok, err) => {
            insert_projection(ok, table);
            insert_projection(err, table);
            VTy::Result(
                Box::new(table[ok.as_ref()].clone()),
                Box::new(table[err.as_ref()].clone()),
            )
        }
        Ty::Array(element) => {
            insert_projection(element, table);
            VTy::Array(Box::new(table[element.as_ref()].clone()))
        }
        Ty::Iterator(element) => {
            insert_projection(element, table);
            VTy::Iterator(Box::new(table[element.as_ref()].clone()))
        }
        Ty::Unknown => VTy::Unknown,
    };
    table.insert(ty.clone(), projected);
}

pub(crate) fn projected_ty(table: &ProjectionTable, ty: &Ty) -> VTy {
    table
        .get(ty)
        .cloned()
        .unwrap_or_else(|| panic!("内部类型投影不变式被破坏: 缺少 {}", ty.name()))
}

pub(crate) fn build_struct_layouts(items: &[Item], projections: &ProjectionTable) -> StructTable {
    let mut table = StructTable::new();
    for item in items {
        let Item::StructDef(sd) = item else { continue };
        let mut fields = Vec::with_capacity(sd.fields.len());
        let mut off = 0usize;
        let mut max_align = 1usize;
        for field in &sd.fields {
            let vty = projected_ty(projections, &field.ty);
            let abi = vty.abi();
            // 每个字段先对齐自身，再推进实际存储宽度；struct 值本身是引用 word，
            // 因此无需预注册其它 struct 的 inline layout。
            off = align_to(off, abi.align_bytes);
            max_align = max_align.max(abi.align_bytes);
            fields.push(StructFieldLayout {
                default: field.default.clone(),
                vty,
                offset: off as i32,
            });
            off += abi.storage_bytes;
        }
        // 尾部 padding 使数组/连续对象中的下一个 struct 仍满足本 struct 最大对齐要求。
        table.insert(
            sd.name.clone(),
            StructLayout {
                fields,
                size: align_to(off, max_align) as i32,
                align: max_align,
            },
        );
    }
    table
}

pub(crate) fn norm_load(bcx: &mut FunctionBuilder, raw: Value, vty: &VTy) -> Value {
    let abi = vty.abi();
    // 从真实 storage 边界进入表达式世界时恢复规范寄存器形式；窄有符号/无符号
    // 必须分别扩展，否则高位语义会在后续 I64 算术和比较中被破坏。
    if abi.storage == abi.register {
        raw
    } else if matches!(vty, VTy::I(_)) {
        bcx.ins().sextend(abi.register, raw)
    } else if matches!(vty, VTy::U(_)) {
        bcx.ins().uextend(abi.register, raw)
    } else {
        raw
    }
}

pub(crate) fn norm_store(bcx: &mut FunctionBuilder, value: Value, vty: &VTy) -> Value {
    let abi = vty.abi();
    let actual = bcx.func.dfg.value_type(value);
    if actual == abi.storage {
        return value;
    }
    // 写回 cell/field/参数边界前恢复声明宽度；float promotion/demotion 只处理真实
    // F32/F64 数值，不与通用 word 的 bit encoding 混用。
    match vty {
        VTy::I(_) | VTy::U(_) => bcx.ins().ireduce(abi.storage, value),
        VTy::F(FloatW::F32) if actual == types::F64 => bcx.ins().fdemote(types::F32, value),
        VTy::F(FloatW::F64) if actual == types::F32 => bcx.ins().fpromote(types::F64, value),
        _ => value,
    }
}

pub(crate) fn storage_word(bcx: &mut FunctionBuilder, value: Value, vty: &VTy) -> Value {
    let abi = vty.abi();
    // 通用容器永远只有一个 I64 word。整数按符号规范扩展；float 必须保存 IEEE bit
    // pattern，而不是执行数值转换，否则 result/array/env 往返后值会改变。
    match abi.word {
        WordRepr::F32Bits => {
            let bits = bcx.ins().bitcast(types::I32, MemFlagsData::new(), value);
            bcx.ins().uextend(types::I64, bits)
        }
        WordRepr::F64Bits => bcx.ins().bitcast(types::I64, MemFlagsData::new(), value),
        WordRepr::Signed | WordRepr::Unsigned if abi.storage != types::I64 => {
            let reduced = bcx.ins().ireduce(abi.storage, value);
            if abi.word == WordRepr::Signed {
                bcx.ins().sextend(types::I64, reduced)
            } else {
                bcx.ins().uextend(types::I64, reduced)
            }
        }
        _ => value,
    }
}

pub(crate) fn restore_word(bcx: &mut FunctionBuilder, raw: Value, vty: &VTy) -> Value {
    // 整数 word 在 storage_word 时已经规范扩展，可直接作为表达式 I64；只有 float
    // 需要把保存的 bit pattern 重解释回真实浮点寄存器类型。
    match vty.abi().word {
        WordRepr::F32Bits => {
            let bits = bcx.ins().ireduce(types::I32, raw);
            bcx.ins().bitcast(types::F32, MemFlagsData::new(), bits)
        }
        WordRepr::F64Bits => bcx.ins().bitcast(types::F64, MemFlagsData::new(), raw),
        _ => raw,
    }
}

pub(crate) fn store_elem(bcx: &mut FunctionBuilder, word: Value, addr: Value, elem_vty: &VTy) {
    let abi = elem_vty.abi();
    // array backing store 按元素真实 storage 宽度写入，而 array runtime 的槽步长仍由
    // VALUE_WORD_BYTES 管理；因此写入前必须把通用 word 恢复成元素 storage 表示。
    let value = match abi.word {
        WordRepr::F32Bits => {
            let bits = bcx.ins().ireduce(types::I32, word);
            bcx.ins().bitcast(types::F32, MemFlagsData::new(), bits)
        }
        WordRepr::F64Bits => bcx.ins().bitcast(types::F64, MemFlagsData::new(), word),
        WordRepr::Signed | WordRepr::Unsigned if abi.storage != types::I64 => {
            bcx.ins().ireduce(abi.storage, word)
        }
        _ => word,
    };
    bcx.ins().store(MemFlagsData::new(), value, addr, 0);
}

#[cfg(test)]
mod tests {
    use super::{
        build_struct_layouts, insert_projection, project_ty, projected_ty, user_signature,
        ProjectionTable, VTy,
    };
    use crate::sema::types::{FloatW, IntW, ReturnBorrowSource, ReturnEffect, Ty, UIntW};

    #[test]
    fn primitive_abi_matrix_is_complete_and_consistent() {
        let values = [
            VTy::I(IntW::W8),
            VTy::I(IntW::W16),
            VTy::I(IntW::W32),
            VTy::I(IntW::W64),
            VTy::U(UIntW::U8),
            VTy::U(UIntW::U16),
            VTy::U(UIntW::U32),
            VTy::U(UIntW::U64),
            VTy::F(FloatW::F32),
            VTy::F(FloatW::F64),
            VTy::Bool,
            VTy::Str,
            VTy::Struct("s".into()),
            VTy::Result(Box::new(VTy::I(IntW::W32)), Box::new(VTy::Str)),
            VTy::Array(Box::new(VTy::I(IntW::W8))),
            VTy::Iterator(Box::new(VTy::I(IntW::W8))),
        ];
        for value in values {
            let abi = value.abi();
            assert!(abi.storage_bytes > 0);
            assert!(abi.align_bytes.is_power_of_two());
            assert_eq!(abi.storage_bytes, abi.storage.bytes() as usize);
            assert_eq!(abi.param, abi.storage);
            assert_eq!(abi.ret, abi.storage);
        }
    }

    #[test]
    fn semantic_type_projection_is_total_and_exact() {
        let cases = vec![
            (Ty::Int(IntW::W8), VTy::I(IntW::W8)),
            (Ty::Int(IntW::W16), VTy::I(IntW::W16)),
            (Ty::Int(IntW::W32), VTy::I(IntW::W32)),
            (Ty::Int(IntW::W64), VTy::I(IntW::W64)),
            (Ty::UInt(UIntW::U8), VTy::U(UIntW::U8)),
            (Ty::UInt(UIntW::U16), VTy::U(UIntW::U16)),
            (Ty::UInt(UIntW::U32), VTy::U(UIntW::U32)),
            (Ty::UInt(UIntW::U64), VTy::U(UIntW::U64)),
            (Ty::Float(FloatW::F32), VTy::F(FloatW::F32)),
            (Ty::Float(FloatW::F64), VTy::F(FloatW::F64)),
            (Ty::Bool, VTy::Bool),
            (Ty::Str, VTy::Str),
            (Ty::Unit, VTy::Unit),
            (
                Ty::Func {
                    params: vec![Ty::Int(IntW::W32), Ty::Str],
                    param_effects: None,
                    return_effect: None,
                    ret: Box::new(Ty::Bool),
                },
                VTy::Func(vec![VTy::I(IntW::W32), VTy::Str], Box::new(VTy::Bool)),
            ),
            (
                Ty::Func {
                    params: Vec::new(),
                    param_effects: Some(Vec::new()),
                    return_effect: Some(ReturnEffect::Borrowed(ReturnBorrowSource::Global(
                        crate::sema::types::BindingId(7),
                    ))),
                    ret: Box::new(Ty::Int(IntW::W32)),
                },
                VTy::Func(
                    Vec::new(),
                    Box::new(VTy::Borrowed(Box::new(VTy::I(IntW::W32)))),
                ),
            ),
            (Ty::FuncPoly, VTy::FuncPoly),
            (Ty::Struct("point".into()), VTy::Struct("point".into())),
            (
                Ty::Result(Box::new(Ty::Int(IntW::W16)), Box::new(Ty::Str)),
                VTy::Result(Box::new(VTy::I(IntW::W16)), Box::new(VTy::Str)),
            ),
            (
                Ty::Array(Box::new(Ty::UInt(UIntW::U64))),
                VTy::Array(Box::new(VTy::U(UIntW::U64))),
            ),
            (
                Ty::Iterator(Box::new(Ty::Float(FloatW::F32))),
                VTy::Iterator(Box::new(VTy::F(FloatW::F32))),
            ),
            (Ty::Unknown, VTy::Unknown),
        ];
        for (semantic, projected) in cases {
            let mut table = ProjectionTable::new();
            insert_projection(&semantic, &mut table);
            assert_eq!(projected_ty(&table, &semantic), projected);
        }
    }

    #[test]
    fn unit_user_signature_has_no_return_value() {
        let unit = user_signature(
            cranelift_codegen::isa::CallConv::WindowsFastcall,
            &[],
            &VTy::Unit,
        );
        assert!(unit.returns.is_empty());

        let value = user_signature(
            cranelift_codegen::isa::CallConv::WindowsFastcall,
            &[],
            &VTy::I(IntW::W32),
        );
        assert_eq!(value.returns.len(), 1);
    }

    #[test]
    #[should_panic(expected = "未确定类型没有值 ABI")]
    fn unknown_type_never_silently_falls_back_to_a_machine_word() {
        let _ = VTy::Unknown.abi();
    }

    #[test]
    fn struct_layout_includes_internal_and_tail_padding() {
        let src = "struct mixed { val i8 a = 1 val f64 b = 2.0 val i16 c = 3 }\nfunc i32 main = () -> return 0\n";
        let tokens = crate::lexer::lex(src).unwrap();
        let program = crate::parser::parse(tokens).unwrap();
        let checked = crate::sema::check(program).unwrap();
        let projections = project_ty(&checked);
        let layouts = build_struct_layouts(&checked.items, &projections);
        let layout = &layouts["mixed"];
        assert_eq!(layout.align, 8);
        assert_eq!(layout.size, 24);
        assert_eq!(
            layout.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
            [0, 8, 16]
        );
    }
}
