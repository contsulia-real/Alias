//! demos/ 当前可执行语料的黄金基线。
//!
//! 本文件只负责一件事：机械枚举 `demos/*.as`，并要求每个 demo 都有明确冻结的
//! `(stdout, stderr, exit)` 三元组。具体语言行为由各 `*_laws` / `golden` / `smoke`
//! 测试拥有，不在这里复制定向用例。

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_alias");

fn run_compiler(path: &Path) -> (Vec<u8>, Vec<u8>, Option<i32>) {
    let out = Command::new(BIN)
        .arg(path)
        .output()
        .expect("启动 alias 二进制失败");
    (out.stdout, out.stderr, out.status.code())
}

struct Baseline {
    file: &'static str,
    stdout: &'static [u8],
    stderr: &'static [u8],
    exit: i32,
}

#[rustfmt::skip]
fn corpus_baselines() -> Vec<Baseline> {
    vec![
        Baseline {
            file: "count_to_ten.as",
            stdout: b"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n",
            stderr: b"",
            exit: 0,
        },
        Baseline {
            file: "hello_native.as",
            stdout: "999\n49\n55\n3\n4\n5\n14\n13\n-5\ntrue\nfalse\ntrue\nhey\nn=6\n[yo]\ntrue\ntrue\n3\n42\n".as_bytes(),
            stderr: b"",
            exit: 11,
        },
        Baseline {
            file: "structs.as",
            stdout: b"42\n7\n100\n3\n100\n110\n110\n9\n110\nfalse\n110\n<struct>\n",
            stderr: b"",
            exit: 0,
        },
        Baseline {
            file: "result_match.as",
            stdout: "100/4 = 25\nprobe(5) = 20\nprobe(0) 出错: n 为零\n3\n999\n行数不能为负\n<ok>\n<err>\ntab:[\t]\nnl:[\n]\nquotes:['\"]\nbackslash:[\\]\nNUL 可比较: true\nn 为零\n".as_bytes(),
            stderr: b"",
            exit: 9,
        },
        Baseline {
            file: "methods.as",
            stdout: "忠犬\nABC\nabc\n3\nhi\n[]\n[plain]\n3\nHi!\n5\n7\nc(7)\n7\n".as_bytes(),
            stderr: b"",
            exit: 0,
        },
        Baseline {
            file: "arrays.as",
            stdout: "10\n30\n6\n7\n5\n5\n4\n4\n4\n3\n5\n2\n3\n50\n忠\n3\n犬bc\n<array>\n".as_bytes(),
            stderr: b"",
            exit: 0,
        },
        Baseline {
            file: "recursion.as",
            stdout: b"5! = 120",
            stderr: b"",
            exit: 0,
        },
    ]
}

fn assert_triplet(
    case: &str,
    got: &(Vec<u8>, Vec<u8>, Option<i32>),
    want_stdout: &[u8],
    want_stderr: &[u8],
    want_exit: i32,
) {
    let (stdout, stderr, code) = got;
    assert_eq!(
        *code,
        Some(want_exit),
        "[{case}] 退出码不符: 期望 {want_exit}, 实际 {code:?}"
    );
    assert_eq!(
        stdout.as_slice(),
        want_stdout,
        "[{case}] stdout 字节不符:\n  实际: {:?}\n  期望: {:?}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(want_stdout),
    );
    assert_eq!(
        stderr.as_slice(),
        want_stderr,
        "[{case}] stderr 字节不符:\n  实际: {:?}\n  期望: {:?}",
        String::from_utf8_lossy(stderr),
        String::from_utf8_lossy(want_stderr),
    );
}

#[test]
fn demos_have_frozen_current_baselines() {
    let demos_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demos");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&demos_dir)
        .expect("枚举 demos 目录失败")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().map(|ext| ext == "as").unwrap_or(false))
        .collect();
    entries.sort();

    let baselines = corpus_baselines();
    assert_eq!(
        entries.len(),
        baselines.len(),
        "demos 数量与冻结基线数量不一致"
    );

    for demo in &entries {
        let name = demo
            .file_name()
            .expect("demo 必须有文件名")
            .to_string_lossy();
        let baseline = baselines
            .iter()
            .find(|baseline| baseline.file == name)
            .unwrap_or_else(|| panic!("demo '{name}' 缺少当前黄金基线"));
        let triplet = run_compiler(demo);
        assert_triplet(
            &name,
            &triplet,
            baseline.stdout,
            baseline.stderr,
            baseline.exit,
        );
    }
}
