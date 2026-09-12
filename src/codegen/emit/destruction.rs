//! Physical execution of sema-resolved destruction recipes.

use super::arrays::{array_element_addr, array_len, array_raw};
use super::value::ExprValue;
use crate::codegen::abi::{PtrLane, VTy};
use crate::codegen::layout::{
    result_layout, ARRAY_DATA_OFFSET, CLOSURE_ENV_OFFSET, RESULT_OK_TAG, RESULT_TAG_OFFSET,
};
use crate::codegen::{invariant_violation, Compiler, Frame, ScopeCleanup, ScopeCleanupAction};
use crate::sema::hir::{BindingId, DestroyNode, DestroyPlan, StorageRelation};
use crate::AliasResult;
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{types, Block, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::FunctionBuilder;
use cranelift_module::Module;

pub(in crate::codegen) fn register_local_cleanup(
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    binding: BindingId,
    cell: cranelift_frontend::Variable,
    vty: VTy,
    relation: StorageRelation,
    plan: DestroyPlan,
) {
    let presence = if relation == StorageRelation::Owning
        && !matches!(plan.nodes.first(), Some(DestroyNode::Inline))
    {
        let presence = bcx.declare_var(types::I8);
        let yes = bcx.ins().iconst(types::I8, 1);
        bcx.def_var(presence, yes);
        if frame.owner_presence.insert(binding, presence).is_some() {
            invariant_violation("同一 local owner 被重复登记")
        }
        Some(presence)
    } else {
        None
    };
    frame
        .cleanup_scopes
        .last_mut()
        .unwrap_or_else(|| invariant_violation("cleanup scope 栈非空"))
        .push(ScopeCleanup {
            binding: Some(binding),
            cell,
            vty,
            action: if relation == StorageRelation::Owning {
                ScopeCleanupAction::Destroy(plan)
            } else {
                ScopeCleanupAction::None
            },
            presence,
        });
}

pub(in crate::codegen) fn register_temporary_cleanup(
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    cell: Value,
    vty: VTy,
    plan: Option<DestroyPlan>,
) {
    let cell_var = bcx.declare_var(types::I64);
    bcx.def_var(cell_var, cell);
    frame
        .cleanup_scopes
        .last_mut()
        .unwrap_or_else(|| invariant_violation("cleanup scope 栈非空"))
        .push(ScopeCleanup {
            binding: None,
            cell: cell_var,
            vty,
            action: match plan {
                Some(plan) => ScopeCleanupAction::Destroy(plan),
                None => ScopeCleanupAction::FreeScalarValue,
            },
            presence: None,
        });
}

pub(in crate::codegen) fn emit_cleanup_to_depth<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    frame: &mut Frame,
    depth: usize,
) -> AliasResult<()> {
    if depth > frame.cleanup_scopes.len() {
        invariant_violation("cleanup depth 超出词法 scope 栈")
    }
    let cleanups = frame.cleanup_scopes[depth..]
        .iter()
        .rev()
        .flat_map(|scope| scope.iter().rev())
        .cloned()
        .collect::<Vec<_>>();
    for cleanup in cleanups {
        let cell = bcx.use_var(cleanup.cell);
        match cleanup.action {
            ScopeCleanupAction::Destroy(plan) => {
                if let Some(presence) = cleanup.presence {
                    let destroy = bcx.create_block();
                    let done = bcx.create_block();
                    let present = bcx.use_var(presence);
                    bcx.ins().brif(present, destroy, &[], done, &[]);
                    bcx.seal_block(destroy);
                    bcx.switch_to_block(destroy);
                    let value = ExprValue::load(bcx, cell, 0, &cleanup.vty);
                    emit_destroy_value(c, bcx, value, cleanup.vty.clone(), &plan)?;
                    bcx.ins().jump(done, &[]);
                    bcx.seal_block(done);
                    bcx.switch_to_block(done);
                    let no = bcx.ins().iconst(types::I8, 0);
                    bcx.def_var(presence, no);
                } else {
                    let value = ExprValue::load(bcx, cell, 0, &cleanup.vty);
                    emit_destroy_value(c, bcx, value, cleanup.vty.clone(), &plan)?;
                }
            }
            ScopeCleanupAction::FreeScalarValue => {
                let value = ExprValue::load(bcx, cell, 0, &cleanup.vty)
                    .into_scalar("内部 runtime temporary 必须是 scalar allocation");
                c.call_rt_void(bcx, "rt.heap.free", &[value])?;
            }
            ScopeCleanupAction::None => {}
        }
        c.call_rt_void(bcx, "rt.heap.free", &[cell])?;
    }
    Ok(())
}

