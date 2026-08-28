//! 不可信源码的编译器资源边界。
//!
//! lexer 与 parser 必须共享这些上限；复制数值会让嵌套 token 流或子解析器绕过
//! 顶层预算，造成同一输入策略出现多个事实源。

pub(crate) const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_TOKENS: usize = 200_000;
pub(crate) const MAX_NESTING: usize = 128;
pub(crate) const MAX_EXPR_CHAIN: usize = 256;
