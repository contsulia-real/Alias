//! 链接器适配 — rust-lld 子进程封装 (Phase 5 AOT)。
//!
//! 所有权律: 本模块是"如何把 COFF 字节变成可执行文件"的唯一拥有者;
//! codegen 只产出字节, 不见链接细节。
//!
//! 选型依据 (MIGRATION.md M19): rust-lld.exe 由 rustup 工具链自带,
//! 零新增 crate; lld_rs 绑定停在 LLVM14 且拖 llvm-sys 构建依赖, 违反
//! 加依赖清单律。定位失败时可用环境变量 ALIAS_RUST_LLD 覆盖。

use crate::{AliasError, AliasResult, Span};

/// SDK 导入库搜索路径发现:
/// 1) %WindowsSdkDir% + %WindowsSdkVersion% → Lib\{ver}\um\x64
/// 2) 兜底: 各固定盘 \Windows Kits\10\Lib\*\um\x64 取最新 (本机 SDK 在 D:)
fn sdk_um_x64() -> Option<std::path::PathBuf> {
    if let (Ok(dir), Ok(ver)) = (
        std::env::var("WindowsSdkDir"),
        std::env::var("WindowsSdkVersion"),
    ) {
        let p = std::path::PathBuf::from(dir.trim_end_matches('\\'))
            .join("Lib")
            .join(ver)
            .join("um")
            .join("x64");
        if p.join("kernel32.Lib").exists() || p.join("kernel32.lib").exists() {
            return Some(p);
        }
    }
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for drive in ["C:", "D:", "E:"].iter() {
        let kits = format!("{drive}\\Windows Kits\\10\\Lib");
        if let Ok(entries) = std::fs::read_dir(&kits) {
            for e in entries.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
                candidates.push(e.path().join("um").join("x64"));
            }
        }
        let pf = std::env::var("ProgramFiles(x86)").ok()?;
        let _ = pf;
    }
    candidates.sort_by(|a, b| b.cmp(a));
    candidates.into_iter().find(|p| {
        p.join("kernel32.Lib").exists() || p.join("kernel32.lib").exists()
    })
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
            msg: format!("未找到 rust-lld.exe (期望于 {}); 可用 ALIAS_RUST_LLD 指定", lld.display()),
            span: Span::default(),
        })
    }
}

/// 把 COFF 目标文件链接为控制台可执行文件。
/// 入口为产物内导出的 `alias_start` (codegen 发射); 纯 kernel32 依赖 —
/// 无 CRT: 入口显式 ExitProcess, 十进制转换/内存拷贝由 shim 区 IR 实现。
pub fn link_exe(obj_bytes: &[u8], out_exe: &std::path::Path) -> AliasResult<()> {
    let tmp = std::env::temp_dir().join(format!(
        "alias_link_{}.obj",
        std::process::id()
    ));
    std::fs::write(&tmp, obj_bytes).map_err(|e| AliasError {
        msg: format!("无法写出临时目标文件 {}: {e}", tmp.display()),
        span: Span::default(),
    })?;

    let lld = locate_rust_lld()?;
    let libpath = sdk_um_x64().map(|p| {
        format!("/LIBPATH:{}", p.display())
    });

    // 临时目录可能含空格 → 命令行参数由 Command 自动加引号
    let mut args: Vec<String> = vec![
        "-flavor".into(),
        "link".into(),
        "/NOLOGO".into(),
        tmp.display().to_string(),
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
    let _ = std::fs::remove_file(&tmp);
    result.map_err(|e| e)
}
