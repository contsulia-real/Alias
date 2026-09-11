//! 原生编译破坏性测试：固定种子随机 AST、边界值、深层闭包与并发编译执行。

use alias::run;

/// Destruction must consult ownership state, not stale cell bits after a move.
/// Exercise both reaching states and self-read before committing replacement.
#[test]
fn replacement_preserves_transferred_resources_and_prepares_rhs_first() {
    let src = r#"
struct box { var string text = 'old' }
func i32 main = () -> {
    var i32 total = 0
    for bool take in [true, false] {
        var array<string> source = ['kept']
        var array<string> destination = []
        if take { destination = move source }
        source = ['new']
        if take {
            while destination[0] != 'kept' { return 1 }
        }
        source = source
        while source[0] != 'new' { return 2 }
        val box object = box()
        object.text = '${object.text} value'
        while object.text != 'old value' { return 3 }
        total = total + source.len()
    }
    var string repeated = 'old'
    var i32 count = 0
    while count < 2 {
        repeated = 'step'
        val string consumed = move repeated
        count = count + consumed.len() / 4
    }
    repeated = 'final'
    while repeated != 'final' { return 4 }
    return total
}
"#;
    assert_eq!(run(src).unwrap(), 2);
}

#[test]
fn replacement_destroys_nested_active_payload_and_iterator_state() {
    let src = r#"
struct holder { var result<array<string>, string> payload = ok(['old']) }
func i32 exits_during_rhs = () -> {
    var string value = 'old'
    value = match true {
        true -> return 7
        false -> 'unused'
    }
    return 0
}
func i32 main = () -> {
    var holder value = holder()
    value = holder(payload = err('error'))
    value = holder(payload = ok(['live']))
    val array<i32> left = [1]
    val array<i32> right = [2]
    var iterator<i32> cursor = left.iterator()
    cursor = right.iterator()
    var i32 total = 0
    for i32 item in cursor { total = total + item }
    return match value.payload {
        ok(items) -> total + items[0].len() + exits_during_rhs()
        err(message) -> message.len()
    }
}
"#;
    assert_eq!(run(src).unwrap(), 13);
}

#[test]
fn explicit_local_bindings_are_destroyed_on_every_scope_exit() {
    let src = r#"
func string pass = () -> {
    val string value = 'kept'
    return move value
}
func unit discard = () -> {
    val string value = 'drop'
}
func i32 main = () -> {
    var i32 total = 0
    var i32 i = 0
    while i < 4 {
        val string local = 'x'
        i = i + 1
        if i < 3 { continue }
        total = total + local.len()
        if i == 3 { break }
    }
    if true {
        val string scoped = 'zz'
        total = total + scoped.len()
    }
    val string captured = 'abc'
    func i32 length = () -> return captured.len()
    total = total + length()
    val string output = pass()
    while output != 'kept' { return 7 }
    discard()
    discard()
    return total
}
"#;
    assert_eq!(run(src).unwrap(), 6);
}

#[test]
fn for_and_pattern_bindings_destroy_or_transfer_their_owners() {
    let src = r#"
func i32 main = () -> {
    val array<string> values = ['a', 'bb', 'ccc']
    var array<string> kept = []
    var i32 total = 0
    var i32 i = 0
    for string item in values {
        i = i + 1
        total = total + i
        if i == 2 { continue }
        if i == 3 { break }
        kept.push(move item)
    }
    val string source = 'source'
    val i32 cloned = match source { text -> text.len() }
    val result<string, i32> wrapped = ok('payload')
    val i32 payload = match wrapped {
        ok(text) -> text.len()
        err(_) -> 0
    }
    val string moved = match 'owned' { text -> move text }
    return total + kept[0].len() + cloned + payload + moved.len()
}
"#;
    assert_eq!(run(src).unwrap(), 25);
}

#[test]
fn parameters_release_owned_values_and_preserve_borrowed_referents() {
    let src = r#"
func string choose = (string value, bool take) -> {
    if take { return move value }
    return 'fallback'
}
func string string.select = (bool take) -> {
    if take { return move(self) }
    return 'fallback'
}
func i32 length = (string value) -> return value.len()
func i32 main = () -> {
    val string moved = choose('owned', true)
    while moved != 'owned' { return 1 }
    val string self_moved = 'self'.select(true)
    while self_moved != 'self' { return 2 }
    val string self_fallback = 'discard'.select(false)
    while self_fallback != 'fallback' { return 3 }
    var i32 i = 0
    while i < 64 {
        val string fallback = choose('discard', false)
        while fallback != 'fallback' { return 4 }
        i = i + 1
    }
    val string source = 'source'
    val i32 size = length(source)
    while source != 'source' { return 5 }
    return size - 6
}
"#;
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn process_exit_destroys_global_owner_trees() {
    let src = r#"
struct bundle { val array<string> values = [] }
val bundle global_bundle = bundle(values = ['a', 'bb'])
val result<array<string>, string> global_result = ok(['ccc'])
func i32 main = () -> {
    val i32 payload = match global_result {
        ok(values) -> values[0].len()
        err(message) -> message.len()
    }
    return global_bundle.values[1].len() + payload - 5
}
"#;
    assert_eq!(run(src).unwrap(), 0);
}

#[test]
fn borrowed_call_temporaries_end_after_the_call() {
    let src = r#"
struct bundle { val result<array<string>, string> values = err('') }
func i32 string.measured = () -> return self.len()
func i32 nested_length = (bundle value) -> return match value.values {
    ok(items) -> items[0].len()
    err(message) -> message.len()
}
func i32 main = () -> return nested_length(bundle(values = ok(['temporary']))) + 'receiver'.measured() - 17
"#;
    assert_eq!(run(src).unwrap(), 0);
}

/// Reuse freed interpolation buffers while repeatedly relocating owning array elements.
/// A borrowed hole must survive concatenation; relocation must not destroy its elements.
#[test]
fn internal_string_release_and_array_relocation_preserve_live_values() {
    let src = r#"
func i32 main = () -> {
    val string seed = 'live'
    var array<string> values = []
    var i32 i = 0
    while i < 512 {
        val string text = 'prefix ${seed} suffix'
        while text != 'prefix live suffix' { return 1 }
        values.push(text)
        i = i + 1
    }
    while seed != 'live' { return 2 }
    i = 0
    while i < values.len() {
        while values[i] != 'prefix live suffix' { return 3 }
        i = i + 1
    }
    return 0
}
"#;
    assert_eq!(run(src).unwrap(), 0);
}

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
    assert_eq!(run(&src).unwrap(), 0);
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
    val u64 u64_max = 18446744073709551615
    val u64 u64_one = 1
    val u64 u64_result = (u64_max - u64_one) + u64_one
    while u64_result != u64_max { return 10 }
    return 0
}
"#;
    assert_eq!(run(src).unwrap(), 0);
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
    let actual =
        run(&src).unwrap_or_else(|error| panic!("深层闭包失败: {error}\n--- source ---\n{src}"));
    assert_eq!(actual, expected);
}

#[test]
fn concurrent_native_runs_keep_values_isolated() {
    let workers = (0..16)
        .map(|id| {
            std::thread::spawn(move || {
                let expected = id * 17 + 3;
                let src = format!("func i32 main = () -> {{ val i32 x = {expected} return x }}\n");
                run(&src).unwrap()
            })
        })
        .collect::<Vec<_>>();
    for (id, worker) in workers.into_iter().enumerate() {
        assert_eq!(worker.join().unwrap(), id as i32 * 17 + 3);
    }
}
