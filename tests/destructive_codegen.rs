//! 原生编译破坏性测试：固定种子随机 AST、边界值、深层闭包与并发编译执行。

use alias::run;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 32) as u32
    }
}

fn random_i32_expr(rng: &mut Lcg, depth: usize) -> (String, i32) {
    if depth == 0 || rng.next().is_multiple_of(5) {
        let value = (rng.next() % 201) as i32 - 100;
        return (value.to_string(), value);
    }
    let (left_src, left) = random_i32_expr(rng, depth - 1);
    let (right_src, right) = random_i32_expr(rng, depth - 1);
    let candidate = match rng.next() % 3 {
        0 => left
            .checked_add(right)
            .map(|value| (format!("({left_src} + {right_src})"), value)),
        1 => left
            .checked_sub(right)
            .map(|value| (format!("({left_src} - {right_src})"), value)),
        _ => left
            .checked_mul(right)
            .map(|value| (format!("({left_src} * {right_src})"), value)),
    };
    candidate.unwrap_or((left_src, left))
}

#[test]
fn deterministic_random_ast_corpus_matches_checked_i32_model() {
    let mut rng = Lcg(0xA11A_5C0D_EC0D_E123);
    let mut src = String::from("func i32 main = () -> {\n");
    for index in 0..160 {
        let (expr, expected) = random_i32_expr(&mut rng, 6);
        src.push_str(&format!("    val i32 v{index} = {expr}\n"));
        src.push_str(&format!(
            "    while v{index} != {expected} {{ return {} }}\n",
            index % 250 + 1
        ));
    }
    src.push_str("    return 0\n}\n");
    assert_eq!(run("random-ast.as", &src).unwrap(), 0);
}

#[test]
fn integer_width_boundaries_accept_results_that_still_fit() {
    let src = r#"
func i32 main = () -> {
    val i8 i8_max = 126
    val i8 i8_one = 1
    val i8 i8_result = i8_max + i8_one
    val i8 i8_min = -128
    while i8_result != 127 { return 1 }
    while i8_min + i8_one != -127 { return 2 }
    val i16 i16_max = 32766
    val i16 i16_one = 1
    val i16 i16_result = i16_max + i16_one
    val i16 i16_min = -32768
    while i16_result != 32767 { return 3 }
    while i16_min + i16_one != -32767 { return 4 }
    val i32 i32_max = 2147483646
    val i32 i32_one = 1
    val i32 i32_result = i32_max + i32_one
    while i32_result != 2147483647 { return 5 }
    val i64 i64_max = 9223372036854775806
    val i64 one = 1
    val i64 i64_result = i64_max + one
    while i64_result != 9223372036854775807 { return 6 }
    val u8 u8_max = 254
    val u8 u8_one = 1
    val u8 u8_result = u8_max + u8_one
    while u8_result != 255 { return 7 }
    val u16 u16_max = 65534
    val u16 u16_one = 1
    val u16 u16_result = u16_max + u16_one
    while u16_result != 65535 { return 8 }
    val u32 u32_max = 4294967294
    val u32 u32_one = 1
    val u32 u32_result = u32_max + u32_one
    while u32_result != 4294967295 { return 9 }
    val u64 u64_max = to_u64(-1)
    val u64 u64_one = 1
    val u64 u64_result = (u64_max - u64_one) + u64_one
    while u64_result != u64_max { return 10 }
    return 0
}
"#;
    assert_eq!(run("boundaries.as", src).unwrap(), 0);
}

fn nested_closure_source(depth: usize) -> String {
    fn body(level: usize, depth: usize) -> String {
        let indent = "    ".repeat(level + 1);
        let mut out = format!("{indent}val i32 x{level} = {}\n", level + 1);
        if level + 1 == depth {
            let sum = std::iter::once("root".to_string())
                .chain((0..depth).map(|i| format!("x{i}")))
                .collect::<Vec<_>>()
                .join(" + ");
            out.push_str(&format!("{indent}return {sum}\n"));
        } else {
            out.push_str(&format!(
                "{indent}func i32 f{} = () -> {{\n{}{indent}}}\n",
                level + 1,
                body(level + 1, depth)
            ));
            out.push_str(&format!("{indent}return f{}()\n", level + 1));
        }
        out
    }

    format!(
        "func i32 main = () -> {{\n    val i32 root = 7\n    func i32 f0 = () -> {{\n{}    }}\n    return f0()\n}}\n",
        body(0, depth)
    )
}

#[test]
fn deep_transitive_closure_capture_chain_is_stable() {
    let depth = 32;
    let expected = 7 + (1..=depth as i32).sum::<i32>();
    let src = nested_closure_source(depth);
    let actual = run("deep-closures.as", &src)
        .unwrap_or_else(|error| panic!("深层闭包失败: {error}\n--- source ---\n{src}"));
    assert_eq!(actual, expected);
}

#[test]
fn concurrent_native_runs_keep_values_isolated() {
    let workers = (0..16)
        .map(|id| {
            std::thread::spawn(move || {
                let expected = id * 17 + 3;
                let src = format!("func i32 main = () -> {{ val i32 x = {expected} return x }}\n");
                run("concurrent-success.as", &src).unwrap()
            })
        })
        .collect::<Vec<_>>();
    for (id, worker) in workers.into_iter().enumerate() {
        assert_eq!(worker.join().unwrap(), id as i32 * 17 + 3);
    }
}
