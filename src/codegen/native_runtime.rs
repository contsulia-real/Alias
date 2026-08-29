//! 产物内 native runtime facade；所有 shim 签名仍由 runtime contract 唯一生成。

use crate::codegen::runtime::{
    runtime_contract, validate_contract_table, RuntimeContract, RUNTIME_CONTRACTS,
};
use crate::codegen::{native_err, Compiler};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::{AbiParam, Signature, Type};
use cranelift_module::{FuncId, Linkage, Module};

pub(super) struct NativeExterns {
    pub(super) get_std_handle: FuncId,
    pub(super) write_file: FuncId,
    pub(super) exit_process: FuncId,
}

impl<M: Module> Compiler<'_, M> {
    fn external_signature(&self, params: &[Type], ret: Option<Type>) -> Signature {
        let mut signature = Signature::new(self.cc);
        signature
            .params
            .extend(params.iter().copied().map(AbiParam::new));
        if let Some(ret) = ret {
            signature.returns.push(AbiParam::new(ret));
        }
        signature
    }

    /// 平台 extern 与 Alias runtime 是两条不同 authority 边界；`alias.*`/`rt.*`
    /// 必须经 runtime owner 生成签名，不能借普通 extern 绕过 contract 检查。
    pub(crate) fn import_external(
        &mut self,
        name: &str,
        params: &[Type],
        ret: Option<Type>,
    ) -> AliasResult<FuncId> {
        if name.starts_with("alias.") || name.starts_with("rt.") {
            return Err(native_err(
                Span::default(),
                format!("内部: runtime 符号 '{name}' 必须经契约表声明"),
            ));
        }
        self.module
            .declare_function(name, Linkage::Import, &self.external_signature(params, ret))
            .map_err(|error| native_err(Span::default(), format!("内部: 符号声明失败 {error}")))
    }
}

macro_rules! shim {
    ($c:expr, $name:expr, |$bcx:ident, $a:ident| $body:block) => {{
        let (__fid, __sig, __contract) =
            $crate::codegen::native_runtime::declare_runtime_shim($c, $name)?;
        let __ret = __contract.ret.map(|v| v.ty.resolve($c.ptr_ty));
        let mut __ctx = cranelift_codegen::Context::new();
        __ctx.func = cranelift_codegen::ir::Function::with_name_signature(
            cranelift_codegen::ir::UserFuncName::user(0x77, __fid.as_u32()),
            __sig.clone(),
        );
        let mut __fbc = cranelift_frontend::FunctionBuilderContext::new();
        let mut $bcx = cranelift_frontend::FunctionBuilder::new(&mut __ctx.func, &mut __fbc);
        let __entry = $bcx.create_block();
        $bcx.append_block_params_for_function_params(__entry);
        $bcx.switch_to_block(__entry);
        $bcx.seal_block(__entry);
        let $a: Vec<cranelift_codegen::ir::Value> = $bcx.block_params(__entry).to_vec();
        let _ = &$a;
        let __terminated: bool = $body;
        if !__terminated {
            if __ret.is_some() {
                return Err($crate::codegen::native_err(
                    $crate::Span::default(),
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
