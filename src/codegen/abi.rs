//! Alias 值 ABI 与内存布局的唯一真相源。

use crate::ast::{Expr, Item, TypeExpr};
use crate::sema::types::{FloatW, IntW, UIntW};
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlagsData, Signature, Type, Value};
use cranelift_frontend::FunctionBuilder;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum VTy {
    I(IntW),
    U(UIntW),
    F(FloatW),
    Bool,
    Str,
    Unit,
    Func(Vec<VTy>, Box<VTy>),
    Struct(String),
    Result(String, String),
    Array(Box<VTy>),
    Iterator(Box<VTy>),
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WordRepr {
    Signed,
    Unsigned,
    F32Bits,
    F64Bits,
    Direct,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ValueAbi {
    pub(crate) register: Type,
    pub(crate) storage: Type,
    pub(crate) storage_bytes: usize,
    pub(crate) align_bytes: usize,
    pub(crate) param: Type,
    pub(crate) ret: Type,
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
            VTy::Bool
            | VTy::Str
            | VTy::Unit
            | VTy::Func(..)
            | VTy::Struct(_)
            | VTy::Result(..)
            | VTy::Array(_)
            | VTy::Iterator(_)
            | VTy::Other => ValueAbi {
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

    pub(crate) fn display_name(&self) -> String {
        match self {
            VTy::I(w) => match w {
                IntW::W8 => "i8".into(),
                IntW::W16 => "i16".into(),
                IntW::W32 => "i32".into(),
                IntW::W64 => "i64".into(),
            },
            VTy::U(w) => match w {
                UIntW::U8 => "u8".into(),
                UIntW::U16 => "u16".into(),
                UIntW::U32 => "u32".into(),
                UIntW::U64 => "u64".into(),
            },
            VTy::F(w) => match w {
                FloatW::F32 => "f32".into(),
                FloatW::F64 => "f64".into(),
            },
            VTy::Bool => "bool".into(),
            VTy::Str => "string".into(),
            VTy::Unit => "unit".into(),
            VTy::Func(..) => "func".into(),
            VTy::Struct(s) => s.clone(),
            VTy::Result(t, e) => format!("result<{t}, {e}>"),
            VTy::Array(t) => format!("array<{}>", t.display_name()),
            VTy::Iterator(t) => format!("iterator<{}>", t.display_name()),
            VTy::Other => "未知".into(),
        }
    }

    pub(crate) fn is_numeric(&self) -> bool {
        matches!(self, VTy::I(_) | VTy::U(_) | VTy::F(_))
    }
}

fn integer_abi(bits: u32, signed: bool) -> ValueAbi {
    let storage = ir_type_bits(bits);
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
    sig.params.push(AbiParam::new(types::I64));
    sig.params.push(AbiParam::new(types::I64));
    sig.params
        .extend(params.iter().map(|p| AbiParam::new(p.abi().param)));
    sig.returns.push(AbiParam::new(ret.abi().ret));
    sig
}

#[derive(Clone, Debug)]
pub(crate) struct StructFieldLayout {
    pub(crate) name: String,
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

pub(crate) fn build_struct_layouts(items: &[Item]) -> StructTable {
    let mut table = StructTable::new();
    for item in items {
        if let Item::StructDef(sd) = item {
            table.insert(
                sd.name.clone(),
                StructLayout {
                    fields: Vec::new(),
                    size: 0,
                    align: 1,
                },
            );
        }
    }
    for item in items {
        let Item::StructDef(sd) = item else { continue };
        let mut fields = Vec::with_capacity(sd.fields.len());
        let mut off = 0usize;
        let mut max_align = 1usize;
        for field in &sd.fields {
            let vty = decl_vty(&field.ty, &table);
            let abi = vty.abi();
            off = align_to(off, abi.align_bytes);
            max_align = max_align.max(abi.align_bytes);
            fields.push(StructFieldLayout {
                name: field.name.clone(),
                default: field.default.clone(),
                vty,
                offset: off as i32,
            });
            off += abi.storage_bytes;
        }
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

pub(crate) fn decl_vty(te: &TypeExpr, structs: &StructTable) -> VTy {
    match te {
        TypeExpr::Named(n) => vty_of_type_name(structs, n),
        TypeExpr::Generic(n, args) if n == "result" && args.len() == 2 => {
            VTy::Result(args[0].display(), args[1].display())
        }
        TypeExpr::Generic(n, args) if n == "array" && args.len() == 1 => {
            VTy::Array(Box::new(decl_vty(&args[0], structs)))
        }
        TypeExpr::Generic(n, args) if n == "iterator" && args.len() == 1 => {
            VTy::Iterator(Box::new(decl_vty(&args[0], structs)))
        }
        TypeExpr::Generic(..) => VTy::Other,
    }
}

pub(crate) fn vty_of_type_name(structs: &StructTable, name: &str) -> VTy {
    let name = name.trim();
    if let Some(inner) = generic_inner(name, "array") {
        return VTy::Array(Box::new(vty_of_type_name(structs, inner)));
    }
    if let Some(inner) = generic_inner(name, "iterator") {
        return VTy::Iterator(Box::new(vty_of_type_name(structs, inner)));
    }
    if let Some(inner) = generic_inner(name, "result") {
        if let Some((a, b)) = split_top_level_pair(inner) {
            return VTy::Result(a.trim().to_string(), b.trim().to_string());
        }
    }
    match name {
        "i8" => VTy::I(IntW::W8),
        "i16" => VTy::I(IntW::W16),
        "i32" => VTy::I(IntW::W32),
        "i64" => VTy::I(IntW::W64),
        "u8" => VTy::U(UIntW::U8),
        "u16" => VTy::U(UIntW::U16),
        "u32" => VTy::U(UIntW::U32),
        "u64" => VTy::U(UIntW::U64),
        "f32" => VTy::F(FloatW::F32),
        "f64" => VTy::F(FloatW::F64),
        "bool" => VTy::Bool,
        "string" => VTy::Str,
        "unit" => VTy::Unit,
        // 当前函数签名类型尚未最终定稿；物理上仍是闭包指针，显示/方法键为 func。
        "func" => VTy::Func(Vec::new(), Box::new(VTy::Other)),
        _ if structs.contains_key(name) => VTy::Struct(name.to_string()),
        _ => VTy::Other,
    }
}

fn generic_inner<'a>(name: &'a str, ctor: &str) -> Option<&'a str> {
    name.strip_prefix(ctor)?
        .strip_prefix('<')?
        .strip_suffix('>')
}

fn split_top_level_pair(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => return Some((&s[..i], &s[i + 1..])),
            _ => {}
        }
    }
    None
}

pub(crate) fn norm_load(bcx: &mut FunctionBuilder, raw: Value, vty: &VTy) -> Value {
    let abi = vty.abi();
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
    match vty {
        VTy::I(_) | VTy::U(_) => bcx.ins().ireduce(abi.storage, value),
        VTy::F(FloatW::F32) if actual == types::F64 => bcx.ins().fdemote(types::F32, value),
        VTy::F(FloatW::F64) if actual == types::F32 => bcx.ins().fpromote(types::F64, value),
        _ => value,
    }
}

pub(crate) fn storage_word(bcx: &mut FunctionBuilder, value: Value, vty: &VTy) -> Value {
    let abi = vty.abi();
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
    use super::*;

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
            VTy::Unit,
            VTy::Struct("s".into()),
            VTy::Result("i32".into(), "string".into()),
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
    fn struct_layout_includes_internal_and_tail_padding() {
        let src = "struct mixed { val i8 a = 1 val f64 b = 2.0 val i16 c = 3 }\nfunc i32 main = () -> return 0\n";
        let tokens = crate::lexer::lex(src).unwrap();
        let program = crate::parser::parse(tokens).unwrap();
        let layouts = build_struct_layouts(&program.items);
        let layout = &layouts["mixed"];
        assert_eq!(layout.align, 8);
        assert_eq!(layout.size, 24);
        assert_eq!(
            layout.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
            [0, 8, 16]
        );
    }
}
