//! sema::exprs — 表达式静态语义模块路由。
//!
//! 子模块直接依赖各自实际 owner；这里只向 sema 其它职责暴露确实共享的窄接口。

mod calls;
mod infer;
mod match_expr;
mod operators;
mod typing;

pub(super) use operators::{binary_result_type, conversion_exists, require_value};
pub(super) use typing::ExprCheckError;
