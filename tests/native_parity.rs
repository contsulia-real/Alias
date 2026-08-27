//! Phase 4 编译器黄金基线测试 — 机械枚举 demos/ 语料 + 定向黄金用例。
//!
//! 当前只有 COFF + rust-lld 原生编译后端；
//! 按迁移政策改为**黄金基线断言**: 语料仍由 `std::fs::read_dir` 机械枚举
//! (禁手写列表), 每个 demo 断言冻结的三元组 (stdout 字节/stderr 字节/
//! 退出码)。基线来自 Phase 4 切换时的实际探测 — 新增 demo 必须显式
//! 冻结基线后再入列 (缺失基线即测试失败, 防止静默漂移)。

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

/// 语料基线: 文件名 → 冻结三元组。新增 demo 须在此显式登记。
struct Baseline {
    file: &'static str,
    stdout: &'static [u8],
    stderr: &'static [u8],
    exit: i32,
}

#[rustfmt::skip]
fn corpus_baselines() -> Vec<Baseline> {
    vec![
        // 引用捕获哨兵 demo: 循环打印 1..10 + import 通知 (golden 同源)
        Baseline {
            file: "count_to_ten.as",
            stdout: b"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n",
            stderr: "[alias] 注意: 1 条 import 已解析但标准库尚未接入 (Phase 5 前)\n".as_bytes(),
            exit: 0,
        },
        // Phase 3 全量对等演练夹具 (P3 探测冻结; M40: P2e 无括号泛化后
        // `println wrap 'yo'` 由「误打印 <func>」修复为真实调用 → "[yo]")
        Baseline {
            file: "hello_native.as",
            stdout: "999\n49\n55\n3\n4\n5\n14\n13\n-5\ntrue\nfalse\ntrue\nhey\nn=6\n[yo]\ntrue\ntrue\n3\n42\n".as_bytes(),
            stderr: b"",
            exit: 11,
        },
        // Phase 2a struct 演练夹具 (M23; 探测冻结)
        Baseline {
            file: "structs.as",
            stdout: b"42\n7\n100\n3\n555\n565\n565\n9\n565\ntrue\n565\n<struct>\n",
            stderr: b"",
            exit: 0,
        },
        // Phase 2b result/match/?/转义演练夹具 (M26; 探测冻结)
        Baseline {
            file: "result_match.as",
            stdout: "100/4 = 25\nprobe(5) = 20\nprobe(0) 出错: n 为零\n3\n999\n行数不能为负\n<ok>\n<err>\ntab:[\t]\nnl:[\n]\nquotes:['\"]\nbackslash:[\\]\nNUL 可比较: true\nn 为零\n".as_bytes(),
            stderr: b"",
            exit: 9,
        },
        // forward-spec 文档: match/result/?/转义 已随 Phase 2b 落地可解析;
        // 语料前进至 import 名解析 (open 未定义 — 标准库 Phase 5 前) → sema 拒绝
        Baseline {
            file: "file_wc.as",
            stdout: b"",
            stderr: "错误 @ 34:10 — 未定义的绑定 'open'\n".as_bytes(),
            exit: 1,
        },
        // Phase 2c 演练夹具 (M31/M32; 探测冻结)
        Baseline {
            file: "methods.as",
            stdout: "忠犬\nABC\nabc\n3\nhi\n[]\n[plain]\n3\nHi!\n5\n7\nc(7)\n7\n".as_bytes(),
            stderr: b"",
            exit: 0,
        },
        // Phase 2d array<T> 演练夹具 (M36; 探测冻结)
        Baseline {
            file: "arrays.as",
            stdout: "10\n30\n6\n7\n5\n5\n4\n4\n5\n99\n5\n2\n3\n50\n忠\n3\n犬bc\n<array>\n".as_bytes(),
            stderr: b"",
            exit: 0,
        },
        // forward-spec 文档: 方法已随 Phase 2c 落地可解析; 语料前进至
        // main 存在性 (helper.as 只有方法定义, 无 main) → sema 拒绝
        Baseline {
            file: "helper.as",
            stdout: b"",
            stderr: "找不到顶层 func main\n".as_bytes(),
            exit: 1,
        },
        // forward-spec 文档: channel 泛型语法未实现 → 解析期拒绝
        Baseline {
            file: "producer_consumer.as",
            stdout: b"",
            stderr: "错误 @ 22:43 — 无法开始一个表达式: Some(Gt)\n".as_bytes(),
            exit: 1,
        },
        // forward-spec 文档: P7 字面量模式 match 未实现 (Phase 2b 仅
        // result ok/err 模式) → 解析期拒绝; 顶层裸绑定语句仍未实现但
        // 语料先在 match 臂处失败
        Baseline {
            file: "recursion.as",
            stdout: b"",
            stderr: "错误 @ 14:4 — match 臂构造器必须是 ok 或 err, 实际 Some(Int(0))\n".as_bytes(),
            exit: 1,
        },
    ]
}

