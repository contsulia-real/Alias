//! Runtime 符号契约的唯一真相源。
//!
//! 调用点和原生 runtime 实现都只引用符号名；参数、返回值与可空性
//! 从本表生成并在构建/测试时校验。

use crate::codegen::{native_err, Compiler};
use crate::{AliasError, AliasResult, Span};
use cranelift_codegen::ir::{types, AbiParam, InstBuilder, Signature, Type, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::{FuncId, Linkage, Module};

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

#[derive(Clone, Copy, Debug)]
pub(crate) struct RuntimeContract {
    pub(crate) symbol: &'static str,
    pub(crate) params: &'static [RuntimeValue],
    pub(crate) ret: Option<RuntimeValue>,
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
    ($name:literal, [$($param:expr),* $(,)?] $(-> $ret:expr)?) => {
        RuntimeContract {
            symbol: $name,
            params: &[$($param),*],
            ret: contract!(@ret $($ret)?),
        }
    };
    (@ret) => { None };
    (@ret $ret:expr) => { Some($ret) };
}

/// 所有 Alias/内部 runtime 符号。新增符号必须在这里声明完整机器契约。
pub(crate) static RUNTIME_CONTRACTS: &[RuntimeContract] = &[
    contract!("alias.cell.new", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64)),
    contract!("alias.env.new", [val(RuntimeTy::I32)] -> val(RuntimeTy::I64)),
    contract!("alias.globals.new", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64)),
    contract!("alias.closure.new", [val(RuntimeTy::I64), nullable(RuntimeTy::I64)] -> val(RuntimeTy::I64)),
    contract!("alias.str.new", [nullable(RuntimeTy::Ptr), val(RuntimeTy::I32)] -> val(RuntimeTy::I64)),
    contract!("alias.str.concat", [val(RuntimeTy::I64), val(RuntimeTy::I64)] -> val(RuntimeTy::I64)),
    contract!("alias.str.cmp", [val(RuntimeTy::I64), val(RuntimeTy::I64)] -> val(RuntimeTy::I32)),
    contract!("alias.str.len", [val(RuntimeTy::I64)] -> val(RuntimeTy::I32)),
    contract!("alias.str.upper", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64)),
    contract!("alias.str.lower", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64)),
    contract!("alias.str.trim", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64)),
    contract!("alias.arr.new", [val(RuntimeTy::I32)] -> val(RuntimeTy::I64)),
    contract!("alias.arr.len", [val(RuntimeTy::I64)] -> val(RuntimeTy::I32)),
    contract!(
        "alias.arr.push",
        [val(RuntimeTy::I64), nullable(RuntimeTy::I64)]
    ),
    contract!("alias.arr.pop", [val(RuntimeTy::I64)] -> nullable(RuntimeTy::I64)),
    contract!("alias.display.int", [val(RuntimeTy::I32)] -> val(RuntimeTy::I64)),
    contract!("alias.display.i64", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64)),
    contract!("alias.display.u64", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64)),
    contract!("alias.display.f32", [val(RuntimeTy::F32)] -> val(RuntimeTy::I64)),
    contract!("alias.display.f64", [val(RuntimeTy::F64)] -> val(RuntimeTy::I64)),
    contract!("alias.display.bool", [val(RuntimeTy::I32)] -> val(RuntimeTy::I64)),
    contract!("alias.display.str", [val(RuntimeTy::I64)] -> val(RuntimeTy::I64)),
    contract!("alias.display.func", [] -> val(RuntimeTy::I64)),
    contract!("alias.display.struct", [] -> val(RuntimeTy::I64)),
    contract!("alias.display.array", [] -> val(RuntimeTy::I64)),
    contract!("alias.display.result", [val(RuntimeTy::I32)] -> val(RuntimeTy::I64)),
    contract!("alias.println.str", [val(RuntimeTy::I64)]),
    contract!("alias.print.str", [val(RuntimeTy::I64)]),
    contract!("alias.println.i32", [val(RuntimeTy::I32)]),
    contract!("alias.print.i32", [val(RuntimeTy::I32)]),
    contract!("alias.println.bool", [val(RuntimeTy::I32)]),
    contract!("alias.print.bool", [val(RuntimeTy::I32)]),
    contract!("alias.abort_div", [val(RuntimeTy::I32)]),
    contract!("alias.abort_oob", [val(RuntimeTy::I32)]),
    contract!("alias.abort_pop", [val(RuntimeTy::I32)]),
    contract!("alias.abort_conv", [val(RuntimeTy::I32)]),
    contract!("alias.abort_overflow", [val(RuntimeTy::I32)]),
    contract!("alias.abort_iter", [val(RuntimeTy::I32)]),
    contract!("rt.heap.alloc", [val(RuntimeTy::I64)] -> val(RuntimeTy::Ptr)),
    contract!("rt.write.dec", [val(RuntimeTy::Ptr), val(RuntimeTy::I64)]),
    contract!(
        "rt.write.stdout",
        [nullable(RuntimeTy::Ptr), val(RuntimeTy::I64)]
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

impl<M: Module> Compiler<'_, M> {
    /// runtime import 的签名只能由 RUNTIME_CONTRACTS 生成；调用点统一经过这里后，
    /// 参数数量、机器类型及 value-vs-unit 形态才不会在 emitter 中形成平行合同。
    pub(crate) fn import_runtime(&mut self, name: &str) -> AliasResult<FuncId> {
        let contract = runtime_contract(name)?;
        let sig = contract.signature(self.cc, self.ptr_ty);
        self.module
            .declare_function(contract.symbol, Linkage::Import, &sig)
            .map_err(|error| native_err(Span::default(), format!("内部: runtime 声明失败 {error}")))
    }

    pub(crate) fn call_rt(
        &mut self,
        bcx: &mut FunctionBuilder,
        name: &str,
        args: &[Value],
    ) -> AliasResult<Value> {
        if runtime_contract(name)?.ret.is_none() {
            return Err(native_err(
                Span::default(),
                format!("内部: 无返回值 runtime '{name}' 被当作值调用"),
            ));
        }
        let inst = self.call_rt_inst(bcx, name, args)?;
        match bcx.inst_results(inst) {
            [value] => Ok(*value),
            _ => Err(native_err(
                Span::default(),
                format!("内部: 有返回值 runtime '{name}' 未产生唯一结果"),
            )),
        }
    }

    pub(crate) fn call_rt_void(
        &mut self,
        bcx: &mut FunctionBuilder,
        name: &str,
        args: &[Value],
    ) -> AliasResult<()> {
        if runtime_contract(name)?.ret.is_some() {
            return Err(native_err(
                Span::default(),
                format!("内部: 有返回值 runtime '{name}' 被当作 unit 调用"),
            ));
        }
        let inst = self.call_rt_inst(bcx, name, args)?;
        if !bcx.inst_results(inst).is_empty() {
            return Err(native_err(
                Span::default(),
                format!("内部: 无返回值 runtime '{name}' 意外产生结果"),
            ));
        }
        Ok(())
    }

    fn call_rt_inst(
        &mut self,
        bcx: &mut FunctionBuilder,
        name: &str,
        args: &[Value],
    ) -> AliasResult<cranelift_codegen::ir::Inst> {
        let contract = runtime_contract(name)?;
        if args.len() != contract.params.len() {
            return Err(native_err(
                Span::default(),
                format!(
                    "内部: runtime '{}' 参数数量不匹配，契约 {}，调用点 {}",
                    name,
                    contract.params.len(),
                    args.len()
                ),
            ));
        }
        for (index, (arg, expected)) in args.iter().zip(contract.params).enumerate() {
            let actual = bcx.func.dfg.value_type(*arg);
            let expected = expected.ty.resolve(self.ptr_ty);
            if actual != expected {
                return Err(native_err(
                    Span::default(),
                    format!(
                        "内部: runtime '{}' 第 {} 个参数类型不匹配，契约 {}，调用点 {}",
                        name,
                        index + 1,
                        expected,
                        actual
                    ),
                ));
            }
        }
        let fid = self.import_runtime(name)?;
        let fref = self.module.declare_func_in_func(fid, bcx.func);
        Ok(bcx.ins().call(fref, args))
    }
}

pub(crate) fn validate_contract_table() -> Result<(), String> {
    let mut names = std::collections::HashSet::new();
    for contract in RUNTIME_CONTRACTS {
        if !names.insert(contract.symbol) {
            return Err(format!("runtime 契约重复: {}", contract.symbol));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{runtime_contract, validate_contract_table};

    #[test]
    fn runtime_contract_table_is_unique() {
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
