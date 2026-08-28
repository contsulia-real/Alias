// emit facade — Cranelift 发射按存储、数组、控制流、表达式、运算、字符串、调用拆分。
use super::*;
use super::{Frame, VTy};
use crate::codegen::{invariant_violation, native_err, Compiler, Slot};
use crate::{AliasResult, Span};
use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{Block, BlockArg, InstBuilder, MemFlagsData, Value};
use cranelift_frontend::{FunctionBuilder, Variable};
use cranelift_module::{Linkage, Module};

mod arrays;
mod calls;
mod cells;
mod control;
mod expr;
mod ops;
mod strings;

pub(crate) use arrays::*;
pub(crate) use calls::*;
pub(crate) use cells::*;
pub(crate) use control::*;
pub(crate) use expr::*;
pub(crate) use ops::*;
pub(crate) use strings::*;
