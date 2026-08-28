//! parser::exprs — 表达式解析 facade。
//!
//! 实现按职责拆为 precedence / postfix / atoms；子模块直接依赖各自实际 owner，
//! 本 facade 不再充当隐式依赖 barrel。

mod atoms;
mod postfix;
mod precedence;