/// 机械枚举 demos/*.as — 逐一对照黄金基线; 未登记文件即失败。
#[test]
fn demos_corpus_golden_baselines() {
    let demos_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demos");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&demos_dir)
        .expect("枚举 demos 目录失败")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "as").unwrap_or(false))
        .collect();
    entries.sort(); // 确定性顺序 — 失败信息可复现
    assert!(
        entries.len() >= 2,
        "语料为空? demos 枚举异常: {:?}",
        demos_dir
    );
    let baselines = corpus_baselines();
    for demo in &entries {
        let name = demo.file_name().unwrap().to_string_lossy().to_string();
        let base = baselines
            .iter()
            .find(|b| b.file == name)
            .unwrap_or_else(|| {
                panic!("demo '{name}' 缺少黄金基线 — 请探测后在 corpus_baselines 显式冻结")
            });
        let triplet = run_compiler(demo);
        assert_triplet(&name, &triplet, base.stdout, base.stderr, base.exit);
    }
}

// ---------------------------------------------------------------------------
// 定向黄金用例 — 期望值来自 P0/P1/P3 探测, 编译器为唯一实现后复验
// ---------------------------------------------------------------------------

struct Targeted {
    name: &'static str,
    src: &'static str,
    stdout: &'static [u8],
    stderr: &'static [u8],
    exit: i32,
}

#[rustfmt::skip]
fn targeted_table() -> Vec<Targeted> {
    vec![
        // 闭包引用捕获哨兵: cond 读外层 n 最新值 (smoke closure_reads_latest_value)
        Targeted {
            name: "closure_reads_latest_value",
            src: "\nfunc i32 main = () -> {\n    var i32 n = 0;\n    func bool lt3 = (i32 cap) -> return n < cap\n    var i32 rounds = 0;\n    while lt3(3) {\n        increase n\n        increase rounds\n    }\n    return rounds\n}\n",
            stdout: b"",
            stderr: b"",
            exit: 3,
        },
        // 字符串插值相等比较 → true (golden string_interpolation_equality)
        Targeted {
            name: "string_interpolation_equality",
            src: "\nfunc i32 main = () -> {\n    var i32 i = 4;\n    println ('n=$i' == 'n=4')\n    return 0\n}\n",
            stdout: b"true\n",
            stderr: b"",
            exit: 0,
        },
        // i32 main 的退出码映射保持原样
        Targeted {
            name: "i32_main_exit_1",
            src: "\nfunc i32 main = () -> { return 1 }\n",
            stdout: b"",
            stderr: b"",
            exit: 1,
        },
        // Q⑥ 顶层副作用先于 main 体 (golden top_level_side_effect_ordering)
        Targeted {
            name: "top_level_side_effect_ordering",
            src: "\nfunc string mk = () -> {\n    println 'top'\n    return 'x'\n}\nval string s = mk()\nfunc i32 main = () -> {\n    println 'main'\n    return 0\n}\n",
            stdout: b"top\nmain\n",
            stderr: b"",
            exit: 0,
        },
        // while false 死代码落空 → 3 (golden while_false_dead_code)
        Targeted {
            name: "while_false_dead_code",
            src: "\nfunc i32 main = () -> {\n    while false {\n        return 7\n    }\n    return 3\n}\n",
            stdout: b"",
            stderr: b"",
            exit: 3,
        },
        // 除零中止存根: span-ID 回查原始行:列, 退出码 1 (对齐 golden div_zero)
        Targeted {
            name: "div_zero_abort_span_fidelity",
            src: "\nfunc i32 main = () -> {\n    return 1 / 0\n}\n",
            stdout: b"",
            stderr: "错误 @ 3:11 — 除以零\n".as_bytes(),
            exit: 1,
        },
    ]
}

struct TempCase {
    dir: PathBuf,
}

impl TempCase {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir()
            .join(format!("alias-p4-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录失败");
        TempCase { dir }
    }

    fn write(&self, name: &str, src: &str) -> PathBuf {
        let path = self.dir.join(format!("{name}.as"));
        std::fs::write(&path, src).expect("写入临时源文件失败");
        path
    }
}

impl Drop for TempCase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
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
fn compiler_targeted_golden_cases() {
    for t in targeted_table() {
        let tmp = TempCase::new(t.name);
        let path = tmp.write(t.name, t.src);
        let triplet = run_compiler(&path);
        assert_triplet(t.name, &triplet, t.stdout, t.stderr, t.exit);
    }
}

/// 引用捕获哨兵 (迁移计划显式要求): count_to_ten.as 经默认编译路径
/// 打印 1..10 + import 通知, 退 0。
#[test]
fn count_to_ten_reference_capture_sentinel() {
    let demo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demos/count_to_ten.as");
    let triplet = run_compiler(&demo);
    assert_triplet(
        "count_to_ten_sentinel",
        &triplet,
        b"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n",
        "[alias] 注意: 1 条 import 已解析但标准库尚未接入 (Phase 5 前)\n".as_bytes(),
        0,
    );
}