pub(super) fn mark_owner_moved(bcx: &mut FunctionBuilder, frame: &mut Frame, binding: BindingId) {
    if let Some(presence) = frame.owner_presence.get(&binding).copied() {
        let no = bcx.ins().iconst(types::I8, 0);
        bcx.def_var(presence, no);
    }
}

pub(super) fn mark_owner_present(bcx: &mut FunctionBuilder, frame: &mut Frame, binding: BindingId) {
    if let Some(presence) = frame.owner_presence.get(&binding).copied() {
        let yes = bcx.ins().iconst(types::I8, 1);
        bcx.def_var(presence, yes);
    }
}

pub(super) fn owner_presence(
    bcx: &mut FunctionBuilder,
    frame: &Frame,
    binding: BindingId,
) -> Value {
    bcx.use_var(
        *frame.owner_presence.get(&binding).unwrap_or_else(|| {
            invariant_violation("MaybeMoved replacement 缺少独立 owner presence")
        }),
    )
}

enum Task {
    Node {
        value: Value,
        vty: VTy,
        node: usize,
    },
    Storage {
        base: Value,
        offset: i32,
        vty: VTy,
        node: usize,
    },
    Switch(Block),
    Jump(Block),
    ArrayDone {
        header: Value,
        wrapper: Value,
        loop_block: Block,
        done: Block,
    },
    ResultDone {
        root: Value,
        join: Block,
    },
    Free(Value),
}

