//! Alias 唯一原生目标平台合同。
//!
//! codegen 与 linker 必须消费同一 triple；分散字符串会让目标 ISA 与 rust-lld
//! 工具链目录在某次修改后静默漂移。

pub(crate) const TARGET_TRIPLE: &str = "x86_64-pc-windows-msvc";
