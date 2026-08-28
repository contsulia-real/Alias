use alias::{build, run};
use std::io::ErrorKind;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // `alias <file>` 与 `alias run <file>` 都是当前正式的一等入口。
    let (cmd, path) = match args.as_slice() {
        [p] => ("run", p.clone()),
        [c, p] if c == "run" || c == "build" => (c.as_str(), p.clone()),
        _ => {
            eprintln!("用法: alias run|build <source.as>");
            return ExitCode::from(2);
        }
    };

    if cmd == "build"
        && !std::path::Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("as"))
    {
        eprintln!("build 输入必须是 .as 源文件");
        return ExitCode::from(2);
    }

    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            // 不把宿主 Windows 的 UI 语言泄漏进 Alias 的可观察契约。
            // NotFound 使用编译器自有的固定文本，其余 I/O 错误保留系统详情。
            if e.kind() == ErrorKind::NotFound {
                eprintln!("无法读取 {path}: 系统找不到指定的文件。 (os error 2)");
            } else {
                eprintln!("无法读取 {path}: {e}");
            }
            return ExitCode::from(2);
        }
    };

    if cmd == "build" {
        // 产物与源文件同目录同名 .exe; 成功静默
        let source_path = std::path::Path::new(&path);
        let out = source_path.with_extension("exe");
        return match build(&src, &out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }

    match run(&src) {
        // 退出码在进程边界 clamp — 编译执行的原生返回值同规约
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
