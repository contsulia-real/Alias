//! Alias 语言实现库: lexer → parser → sema → Cranelift 原生代码生成。
//! 二进制入口在 main.rs, 本 crate 暴露可编程接口供测试与工具使用。

pub mod ast;
pub mod codegen;
pub mod lexer;
pub mod linker;
pub mod parser;
pub mod sema;

use std::fmt;

/// 编译期/运行期错误的统一载体。
/// 宪法要求"报错提供详细信息": 从第一天起就带 file:line:col。
#[derive(Debug, Clone)]
pub struct AliasError {
    pub msg: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Span {
    pub line: u32,
    pub col: u32,
    pub len: u32,
}

impl fmt::Display for AliasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Q⑤ 裁决: Span 为 default (全零) 时省略位置前缀。
        // 不变式: 真实 span 的 line>=1 且 col>=1 (lexer.rs 初始化 line:1
        // col:1 且 span_here 强制 col>=1), 全零只可能来自 Span::default()
        // 哨兵 — 当前唯一产生点是「找不到顶层 func main」。
        if self.span == Span::default() {
            write!(f, "{}", self.msg)
        } else {
            write!(f, "错误 @ {}:{} — {}", self.span.line, self.span.col, self.msg)
        }
    }
}

pub type AliasResult<T> = Result<T, AliasError>;

/// 唯一编排入口: lex → parse → sema → 原生代码生成 (进程内 JIT 执行)。
/// Phase 4 起编译器为唯一后端; 函数签名与迁移前逐字一致 (smoke.rs 依赖)。
pub fn run(_path: &str, src: &str) -> AliasResult<i32> {
    let tokens = lexer::lex(src)?;
    let program = parser::parse(tokens)?;
    sema::check(&program)?;
    codegen::execute(program)
}

/// AOT 编排: 同一前端管线 → COFF 目标文件 → rust-lld 链接出独立可执行文件。
/// 成功时产物行为与 run() 对同一源程序的行为逐字节一致 (tests/aot_parity.rs)。
pub fn build(_path: &str, src: &str, out_exe: &std::path::Path) -> AliasResult<()> {
    let tokens = lexer::lex(src)?;
    let program = parser::parse(tokens)?;
    sema::check(&program)?;
    let obj = codegen::compile_to_object(program)?;
    linker::link_exe(&obj, out_exe)
}
