//! Alias 语言实现库: lexer → parser → sema → Cranelift 原生代码生成。
//! 二进制入口在 main.rs, 本 crate 暴露可编程接口供测试与工具使用。

pub mod ast;
pub mod codegen;
pub mod lexer;
pub mod linker;
pub mod parser;
pub mod sema;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// 编译期/运行期错误的统一载体。
/// 统一携带源码内的 line:col:len；当前不保存文件路径。
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
            write!(
                f,
                "错误 @ {}:{} — {}",
                self.span.line, self.span.col, self.msg
            )
        }
    }
}

pub type AliasResult<T> = Result<T, AliasError>;

static RUN_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempExecutable {
    dir: PathBuf,
    path: PathBuf,
}

impl TempExecutable {
    fn create() -> AliasResult<Self> {
        let base = std::env::temp_dir();
        for _ in 0..128 {
            let tick = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = base.join(format!(
                "alias_run_{}_{tick:032x}_{seq:016x}",
                std::process::id()
            ));
            match std::fs::create_dir(&dir) {
                Ok(()) => {
                    let path = dir.join("program.exe");
                    return Ok(Self { dir, path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(AliasError {
                        msg: format!("无法创建临时编译目录 {}: {e}", dir.display()),
                        span: Span::default(),
                    })
                }
            }
        }
        Err(AliasError {
            msg: "无法分配唯一临时编译目录".into(),
            span: Span::default(),
        })
    }
}

impl Drop for TempExecutable {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

/// 编译并运行：完整执行 lex → parse → sema → COFF → rust-lld → 临时 exe，
/// 随后启动该原生进程。不存在 AST 求值或进程内机器码执行路径。
pub fn run(path: &str, src: &str) -> AliasResult<i32> {
    let executable = TempExecutable::create()?;
    build(path, src, &executable.path)?;
    let status = std::process::Command::new(&executable.path)
        .status()
        .map_err(|e| AliasError {
            msg: format!("无法启动编译产物 {}: {e}", executable.path.display()),
            span: Span::default(),
        })?;
    let code = status.code().ok_or_else(|| AliasError {
        msg: "编译产物被外部信号终止".into(),
        span: Span::default(),
    })?;
    Ok(code)
}

/// 唯一编译管线：前端 → COFF 目标文件 → rust-lld → 独立原生可执行文件。
pub fn build(_path: &str, src: &str, out_exe: &Path) -> AliasResult<()> {
    let tokens = lexer::lex(src)?;
    let program = parser::parse(tokens)?;
    let checked = sema::check(program)?;
    let obj = codegen::compile_to_object(checked)?;
    linker::link_exe(&obj, out_exe)
}
