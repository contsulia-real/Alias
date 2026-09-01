//! Alias 值 ABI 与内存布局的唯一真相源。

use crate::sema::hir::{CheckedProgram, Expr, Item};
use crate::sema::types::{FloatW, IntW, ReturnEffect, Ty, UIntW};
use crate::{AliasError, AliasResult, Span};
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Type, Value};
use cranelift_frontend::FunctionBuilder;
use std::collections::HashMap;

/// 当前目标 heap object header 与 env/capture slot 的固定 machine-word 宽度。
/// 它不描述任意 Alias value；array/result payload 必须消费各自 typed ValueLayout。
pub(crate) const OBJECT_WORD_BYTES: i64 = 8;

pub(crate) const fn object_word_offset(index: usize) -> i32 {
    (index as i64 * OBJECT_WORD_BYTES) as i32
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

/// Canonical physical storage layout for one language value. `stride` is kept distinct from
/// `size`: aggregates and container elements may require tail padding even when every current
/// scalar happens to have `stride == size`. If callers recompute this tuple independently, pointer
/// fields and typed array backing will diverge as soon as multi-lane values enter the ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ValueLayout {
    pub(crate) size: usize,
    pub(crate) align: usize,
    pub(crate) stride: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub(crate) enum PtrLane {
    Provenance = 0,
    Address = 1,
    ViewStart = 2,
    ViewEnd = 3,
}

impl PtrLane {
    const ALL: [Self; 4] = [
        Self::Provenance,
        Self::Address,
        Self::ViewStart,
        Self::ViewEnd,
    ];
}

/// Windows x64 上 Alias pointer capability 的唯一物理布局 owner。
///
/// machine pointer 仍是单个 64-bit 地址；Alias pointer value 是四个独立 I64 lane。
/// 把二者都当成单个 pointer type 会让后续 expression/storage/parameter lowering 把 32-byte
/// capability 错压成一个 I64，因此这里同时冻结 lane 次序、offset 与 aggregate layout。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PtrLayout {
    lanes: [Type; 4],
    offsets: [i32; 4],
    value: ValueLayout,
}

const WINDOWS_X64_PTR_LAYOUT: PtrLayout = PtrLayout {
    lanes: [types::I64; 4],
    offsets: [0, 8, 16, 24],
    value: ValueLayout {
        size: 32,
        align: 8,
        stride: 32,
    },
};

impl PtrLayout {
    /// Binds the frozen language capability layout to the explicit x64 machine-pointer target.
    /// A different target must define its own complete layout instead of silently reusing these
    /// offsets with a different machine address width.
    pub(crate) fn for_current_target(machine_pointer: Type) -> AliasResult<Self> {
        if machine_pointer != types::I64 {
            return Err(AliasError {
                msg: format!(
                    "当前 Alias pointer ABI 需要 64-bit machine pointer，目标提供 {machine_pointer}"
                ),
                span: Span::default(),
            });
        }
        let layout = WINDOWS_X64_PTR_LAYOUT;
        for lane in PtrLane::ALL {
            let index = lane as usize;
            if layout.lanes[index] != machine_pointer
                || layout.offsets[index] != (index * machine_pointer.bytes() as usize) as i32
            {
                return Err(AliasError {
                    msg: "内部 ABI 不变式被破坏: PtrLayout lane 类型或 offset 漂移".into(),
                    span: Span::default(),
                });
            }
        }
        if layout.value.size != 32 || layout.value.align != 8 || layout.value.stride != 32 {
            return Err(AliasError {
                msg: "内部 ABI 不变式被破坏: PtrLayout aggregate size/align/stride 漂移".into(),
                span: Span::default(),
            });
        }
        Ok(layout)
    }

    pub(crate) fn machine_pointer_type(self) -> Type {
        self.value_abi().expression.lanes[PtrLane::Address as usize]
    }

