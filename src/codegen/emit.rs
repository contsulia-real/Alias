//! Cranelift 发射 facade：仅声明按职责拆分的 emitter 子模块。
//!
//! 子模块直接依赖实际 contract owner；本 facade 不再通过 glob import 提供偶然可见性。

pub(super) mod arrays;
pub(super) mod calls;
pub(super) mod cells;
pub(super) mod control;
pub(super) mod expr;
pub(super) mod ops;
pub(super) mod places;
pub(super) mod strings;
