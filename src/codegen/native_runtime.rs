//! Native runtime facade. Runtime contracts remain the single ABI source.

use crate::codegen::runtime::{
    runtime_contract, validate_contract_table, RuntimeContract, RUNTIME_CONTRACTS,
};
use crate::codegen::{invariant_violation, native_err, Compiler};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{
    BlockArg, Function, InstBuilder, StackSlotData, StackSlotKind, UserFuncName, Value,
};
use cranelift_codegen::Context;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::HashMap;

pub(super) struct NativeExterns {
    pub(super) get_std_handle: FuncId,
    pub(super) write_file: FuncId,
    pub(super) exit_process: FuncId,
}

macro_rules! shim {
    ($c:expr, $name:expr, |$bcx:ident, $a:ident| $body:block) => {{
        let (__fid, __sig, __contract) = declare_runtime_shim($c, $name)?;
        let __ret = __contract.ret.map(|v| v.ty.resolve($c.ptr_ty));
        let mut __ctx = Context::new();
        __ctx.func =
            Function::with_name_signature(UserFuncName::user(0x77, __fid.as_u32()), __sig.clone());
        let mut __fbc = FunctionBuilderContext::new();
        let mut $bcx = FunctionBuilder::new(&mut __ctx.func, &mut __fbc);
        let __entry = $bcx.create_block();
        $bcx.append_block_params_for_function_params(__entry);
        $bcx.switch_to_block(__entry);
        $bcx.seal_block(__entry);
        let $a: Vec<Value> = $bcx.block_params(__entry).to_vec();
        let _ = &$a;
        let __terminated: bool = $body;
        if !__terminated {
            if __ret.is_some() {
                return Err(native_err(
                    Span::default(),
                    format!("内部: 有返回值 runtime shim '{}' 未终止", $name),
                ));
            }
            $bcx.ins().return_(&[]);
        }
        $bcx.finalize($c.module.target_config());
        $c.define_verified_function(__fid, &mut __ctx, &format!("runtime shim '{}'", $name))?;
    }};
}

mod abort;
mod alloc;
mod arrays;
mod display;
mod display_float;
mod display_integer;
mod driver;
mod io;
mod strings;

pub(crate) use abort::define_span_data;
pub(crate) use driver::emit_native_runtime;

pub(super) fn declare_runtime_shim<M: Module>(
    c: &mut Compiler<'_, M>,
    name: &str,
) -> AliasResult<(
    FuncId,
    cranelift_codegen::ir::Signature,
    &'static RuntimeContract,
)> {
    let contract = runtime_contract(name)?;
    let sig = contract.signature(c.cc, c.ptr_ty);
    let fid = c
        .module
        .declare_function(contract.symbol, Linkage::Export, &sig)
        .map_err(|e| native_err(Span::default(), format!("内部: shim 声明失败 {e}")))?;
    if !c.runtime_defined.insert(contract.symbol) {
        return Err(native_err(
            Span::default(),
            format!("内部: runtime 重复定义 '{}'", contract.symbol),
        ));
    }
    Ok((fid, sig, contract))
}

pub(super) fn validate_native_runtime_coverage<M: Module>(c: &Compiler<'_, M>) -> AliasResult<()> {
    validate_contract_table().map_err(|msg| native_err(Span::default(), msg))?;
    let expected = RUNTIME_CONTRACTS
        .iter()
        .map(|contract| contract.symbol)
        .collect::<std::collections::HashSet<_>>();
    if c.runtime_defined != expected {
        let missing = expected
            .difference(&c.runtime_defined)
            .copied()
            .collect::<Vec<_>>();
        let extra = c
            .runtime_defined
            .difference(&expected)
            .copied()
            .collect::<Vec<_>>();
        return Err(native_err(
            Span::default(),
            format!("内部: 原生 runtime 与契约表不一致，缺失 {missing:?}，多余 {extra:?}"),
        ));
    }
    Ok(())
}