    /// Canonical `ValueAbi` shape for both `ptr<T>` and `ptr<T>?`. Parameter and return shapes
    /// are frozen here even before their caller/callee lowering is opened, so a consumer that
    /// still asks for a direct scalar fails closed instead of collapsing the capability to I64.
    pub(crate) fn value_abi(self) -> ValueAbi {
        ValueAbi {
            expression: ExpressionAbi {
                lanes: self.lanes.to_vec(),
            },
            storage: StorageAbi {
                lanes: self
                    .lanes
                    .into_iter()
                    .zip(self.offsets)
                    .map(|(ty, offset)| StorageLane { ty, offset })
                    .collect(),
                layout: self.value,
            },
            parameter: ParameterAbi {
                direct: None,
                indirect_by_value: true,
            },
            result: ReturnAbi {
                direct: None,
                explicit_sret: true,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExpressionAbi {
    lanes: Vec<Type>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StorageLane {
    ty: Type,
    offset: i32,
}

impl StorageLane {
    pub(crate) fn ty(self) -> Type {
        self.ty
    }

    pub(crate) fn offset(self) -> i32 {
        self.offset
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StorageAbi {
    lanes: Vec<StorageLane>,
    layout: ValueLayout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ParameterAbi {
    direct: Option<Type>,
    indirect_by_value: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReturnAbi {
    direct: Option<Type>,
    explicit_sret: bool,
}

/// 一个静态值跨 Cranelift expression、storage 与用户函数边界时的唯一物理合同。
/// aggregate 形态必须保留为 aggregate；把它压回单个 I64 会丢失 capability lanes。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValueAbi {
    expression: ExpressionAbi,
    storage: StorageAbi,
    parameter: ParameterAbi,
    result: ReturnAbi,
}

impl ValueAbi {
    fn scalar(register: Type, storage: Type, align: usize) -> Self {
        let size = storage.bytes() as usize;
        Self {
            expression: ExpressionAbi {
                lanes: vec![register],
            },
            storage: StorageAbi {
                lanes: vec![StorageLane {
                    ty: storage,
                    offset: 0,
                }],
                layout: ValueLayout {
                    size,
                    align,
                    stride: align_to(size, align),
                },
            },
            parameter: ParameterAbi {
                direct: Some(storage),
                indirect_by_value: false,
            },
            result: ReturnAbi {
                direct: Some(storage),
                explicit_sret: false,
            },
        }
    }

    fn scalar_register(&self) -> Type {
        match self.expression.lanes.as_slice() {
            [ty] => *ty,
            lanes => panic!(
                "内部 ABI 不变式被破坏: {}-lane aggregate 进入 scalar expression 路径",
                lanes.len()
            ),
        }
    }

    pub(crate) fn expression_types(&self) -> &[Type] {
        &self.expression.lanes
    }

    pub(crate) fn storage_lanes(&self) -> &[StorageLane] {
        &self.storage.lanes
    }

    fn scalar_storage(&self) -> Type {
        match self.storage.lanes.as_slice() {
            [lane] if lane.offset == 0 => lane.ty,
            _ => panic!(
                "内部 ABI 不变式被破坏: {}-byte aggregate 进入 scalar storage 路径",
                self.storage.layout.size
            ),
        }
    }

    fn direct_parameter(&self) -> Type {
        match (self.parameter.direct, self.parameter.indirect_by_value) {
            (Some(ty), false) => ty,
            (None, true) => panic!(
                "内部 ABI 不变式被破坏: IndirectByValue({}, {}) 尚未进入 caller/callee lowering",
                self.storage.layout.size, self.storage.layout.align
            ),
            _ => panic!("内部 ABI 不变式被破坏: parameter ABI 形态冲突"),
        }
    }

    fn direct_result(&self) -> Type {
        match (self.result.direct, self.result.explicit_sret) {
            (Some(ty), false) => ty,
            (None, true) => panic!(
                "内部 ABI 不变式被破坏: ExplicitSRet({}, {}) 尚未进入 caller/callee lowering",
                self.storage.layout.size, self.storage.layout.align
            ),
            _ => panic!("内部 ABI 不变式被破坏: return ABI 形态冲突"),
        }
    }

}

impl VTy {
    pub(crate) fn abi(&self) -> ValueAbi {
        match self {
            VTy::I(w) => integer_abi(w.bits()),
            VTy::U(w) => integer_abi(w.bits()),
            VTy::F(FloatW::F32) => ValueAbi::scalar(types::F32, types::F32, 4),
            VTy::F(FloatW::F64) => ValueAbi::scalar(types::F64, types::F64, 8),
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
            | VTy::Iterator(_) => ValueAbi::scalar(types::I64, types::I64, 8),
        }
    }

    pub(crate) fn is_numeric(&self) -> bool {
        matches!(self, VTy::I(_) | VTy::U(_) | VTy::F(_))
    }
}

fn integer_abi(bits: u32) -> ValueAbi {
    let storage = ir_type_bits(bits);
    // 算术/比较发射统一处理 I64 形式，但 memory 与函数边界必须保持源码声明宽度；
    // 否则 i8/i16/i32 的布局和 Windows 调用签名会被内部规范化意外扩大。
    ValueAbi::scalar(types::I64, storage, (bits / 8) as usize)
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
    vty.abi().scalar_storage()
}

pub(crate) fn value_layout(vty: &VTy) -> ValueLayout {
    vty.abi().storage.layout
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
    sig.params.extend(
        params
            .iter()
            .map(|p| AbiParam::new(p.abi().direct_parameter())),
    );
    if *ret != VTy::Unit {
        sig.returns.push(AbiParam::new(ret.abi().direct_result()));
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
            let field_layout = value_layout(&vty);
            // 每个字段先对齐自身，再推进实际存储宽度；struct 值本身是引用 word，
            // 因此无需预注册其它 struct 的 inline layout。
            off = align_to(off, field_layout.align);
            max_align = max_align.max(field_layout.align);
            fields.push(StructFieldLayout {
                default: field.default.clone(),
                vty,
                offset: off as i32,
            });
            off += field_layout.size;
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
    let storage = abi.scalar_storage();
    let register = abi.scalar_register();
    // 从真实 storage 边界进入表达式世界时恢复规范寄存器形式；窄有符号/无符号
    // 必须分别扩展，否则高位语义会在后续 I64 算术和比较中被破坏。
    if storage == register {
        raw
    } else if matches!(vty, VTy::I(_)) {
        bcx.ins().sextend(register, raw)
    } else if matches!(vty, VTy::U(_)) {
        bcx.ins().uextend(register, raw)
    } else {
        raw
    }
}

pub(crate) fn norm_store(bcx: &mut FunctionBuilder, value: Value, vty: &VTy) -> Value {
    let abi = vty.abi();
    let storage = abi.scalar_storage();
    let actual = bcx.func.dfg.value_type(value);
    if actual == storage {
        return value;
    }
    // 写回 cell/field/参数边界前恢复声明宽度；float promotion/demotion 只处理真实
    // F32/F64 数值，不与通用 word 的 bit encoding 混用。
    match vty {
        VTy::I(_) | VTy::U(_) => bcx.ins().ireduce(storage, value),
        VTy::F(FloatW::F32) if actual == types::F64 => bcx.ins().fdemote(types::F32, value),
        VTy::F(FloatW::F64) if actual == types::F32 => bcx.ins().fpromote(types::F64, value),
        _ => value,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_struct_layouts, insert_projection, project_ty, projected_ty, user_signature,
        value_layout, ExpressionAbi, ParameterAbi, ProjectionTable, PtrLane, PtrLayout, ReturnAbi,
        StorageAbi, StorageLane, VTy, ValueAbi, ValueLayout,
    };
    use crate::sema::types::{FloatW, IntW, ReturnBorrowSource, ReturnEffect, Ty, UIntW};
    use cranelift_codegen::ir::types;

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
            let layout = value_layout(&value);
            let storage = abi.scalar_storage();
            assert!(layout.size > 0);
            assert!(layout.align.is_power_of_two());
            assert_eq!(layout.size, storage.bytes() as usize);
            assert_eq!(abi.parameter.direct, Some(storage));
            assert!(!abi.parameter.indirect_by_value);
            assert_eq!(abi.result.direct, Some(storage));
            assert!(!abi.result.explicit_sret);
            assert!(layout.stride >= layout.size);
            assert_eq!(layout.stride % layout.align, 0);
        }
    }

    #[test]
    fn value_abi_can_represent_multi_lane_aggregate_without_scalar_collapse() {
        static LANES: [cranelift_codegen::ir::Type; 2] = [types::I32, types::I64];
        let layout = ValueLayout {
            size: 16,
            align: 8,
            stride: 16,
        };
        let abi = ValueAbi {
            expression: ExpressionAbi {
                lanes: LANES.to_vec(),
            },
            storage: StorageAbi {
                lanes: LANES
                    .into_iter()
                    .enumerate()
                    .map(|(index, ty)| StorageLane {
                        ty,
                        offset: (index * 8) as i32,
                    })
                    .collect(),
                layout,
            },
            parameter: ParameterAbi {
                direct: None,
                indirect_by_value: true,
            },
            result: ReturnAbi {
                direct: None,
                explicit_sret: true,
            },
        };

        assert_eq!(abi.expression.lanes, LANES);
        assert_eq!(
            abi.storage.lanes,
            vec![
                StorageLane {
                    ty: types::I32,
                    offset: 0,
                },
                StorageLane {
                    ty: types::I64,
                    offset: 8,
                },
            ]
        );
        assert_eq!(abi.storage.layout, layout);
        assert!(abi.parameter.indirect_by_value);
        assert!(abi.result.explicit_sret);
        assert!(std::panic::catch_unwind(|| abi.scalar_register()).is_err());
        assert!(std::panic::catch_unwind(|| abi.scalar_storage()).is_err());
        assert!(std::panic::catch_unwind(|| abi.direct_parameter()).is_err());
        assert!(std::panic::catch_unwind(|| abi.direct_result()).is_err());
    }

    #[test]
    fn pointer_layout_is_four_i64_lanes_with_exact_windows_x64_offsets() {
        let layout = PtrLayout::for_current_target(types::I64).unwrap();
        assert_eq!(layout.lanes, [types::I64; 4]);
        assert_eq!(
            PtrLane::ALL.map(|lane| layout.offsets[lane as usize]),
            [0, 8, 16, 24]
        );
        assert_eq!(
            layout.value,
            ValueLayout {
                size: 32,
                align: 8,
                stride: 32,
            }
        );
        assert_eq!(layout.machine_pointer_type(), types::I64);
        let abi = layout.value_abi();
        assert_eq!(abi.expression.lanes, [types::I64; 4]);
        assert_eq!(
            abi.storage.lanes,
            vec![
                StorageLane {
                    ty: types::I64,
                    offset: 0,
                },
                StorageLane {
                    ty: types::I64,
                    offset: 8,
                },
                StorageLane {
                    ty: types::I64,
                    offset: 16,
                },
                StorageLane {
                    ty: types::I64,
                    offset: 24,
                },
            ]
        );
        assert_eq!(abi.storage.layout, layout.value);
        assert_eq!(
            abi.parameter,
            ParameterAbi {
                direct: None,
                indirect_by_value: true,
            }
        );
        assert_eq!(
            abi.result,
            ReturnAbi {
                direct: None,
                explicit_sret: true,
            }
        );

        let error = PtrLayout::for_current_target(types::I32)
            .expect_err("32-bit machine pointer must not reuse the Windows x64 layout");
        assert!(
            error.msg.contains("64-bit machine pointer"),
            "{}",
            error.msg
        );
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
