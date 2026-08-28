use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_alias");

struct TempSource {
    dir: PathBuf,
    path: PathBuf,
}

impl TempSource {
    fn new(name: &str, src: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "alias-reserved-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        let path = dir.join("case.as");
        std::fs::write(&path, src).expect("写入临时源文件失败");
        Self { dir, path }
    }
}

impl Drop for TempSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn compile(name: &str, src: &str) -> std::process::Output {
    let temp = TempSource::new(name, src);
    Command::new(BIN)
        .arg(&temp.path)
        .output()
        .expect("启动 alias 二进制失败")
}

#[test]
fn predefined_call_names_cannot_be_redeclared() {
    for name in [
        "print", "println", "from", "try_from", "typeof", "increase", "decrease",
    ] {
        let src = format!("val i32 {name} = 1\nfunc i32 main = () -> return 0\n");
        let out = compile(name, &src);
        assert!(!out.status.success(), "预定义名字 {name} 不得允许用户声明");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("预定义名字") && stderr.contains(name),
            "{name} 应由统一保留名字规则拒绝，实际 stderr: {stderr:?}"
        );
    }
}

#[test]
fn result_constructor_names_cannot_be_redeclared() {
    for name in ["ok", "err"] {
        let src = format!("val i32 {name} = 1\nfunc i32 main = () -> return 0\n");
        let out = compile(name, &src);
        assert!(
            !out.status.success(),
            "result 构造器名字 {name} 不得允许用户声明"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("预定义名字") && stderr.contains(name),
            "{name} 应由统一保留名字规则拒绝，实际 stderr: {stderr:?}"
        );
    }
}
