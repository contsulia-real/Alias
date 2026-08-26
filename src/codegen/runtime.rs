//! Runtime 符号契约的唯一真相源。
//!
//! 调用点、JIT host 注册和 AOT shim 都只引用符号名；参数、返回值、可空性
//! 与后端覆盖从本表生成并在构建/测试时校验。

use crate::{AliasError, AliasResult, Span};
use cranelift_codegen::ir::{types, AbiParam, Signature, Type};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeTy {
    I32,
    I64,
    F32,
    F64,
    Ptr,
}

impl RuntimeTy {
    pub(crate) fn resolve(self, ptr_ty: Type) -> Type {
        match self {
            Self::I32 => types::I32,
            Self::I64 => types::I64,
            Self::F32 => types::F32,
            Self::F64 => types::F64,
            Self::Ptr => ptr_ty,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeValue {
    pub(crate) ty: RuntimeTy,
    /// 仅对指针或承载指针的 I64 有意义；仍显式填写，避免契约靠口头约定。
    pub(crate) nullable: bool,
}

const fn val(ty: RuntimeTy) -> RuntimeValue {
    RuntimeValue {
        ty,
        nullable: false,
    }
}

const fn nullable(ty: RuntimeTy) -> RuntimeValue {
    RuntimeValue { ty, nullable: true }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeBackends {
    pub(crate) jit: bool,
    pub(crate) aot: bool,
}

const BOTH: RuntimeBackends = RuntimeBackends {
    jit: true,
    aot: true,
};
const JIT_ONLY: RuntimeBackends = RuntimeBackends {
    jit: true,
    aot: false,
};
const AOT_ONLY: RuntimeBackends = RuntimeBackends {
    jit: false,
    aot: true,
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct RuntimeContract {
    pub(crate) symbol: &'static str,
    pub(crate) params: &'static [RuntimeValue],
    pub(crate) ret: Option<RuntimeValue>,
    pub(crate) backends: RuntimeBackends,
}

impl RuntimeContract {
    pub(crate) fn signature(
        &self,
        cc: cranelift_codegen::isa::CallConv,
        ptr_ty: Type,
    ) -> Signature {
        let mut sig = Signature::new(cc);
        sig.params.extend(
            self.params
                .iter()
                .map(|p| AbiParam::new(p.ty.resolve(ptr_ty))),
        );
        if let Some(ret) = self.ret {
            sig.returns.push(AbiParam::new(ret.ty.resolve(ptr_ty)));
        }
        sig
    }
}

macro_rules! contract {
    ($name:literal, [$($param:expr),* $(,)?] $(-> $ret:expr)?, $backends:expr) => {
        RuntimeContract {
            symbol: $name,
            params: &[$($param),*],
            ret: contract!(@ret $($ret)?),
            backends: $backends,
        }
    };
    (@ret) => { None };
    (@ret $ret:expr) => { Some($ret) };
}

/// 所有 Alias/内部 runtime 符号。新增符号必须在这里声明后端覆盖和可空性。
pub(crate) static RUNTIME_CONTRACTS: &[RuntimeContract] = &[
    contract!("alias.cell.new", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.env.new", [val(RuntimeTy::I32)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.globals.new", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.closure.new", [val(RuntimeTy::I64), nullable(RuntimeTy::I64)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.str.new", [nullable(RuntimeTy::Ptr), val(RuntimeTy::I32)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.str.concat", [val(RuntimeTy::I64), val(RuntimeTy::I64)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.str.cmp", [val(RuntimeTy::I64), val(RuntimeTy::I64)] -> val(RuntimeTy::I32), BOTH),
    contract!("alias.str.len", [val(RuntimeTy::I64)] -> val(RuntimeTy::I32), BOTH),
    contract!("alias.str.upper", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.str.lower", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.str.trim", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.arr.new", [val(RuntimeTy::I32), val(RuntimeTy::I32)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.arr.len", [val(RuntimeTy::I64)] -> val(RuntimeTy::I32), BOTH),
    contract!(
        "alias.arr.push",
        [val(RuntimeTy::I64), nullable(RuntimeTy::I64)],
        BOTH
    ),
    contract!("alias.arr.pop", [val(RuntimeTy::I64)] -> nullable(RuntimeTy::I64), BOTH),
    contract!("alias.display.int", [val(RuntimeTy::I32)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.display.i64", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.display.u64", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.display.f32", [val(RuntimeTy::F32)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.display.f64", [val(RuntimeTy::F64)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.display.bool", [val(RuntimeTy::I32)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.display.str", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.display.unit", [] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.display.func", [] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.display.struct", [] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.display.array", [] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.display.result", [val(RuntimeTy::I32)] -> val(RuntimeTy::I64), BOTH),
    contract!("alias.println.str", [val(RuntimeTy::I64)], BOTH),
    contract!("alias.print.str", [val(RuntimeTy::I64)], BOTH),
    contract!("alias.println.i32", [val(RuntimeTy::I32)], BOTH),
    contract!("alias.print.i32", [val(RuntimeTy::I32)], BOTH),
    contract!("alias.println.bool", [val(RuntimeTy::I32)], BOTH),
    contract!("alias.print.bool", [val(RuntimeTy::I32)], BOTH),
    contract!("alias.abort_div", [val(RuntimeTy::I32)], BOTH),
    contract!("alias.abort_oob", [val(RuntimeTy::I32)], BOTH),
    contract!("alias.abort_pop", [val(RuntimeTy::I32)], BOTH),
    contract!("alias.abort_conv", [val(RuntimeTy::I32)], BOTH),
    contract!("alias.runtime.failed", [] -> val(RuntimeTy::I32), JIT_ONLY),
    contract!("rt.heap.alloc", [val(RuntimeTy::I64)] -> val(RuntimeTy::Ptr), AOT_ONLY),
    contract!(
        "rt.write.dec",
        [val(RuntimeTy::Ptr), val(RuntimeTy::I64)],
        AOT_ONLY
    ),
    contract!(
        "rt.write.stdout",
        [nullable(RuntimeTy::Ptr), val(RuntimeTy::I64)],
        AOT_ONLY
    ),
];

pub(crate) fn runtime_contract(symbol: &str) -> AliasResult<&'static RuntimeContract> {
    RUNTIME_CONTRACTS
        .iter()
        .find(|contract| contract.symbol == symbol)
        .ok_or_else(|| AliasError {
            msg: format!("内部: 未登记 runtime 符号 '{symbol}'"),
            span: Span::default(),
        })
}

pub(crate) fn validate_contract_table() -> Result<(), String> {
    let mut names = std::collections::HashSet::new();
    for contract in RUNTIME_CONTRACTS {
        if !names.insert(contract.symbol) {
            return Err(format!("runtime 契约重复: {}", contract.symbol));
        }
        if !contract.backends.jit && !contract.backends.aot {
            return Err(format!("runtime 符号没有实现后端: {}", contract.symbol));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_contract_table_is_unique_and_every_symbol_has_a_backend() {
        validate_contract_table().unwrap();
    }

    #[test]
    fn nullable_edges_are_frozen_in_the_contract_table() {
        let str_new = runtime_contract("alias.str.new").unwrap();
        assert!(str_new.params[0].nullable, "空字符串允许 null 数据指针");
        let stdout = runtime_contract("rt.write.stdout").unwrap();
        assert!(stdout.params[0].nullable, "零长度写允许 null 数据指针");
        let closure = runtime_contract("alias.closure.new").unwrap();
        assert!(closure.params[1].nullable, "无捕获闭包允许 null env");
        assert!(
            !runtime_contract("alias.cell.new")
                .unwrap()
                .ret
                .unwrap()
                .nullable
        );
    }
}
