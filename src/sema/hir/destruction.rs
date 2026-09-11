//! Resolved destruction recipes. Ownership state decides whether a recipe runs;
//! this owner decides which owned children it destroys, independently of cloneability.

use super::{CheckedProgram, Item};
use crate::sema::types::Ty;
use crate::{AliasError, AliasResult, Span};
use std::collections::{HashMap, HashSet};

/// Flat child indices avoid recursive plan construction, comparison and drop.
/// Node zero is the root; struct children retain declaration order for reverse execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DestroyPlan {
    pub(crate) nodes: Vec<DestroyNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DestroyNode {
    Inline,
    String,
    Iterator,
    Closure,
    Struct { name: String, fields: Vec<usize> },
    Array { element: usize },
    Result { ok: usize, err: usize },
}

pub(super) fn struct_fields(program: &CheckedProgram) -> HashMap<String, Vec<Ty>> {
    program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::StructDef(def) => Some((
                def.name.clone(),
                def.fields.iter().map(|f| f.ty.clone()).collect(),
            )),
            Item::Binding(_) => None,
        })
        .collect()
}

pub(super) fn plan(
    ty: &Ty,
    span: Span,
    structs: &HashMap<String, Vec<Ty>>,
) -> AliasResult<DestroyPlan> {
    enum Task {
        Value(Ty, usize),
        Leave(String),
    }
    let mut nodes = vec![DestroyNode::Inline];
    let mut tasks = vec![Task::Value(ty.clone(), 0)];
    let mut visiting = HashSet::new();
    let invalid = || AliasError {
        msg: "内部 sema 不变式被破坏: destruction 缺少完整的可销毁类型".into(),
        span,
    };
    while let Some(task) = tasks.pop() {
        let (ty, index) = match task {
            Task::Leave(name) => {
                visiting.remove(&name);
                continue;
            }
            Task::Value(ty, index) => (ty, index),
        };
        nodes[index] = match ty {
            Ty::Int(_) | Ty::UInt(_) | Ty::Float(_) | Ty::Bool => DestroyNode::Inline,
            Ty::Str => DestroyNode::String,
            // Iterator owns only cursor state, never the source array or its elements.
            Ty::Iterator(_) => DestroyNode::Iterator,
            Ty::Struct(name) => {
                if !visiting.insert(name.clone()) {
                    return Err(invalid());
                }
                let types = structs.get(&name).ok_or_else(invalid)?;
                tasks.push(Task::Leave(name.clone()));
                let fields: Vec<_> = (nodes.len()..nodes.len() + types.len()).collect();
                nodes.resize(nodes.len() + types.len(), DestroyNode::Inline);
                for (ty, child) in types.iter().zip(&fields).rev() {
                    tasks.push(Task::Value(ty.clone(), *child));
                }
                DestroyNode::Struct { name, fields }
            }
            Ty::Array(ty) => {
                let element = nodes.len();
                nodes.push(DestroyNode::Inline);
                tasks.push(Task::Value(*ty, element));
                DestroyNode::Array { element }
            }
            Ty::Result(ok_ty, err_ty) => {
                let ok = nodes.len();
                let err = ok + 1;
                nodes.resize(err + 1, DestroyNode::Inline);
                tasks.push(Task::Value(*err_ty, err));
                tasks.push(Task::Value(*ok_ty, ok));
                DestroyNode::Result { ok, err }
            }
            Ty::Func { .. } => DestroyNode::Closure,
            Ty::Unit | Ty::Unknown | Ty::FuncPoly => return Err(invalid()),
        };
    }
    Ok(DestroyPlan { nodes })
}