pub(crate) fn emit_destroy_value<M: Module>(
    c: &mut Compiler<M>,
    bcx: &mut FunctionBuilder,
    value: ExprValue,
    vty: VTy,
    plan: &DestroyPlan,
) -> AliasResult<()> {
    if matches!(plan.nodes.first(), Some(DestroyNode::RawAllocationRoot)) {
        if !matches!(vty, VTy::Ptr { .. }) {
            invariant_violation("raw allocation destruction recipe 与 pointer VTy 不一致")
        }
        let descriptor = value.pointer_lane(bcx, &vty, PtrLane::Provenance);
        c.call_rt_void(bcx, "rt.raw.free", &[descriptor])?;
        return Ok(());
    }
    let value = value.into_scalar("destruction 尚未支持 multi-lane owned value");
    let mut tasks = vec![Task::Node {
        value,
        vty,
        node: 0,
    }];
    while let Some(task) = tasks.pop() {
        match task {
            Task::Storage {
                base,
                offset,
                vty,
                node,
            } => {
                let value = ExprValue::load(bcx, base, offset, &vty)
                    .into_scalar("destruction child 尚未支持 multi-lane value");
                tasks.push(Task::Node { value, vty, node });
            }
            Task::Switch(block) => bcx.switch_to_block(block),
            Task::Jump(block) => {
                bcx.ins().jump(block, &[]);
            }
            Task::Free(value) => c.call_rt_void(bcx, "rt.heap.free", &[value])?,
            Task::ArrayDone {
                header,
                wrapper,
                loop_block,
                done,
            } => {
                bcx.seal_block(loop_block);
                bcx.seal_block(done);
                bcx.switch_to_block(done);
                let data =
                    bcx.ins()
                        .load(types::I64, MemFlagsData::new(), header, ARRAY_DATA_OFFSET);
                c.call_rt_void(bcx, "rt.heap.free", &[data])?;
                c.call_rt_void(bcx, "rt.heap.free", &[header])?;
                c.call_rt_void(bcx, "rt.heap.free", &[wrapper])?;
            }
            Task::ResultDone { root, join } => {
                bcx.seal_block(join);
                bcx.switch_to_block(join);
                c.call_rt_void(bcx, "rt.heap.free", &[root])?;
            }
            Task::Node { value, vty, node } => match plan
                .nodes
                .get(node)
                .unwrap_or_else(|| invariant_violation("DestroyPlan child index 越界"))
            {
                DestroyNode::Inline => match vty {
                    VTy::I(_) | VTy::U(_) | VTy::F(_) | VTy::Bool | VTy::Borrowed(_) => {}
                    _ => invariant_violation("Inline destruction 与物理类型不一致"),
                },
                DestroyNode::String => {
                    if vty != VTy::Str {
                        invariant_violation("String destruction 与物理类型不一致")
                    }
                    c.call_rt_void(bcx, "rt.str.drop", &[value])?;
                }
                DestroyNode::Iterator => {
                    if !matches!(vty, VTy::Iterator(_)) {
                        invariant_violation("Iterator destruction 与物理类型不一致")
                    }
                    c.call_rt_void(bcx, "rt.heap.free", &[value])?;
                }
                DestroyNode::Closure => {
                    if !matches!(vty, VTy::Func { .. }) {
                        invariant_violation("Closure destruction 与物理类型不一致")
                    }
                    let env = bcx.ins().load(
                        types::I64,
                        MemFlagsData::new(),
                        value,
                        CLOSURE_ENV_OFFSET,
                    );
                    c.call_rt_void(bcx, "rt.heap.free", &[env])?;
                    c.call_rt_void(bcx, "rt.heap.free", &[value])?;
                }
                DestroyNode::RawAllocationRoot => {
                    invariant_violation("nested raw allocation destruction 未被 root gate 拦截")
                }
                DestroyNode::Struct { name, fields } => {
                    let VTy::Struct(vname) = &vty else {
                        invariant_violation("Struct destruction 与物理类型不一致")
                    };
                    if vname != name {
                        invariant_violation("Struct destruction 名称漂移")
                    }
                    let layout = c
                        .struct_layouts
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| invariant_violation("Struct destruction 缺少布局"));
                    if layout.fields.len() != fields.len() {
                        invariant_violation("Struct destruction 字段数量漂移")
                    }
                    tasks.push(Task::Free(value));
                    // Stack order executes the last declared field first.
                    for (field, child) in layout.fields.iter().zip(fields) {
                        tasks.push(Task::Storage {
                            base: value,
                            offset: field.offset,
                            vty: field.vty.clone(),
                            node: *child,
                        });
                    }
                }
                DestroyNode::Array { element } => {
                    let VTy::Array(element_vty) = vty else {
                        invariant_violation("Array destruction 与物理类型不一致")
                    };
                    let header = array_raw(bcx, value);
                    let len = array_len(bcx, header);
                    let index = bcx.declare_var(types::I64);
                    bcx.def_var(index, len);
                    let loop_block = bcx.create_block();
                    let body = bcx.create_block();
                    let done = bcx.create_block();
                    bcx.ins().jump(loop_block, &[]);
                    bcx.switch_to_block(loop_block);
                    let current = bcx.use_var(index);
                    let more = bcx.ins().icmp_imm_s(IntCC::UnsignedGreaterThan, current, 0);
                    bcx.ins().brif(more, body, &[], done, &[]);
                    bcx.seal_block(body);
                    bcx.switch_to_block(body);
                    let previous = bcx.ins().iadd_imm_s(current, -1);
                    bcx.def_var(index, previous);
                    let address = array_element_addr(bcx, header, previous);
                    tasks.push(Task::ArrayDone {
                        header,
                        wrapper: value,
                        loop_block,
                        done,
                    });
                    tasks.push(Task::Jump(loop_block));
                    tasks.push(Task::Storage {
                        base: address,
                        offset: 0,
                        vty: *element_vty,
                        node: *element,
                    });
                }
                DestroyNode::Result { ok, err } => {
                    let VTy::Result(ok_vty, err_vty) = vty else {
                        invariant_violation("Result destruction 与物理类型不一致")
                    };
                    let layout = result_layout(&ok_vty, &err_vty);
                    let tag =
                        bcx.ins()
                            .load(types::I64, MemFlagsData::new(), value, RESULT_TAG_OFFSET);
                    let is_ok = bcx.ins().icmp_imm_s(IntCC::Equal, tag, RESULT_OK_TAG);
                    let ok_block = bcx.create_block();
                    let err_block = bcx.create_block();
                    let join = bcx.create_block();
                    bcx.ins().brif(is_ok, ok_block, &[], err_block, &[]);
                    bcx.seal_block(ok_block);
                    bcx.seal_block(err_block);
                    tasks.push(Task::ResultDone { root: value, join });
                    tasks.push(Task::Jump(join));
                    tasks.push(Task::Storage {
                        base: value,
                        offset: layout.payload_offset,
                        vty: *err_vty,
                        node: *err,
                    });
                    tasks.push(Task::Switch(err_block));
                    tasks.push(Task::Jump(join));
                    tasks.push(Task::Storage {
                        base: value,
                        offset: layout.payload_offset,
                        vty: *ok_vty,
                        node: *ok,
                    });
                    tasks.push(Task::Switch(ok_block));
                }
            },
        }
    }
    Ok(())
}
