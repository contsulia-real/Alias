//! Alias 当前可观察行为黄金记录。
//!
//! 本文件断言编译产物/CLI 的精确三元组 (stdout 字节, stderr 字节, 退出码)。
//! 当前语言规范见 `docs/spec-notes.md`；历史变更见 `MIGRATION.md`。
//! 精确字节发生有意变化时，必须在同一批修改中同步规范、黄金记录与迁移说明。

use std::path::PathBuf;
use std::process::Command;

/// 编译产物二进制路径 (cargo 测试基础设施注入)。
const BIN: &str = env!("CARGO_BIN_EXE_alias");

/// 用例输入形态: 内联源码 / 仓库内 demo / 无参数调用。
enum Input {
    /// 源码文本, 运行前写入临时目录
    Inline(&'static str),
    /// 相对 crate 根的既有文件路径, 原样传给二进制
    Demo(&'static str),
    /// 不传任何命令行参数
    NoArgs,
}

/// 一条黄金记录: 名字 + 输入 + 精确期望三元组。
struct Golden {
    name: &'static str,
    input: Input,
    stdout: &'static [u8],
    stderr: &'static [u8],
    exit: i32,
}

// ---------------------------------------------------------------------------
// 黄金记录表 — Span 列坐标按当前 lexer 的实际算法冻结。
// `col` 游标从 1 开始，但 token 起点取 span_here(1)，即对非首列通常表现为
// 视觉列 - 1，同时通过 max(1) 保证不会产生真实 col=0。
// ---------------------------------------------------------------------------

#[rustfmt::skip]
fn golden_table() -> Vec<Golden> {
    vec![
        // ---- demos/count_to_ten.as: 已知良好的 demo 夹具 ----
        // 循环体先 increase i 再 println i, 故打印 1..10 (非 0..9);
        // import 触发标准库尚未接入通知，文本逐字节冻结。
        Golden {
            name: "count_to_ten_demo",
            input: Input::Demo("demos/count_to_ten.as"),
            stdout: b"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n",
            stderr: "[alias] 注意: 1 条 import 已解析但标准库尚未接入 (Phase 5 前)\n".as_bytes(),
            exit: 0,
        },
        // ---- 退出码 = main 返回值 (i32) ----
        Golden {
            name: "arithmetic_exit_48",
            input: Input::Inline(
                "func i32 main = () -> { var i32 x = 6; increase x; val i32 y = x * 7; return y - 1 }\n",
            ),
            stdout: b"",
            stderr: b"",
            exit: 48,
        },
        // ---- main 只接受 i32，其余返回类型在 sema 阶段拒绝 ----
        Golden {
            name: "bool_main_rejected",
            input: Input::Inline("func bool main = () -> { return true }\n"),
            stdout: b"",
            stderr: "错误 @ 1:1 — 顶层 func main 返回类型必须是 i32, 实际 bool\n".as_bytes(),
            exit: 1,
        },
        Golden {
            name: "string_main_rejected",
            input: Input::Inline("func string main = () -> { return 'x' }\n"),
            stdout: b"",
            stderr: "错误 @ 1:1 — 顶层 func main 返回类型必须是 i32, 实际 string\n".as_bytes(),
            exit: 1,
        },
        Golden {
            name: "unit_main_rejected",
            input: Input::Inline("func unit main = () -> { return }\n"),
            stdout: b"",
            stderr: "错误 @ 1:1 — 顶层 func main 返回类型必须是 i32, 实际 unit\n".as_bytes(),
            exit: 1,
        },
        // ---- 字符串插值 'n=$i' 相等比较 ----
        Golden {
            name: "string_interpolation_equality",
            input: Input::Inline(
                "func i32 main = () -> {\n    var i32 i = 4;\n    println ('n=$i' == 'n=4')\n    return 0\n}\n",
            ),
            stdout: b"true\n",
            stderr: b"",
            exit: 0,
        },
        // ---- 除零: 运行时错误, span 为除号左侧操作数 `1`；当前坐标为 2:11 ----
        Golden {
            name: "division_by_zero_error",
            input: Input::Inline("func i32 main = () -> {\n    return 1 / 0\n}\n"),
            stdout: b"",
            stderr: "错误 @ 2:11 — 除以零\n".as_bytes(),
            exit: 1,
        },
        // ---- display 渲染: func→<func>; unit 是无返回值标记，不在 display 域 ----
        Golden {
            name: "display_func",
            input: Input::Inline(
                "func i32 main = () -> {\n    func i32 f = () -> return 5\n    println f\n    return 0\n}\n",
            ),
            stdout: "<func>\n".as_bytes(),
            stderr: b"",
            exit: 0,
        },
        // ---- 顶层副作用顺序: 顶层绑定初始化先于 main 体执行 ----
        Golden {
            name: "top_level_side_effect_ordering",
            input: Input::Inline(
                "func string mk = () -> {\n    println 'top'\n    return 'x'\n}\nval string s = mk()\nfunc i32 main = () -> {\n    println 'main'\n    return 0\n}\n",
            ),
            stdout: b"top\nmain\n",
            stderr: b"",
            exit: 0,
        },
        // ---- 闭包引用捕获: cond 闭包读到外层 n 的最新值 ----
        Golden {
            name: "closure_reference_capture_latest_value",
            input: Input::Inline(
                "func i32 main = () -> {\n    var i32 n = 0;\n    func bool lt3 = (i32 cap) -> return n < cap\n    var i32 rounds = 0;\n    while lt3(3) {\n        increase n\n        increase rounds\n    }\n    return rounds\n}\n",
            ),
            stdout: b"",
            stderr: b"",
            exit: 3,
        },
        // ---- while false 死代码: 条件为假直接落空, 返回 3 ----
        Golden {
            name: "while_false_dead_code",
            input: Input::Inline(
                "func i32 main = () -> {\n    while false {\n        return 7\n    }\n    return 3\n}\n",
            ),
            stdout: b"",
            stderr: b"",
            exit: 3,
        },
        // ---- val 重赋值: span 为赋值目标 `a`；当前坐标为 3:4 ----
        Golden {
            name: "val_reassignment_error",
            input: Input::Inline(
                "func i32 main = () -> {\n    val i32 a = 1;\n    a = 2;\n    return 0\n}\n",
            ),
            stdout: b"",
            stderr: "错误 @ 3:4 — 'a' 是 val 绑定, 不可重新赋值\n".as_bytes(),
            exit: 1,
        },
        // ---- 类型槽强制非空: span 为名字 `x`；当前坐标为 2:8 ----
        Golden {
            name: "missing_type_slot_error",
            input: Input::Inline(
                "func i32 main = () -> {\n    var x = 1;\n    return 0\n}\n",
            ),
            stdout: b"",
            stderr: "错误 @ 2:8 — Var 绑定的类型槽不能为空 — 本语言没有类型推断, 必须显式标注\n".as_bytes(),
            exit: 1,
        },
        // ---- 缺 main: Span::default() 时省略位置前缀 ----
        Golden {
            name: "missing_main_error",
            input: Input::Inline("val i32 x = 1\n"),
            stdout: b"",
            stderr: "找不到顶层 func main\n".as_bytes(),
            exit: 1,
        },
        // ---- 进程边界 clamp: main 返 300 → 255 ----
        Golden {
            name: "exit_code_clamped_to_255",
            input: Input::Inline("func i32 main = () -> {\n    return 300\n}\n"),
            stdout: b"",
            stderr: b"",
            exit: 255,
        },
        // ---- CLI 层: 无参数 → 用法提示, 退出码 2 ----
        Golden {
            name: "no_args_usage_exit_2",
            input: Input::NoArgs,
            stdout: b"",
            stderr: "用法: alias run|build <source.as>\n".as_bytes(),
            exit: 2,
        },
    ]
}

// ---------------------------------------------------------------------------
// 运行器
// ---------------------------------------------------------------------------

/// 每用例独立临时目录: 以用例名 + 进程 PID 命名, 并行测试互不干扰;
/// Drop 时整体清除 (RAII), 断言失败也不遗留垃圾。
struct TempCase {
    dir: PathBuf,
}

impl TempCase {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("alias-golden-{name}-{}", std::process::id()));
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

fn run_binary(args: &[&std::ffi::OsStr]) -> (Vec<u8>, Vec<u8>, Option<i32>) {
    let out = Command::new(BIN)
        .args(args)
        .output()
        .expect("启动 alias 二进制失败");
    (out.stdout, out.stderr, out.status.code())
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
fn golden_triplets() {
    for g in golden_table() {
        let tmp = TempCase::new(g.name);
        let triplet = match g.input {
            Input::Inline(src) => {
                let path = tmp.write(g.name, src);
                run_binary(&[path.as_os_str()])
            }
            Input::Demo(rel) => {
                let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
                run_binary(&[path.as_os_str()])
            }
            Input::NoArgs => run_binary(&[]),
        };
        assert_triplet(g.name, &triplet, g.stdout, g.stderr, g.exit);
    }
}

/// 不存在文件的用例: 期望 stderr 含动态路径, 无法放进静态表, 单独构造。
#[test]
fn missing_file_exit_2() {
    let tmp = TempCase::new("missing_file");
    let path = tmp.dir.join("no_such_file.as");
    let triplet = run_binary(&[path.as_os_str()]);
    let want_stderr = format!(
        "无法读取 {}: 系统找不到指定的文件。 (os error 2)\n",
        path.display()
    );
    assert_triplet(
        "missing_file_exit_2",
        &triplet,
        b"",
        want_stderr.as_bytes(),
        2,
    );
}
