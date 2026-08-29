use super::NativeExterns;
use crate::codegen::abi::VALUE_WORD_BYTES;
use crate::codegen::emit::cells::first_result;
use crate::codegen::layout::{CLOSURE_BYTES, CLOSURE_CODE_OFFSET, CLOSURE_ENV_OFFSET};
use crate::codegen::Compiler;
use crate::AliasResult;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, InstBuilder, MemFlagsData, TrapCode};
use cranelift_module::{FuncId, Module};

const HEAP_ZERO_MEMORY: i64 = 0x0000_0008;

pub(super) fn emit_alloc_runtime<M: Module>(
    c: &mut Compiler<'_, M>,
    ext: &NativeExterns,
    heap_alloc: FuncId,
    get_process_heap: FuncId,
) -> AliasResult<()> {
    macro_rules! call_rt_m {
        ($bcx:expr, $nm:expr, $args:expr) => {{
            let __args = $args;
            c.call_rt(&mut $bcx, $nm, &__args)?
        }};
    }
    macro_rules! call_ext_m {
        ($bcx:expr, $fid:expr, $args:expr) => {{
            let __r = c.module.declare_func_in_func($fid, &mut $bcx.func);
            let __args = $args;
            let __inst = $bcx.ins().call(__r, &__args);
            first_result(&$bcx, __inst)
        }};
    }

    shim!(c, "rt.heap.alloc", |bcx, a| {
        let h = call_ext_m!(bcx, get_process_heap, vec![]);
        // cell/env/object header 依赖新分配 word 初始为零。移除 HEAP_ZERO_MEMORY 会在
        // constructor 写入显式字段前暴露未初始化指针和长度。
        let flags = bcx.ins().iconst(types::I32, HEAP_ZERO_MEMORY);
        let p = call_ext_m!(bcx, heap_alloc, vec![h, flags, a[0]]);
        let failed = bcx.ins().icmp_imm_s(IntCC::Equal, p, 0);
        let fail_b = bcx.create_block();
        let ok_b = bcx.create_block();
        bcx.ins().brif(failed, fail_b, &[], ok_b, &[]);
        bcx.seal_block(fail_b);
        bcx.seal_block(ok_b);
        bcx.switch_to_block(fail_b);
        let one = bcx.ins().iconst(types::I32, 1);
        let ep = c.module.declare_func_in_func(ext.exit_process, bcx.func);
        bcx.ins().call(ep, &[one]);
        // 外部分配失败必须终止；trap 防止 Cranelift 把 ExitProcess 当作可返回调用后
        // 继续携带 null 指针进入正常分配路径。
        bcx.ins().trap(TrapCode::INTEGER_DIVISION_BY_ZERO);
        bcx.switch_to_block(ok_b);
        bcx.ins().return_(&[p]);
        true
    });

    shim!(c, "alias.cell.new", |bcx, a| {
        let p = call_rt_m!(bcx, "rt.heap.alloc", vec![a[0]]);
        bcx.ins().return_(&[p]);
        true
    });
    shim!(c, "alias.env.new", |bcx, a| {
        let n64 = bcx.ins().sextend(types::I64, a[0]);
        let bytes = bcx.ins().imul_imm_s(n64, VALUE_WORD_BYTES);
        let p = call_rt_m!(bcx, "rt.heap.alloc", vec![bytes]);
        bcx.ins().return_(&[p]);
        true
    });
    shim!(c, "alias.globals.new", |bcx, a| {
        let p = call_rt_m!(bcx, "rt.heap.alloc", vec![a[0]]);
        bcx.ins().return_(&[p]);
        true
    });
    shim!(c, "alias.closure.new", |bcx, a| {
        let sz = bcx.ins().iconst(types::I64, CLOSURE_BYTES);
        let p = call_rt_m!(bcx, "rt.heap.alloc", vec![sz]);
        bcx.ins()
            .store(MemFlagsData::new(), a[0], p, CLOSURE_CODE_OFFSET);
        bcx.ins()
            .store(MemFlagsData::new(), a[1], p, CLOSURE_ENV_OFFSET);
        bcx.ins().return_(&[p]);
        true
    });
    Ok(())
}
