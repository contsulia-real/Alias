//! 链接器适配 — 唯一原生编译管线的 rust-lld 子进程封装。
//!
//! 所有权律: 本模块是"如何把 COFF 字节变成可执行文件"的唯一拥有者;
//! codegen 只产出字节, 不见链接细节。
//!
//! 选型依据 (MIGRATION.md M19): rust-lld.exe 由 rustup 工具链自带,
//! 零新增 crate; lld_rs 绑定停在 LLVM14 且拖 llvm-sys 构建依赖, 违反
//! 加依赖清单律。定位失败时可用环境变量 ALIAS_RUST_LLD 覆盖。

use crate::{AliasError, AliasResult, Span};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempObject(std::path::PathBuf);

impl Drop for TempObject {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn create_temp_object(bytes: &[u8]) -> AliasResult<TempObject> {
    let base = std::env::temp_dir();
    for _ in 0..128 {
        let tick = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!(
            "alias_link_{}_{tick:032x}_{seq:016x}.obj",
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(bytes).map_err(|e| AliasError {
                    msg: format!("无法写出临时目标文件 {}: {e}", path.display()),
                    span: Span::default(),
                })?;
                file.flush().map_err(|e| AliasError {
                    msg: format!("无法刷新临时目标文件 {}: {e}", path.display()),
                    span: Span::default(),
                })?;
                return Ok(TempObject(path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(AliasError {
                    msg: format!("无法创建临时目标文件 {}: {e}", path.display()),
                    span: Span::default(),
                })
            }
        }
    }
    Err(AliasError {
        msg: "无法分配唯一临时目标文件名".into(),
        span: Span::default(),
    })
}

/// SDK 导入库搜索路径发现:
/// 1) %WindowsSdkDir% + %WindowsSdkVersion% → Lib\{ver}\um\x64
/// 2) 兜底: %ProgramFiles(x86)%\Windows Kits\10\Lib\*\um\x64 取最新。
fn sdk_um_x64() -> Option<std::path::PathBuf> {
    if let (Ok(dir), Ok(ver)) = (
        std::env::var("WindowsSdkDir"),
        std::env::var("WindowsSdkVersion"),
    ) {
        let p = std::path::PathBuf::from(dir.trim_end_matches(['\\', '/']))
            .join("Lib")
            .join(ver.trim_end_matches(['\\', '/']))
            .join("um")
            .join("x64");
        if p.join("kernel32.Lib").exists() || p.join("kernel32.lib").exists() {
            return Some(p);
        }
    }
    let program_files = std::env::var_os("ProgramFiles(x86)")?;
    let kits = std::path::PathBuf::from(program_files)
        .join("Windows Kits")
        .join("10")
        .join("Lib");
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(kits) {
        for entry in entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
        {
            candidates.push(entry.path().join("um").join("x64"));
        }
    }
    candidates.sort_by(|a, b| b.cmp(a));
    candidates
        .into_iter()
        .find(|p| p.join("kernel32.Lib").exists() || p.join("kernel32.lib").exists())
}

fn locate_rust_lld() -> AliasResult<std::path::PathBuf> {
    if let Ok(p) = std::env::var("ALIAS_RUST_LLD") {
        return Ok(std::path::PathBuf::from(p));
    }
    let out = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .map_err(|e| AliasError {
            msg: format!("无法调用 rustc 定位 sysroot: {e}"),
            span: Span::default(),
        })?;
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let lld = std::path::PathBuf::from(&sysroot)
        .join("lib")
        .join("rustlib")
        .join("x86_64-pc-windows-msvc")
        .join("bin")
        .join("rust-lld.exe");
    if lld.exists() {
        Ok(lld)
    } else {
        Err(AliasError {
            msg: format!(
                "未找到 rust-lld.exe (期望于 {}); 可用 ALIAS_RUST_LLD 指定",
                lld.display()
            ),
            span: Span::default(),
        })
    }
}

/// 把 COFF 目标文件链接为控制台可执行文件。
/// 入口为产物内导出的 `alias_start` (codegen 发射); 纯 kernel32 依赖 —
/// 无 CRT: 入口显式 ExitProcess, 十进制转换/内存拷贝由 shim 区 IR 实现。
pub fn link_exe(obj_bytes: &[u8], out_exe: &std::path::Path) -> AliasResult<()> {
    let tmp = create_temp_object(obj_bytes)?;

    let lld = locate_rust_lld()?;
    let libpath = sdk_um_x64().map(|p| format!("/LIBPATH:{}", p.display()));

    // 临时目录可能含空格 → 命令行参数由 Command 自动加引号
    let mut args: Vec<String> = vec![
        "-flavor".into(),
        "link".into(),
        "/NOLOGO".into(),
        tmp.0.display().to_string(),
        format!("/OUT:{}", out_exe.display()),
        "/SUBSYSTEM:CONSOLE".into(),
        "/ENTRY:alias_start".into(),
    ];
    if let Some(lp) = &libpath {
        args.push(lp.clone());
    }
    args.push("kernel32.lib".into());

    let out = std::process::Command::new(&lld)
        .args(&args)
        .output()
        .map_err(|e| AliasError {
            msg: format!("无法执行链接器 {}: {e}", lld.display()),
            span: Span::default(),
        });

    let result = out.and_then(|o| {
        if o.status.success() {
            Ok(())
        } else {
            Err(AliasError {
                msg: format!(
                    "链接失败 ({}):\n{}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr)
                ),
                span: Span::default(),
            })
        }
    });
    result
}

#[cfg(test)]
mod tests {
    use super::create_temp_object;
    use std::collections::HashSet;

    #[test]
    fn concurrent_temp_objects_are_unique_and_cleaned_up() {
        let objects = (0u8..32)
            .map(|byte| std::thread::spawn(move || create_temp_object(&[byte]).unwrap()))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();

        let paths = objects
            .iter()
            .map(|object| object.0.clone())
            .collect::<HashSet<_>>();
        assert_eq!(paths.len(), objects.len());
        for (byte, object) in (0u8..32).zip(&objects) {
            assert_eq!(std::fs::read(&object.0).unwrap(), [byte]);
        }
        drop(objects);
        assert!(paths.iter().all(|path| !path.exists()));
    }
}
