//! sema::exprs — 表达式静态语义模块路由。
//!
//! 子模块直接依赖各自实际 owner；这里只向 sema 其它职责暴露确实共享的窄接口。

mod calls;
mod deep_clone;
mod infer;
mod match_expr;
mod move_value;
mod operators;
mod shallow_clone;
mod typing;

pub(super) use deep_clone::deep_clone_plan_with;
pub(super) use operators::{binary_result_type, conversion_exists, require_value};
pub(super) use shallow_clone::shallow_clone_root_plan_with;
pub(super) use typing::ExprCheckError;
