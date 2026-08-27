use alias::{run, AliasError};

fn fail(src: &str) -> AliasError {
    run("law.as", src).expect_err("该程序应在编译期失败")
}

#[test]
fn operator_methods_share_numeric_semantics() {
    let src = r#"
func i32 main = () -> {
    val i32 a = 10
    val i32 b = 2
    return a.plus(b).minus(3).times(4).div(6)
}
"#;
    assert_eq!(run("operator-methods.as", src).unwrap(), 6);
}

#[test]
fn operator_methods_support_noparen_method_form() {
    let src = r#"
func i32 main = () -> {
    val i32 a = 7
    val i32 b = 5
    return a plus b
}
"#;
    assert_eq!(run("operator-noparen.as", src).unwrap(), 12);
}

#[test]
fn not_method_matches_bang() {
    let src = r#"
func i32 main = () -> {
    val bool a = true.not()
    val bool b = false not
    return (a == !true) && (b == !false) ? 0 : 1
}
"#;
    assert_eq!(run("not-method.as", src).unwrap(), 0);
}

#[test]
fn numeric_operator_methods_preserve_declared_widths() {
    let src = r#"
func i32 main = () -> {
    val i8 a = 10
    val i8 b = 2
    val i8 c = a.plus(b).minus(b).times(b).div(b)
    return to_i32(c)
}
"#;
    assert_eq!(run("operator-width.as", src).unwrap(), 10);
}

#[test]
fn non_numeric_type_can_define_its_own_plus_method() {
    let src = r#"
struct box {
    val i32 value = 0
}

func i32 box.plus = (box other) -> return self.value + other.value

func i32 main = () -> {
    val box a = box(value = 2)
    val box b = box(value = 5)
    return a.plus(b)
}
"#;
    assert_eq!(run("custom-plus.as", src).unwrap(), 7);
}

#[test]
fn func_requires_explicit_return_and_literal_rhs() {
    let implicit = r#"
func i32 f = () -> 1
func i32 main = () -> return 0
"#;
    let e = fail(implicit);
    assert!(e.msg.contains("所有可达路径都必须显式 return"), "{}", e.msg);

    let non_literal_rhs = r#"
func i32 a = () -> return 1
func i32 b = a
func i32 main = () -> return 0
"#;
    let e = fail(non_literal_rhs);
    assert!(e.msg.contains("func 绑定必须由函数字面量初始化"), "{}", e.msg);
}

#[test]
fn short_circuit_and_ternary_only_evaluate_selected_paths() {
    let src = r#"
func i32 main = () -> {
    var i32 hits = 0
    func bool touch = () -> {
        increase hits
        return true
    }
    func i32 left = () -> {
        increase hits
        return 10
    }
    func i32 right = () -> {
        increase hits
        increase hits
        return 20
    }

    val bool a = false && touch()
    val bool b = true || touch()
    val i32 picked = true ? left() : right()
    return hits
}
"#;
    assert_eq!(run("short-circuit.as", src).unwrap(), 1);
}

#[test]
fn for_consumes_array_and_iterator_with_break_continue() {
    let array_src = r#"
func i32 main = () -> {
    var i32 sum = 0
    for i32 x in [1, 2, 3, 4] {
        if x == 2 { continue }
        if x == 4 { break }
        sum = sum + x
    }
    return sum
}
"#;
    assert_eq!(run("for-array.as", array_src).unwrap(), 4);

    let iterator_src = r#"
func i32 main = () -> {
    val array<i32> xs = [1, 2, 3]
    val iterator<i32> it = xs.iterator()
    var i32 sum = 0
    for i32 x in it {
        sum = sum + x
    }
    return sum
}
"#;
    assert_eq!(run("for-iterator.as", iterator_src).unwrap(), 6);
}

#[test]
fn array_iterator_is_invalidated_by_structural_alias_mutation() {
    let src = r#"
func i32 main = () -> {
    val array<i32> xs = [1, 2]
    val array<i32> alias = xs
    for i32 x in xs {
        alias.push(3)
    }
    return 0
}
"#;
    assert_eq!(run("iterator-invalidation.as", src).unwrap(), 1);
}

#[test]
fn old_condition_for_syntax_is_rejected() {
    let src = r#"
func i32 main = () -> {
    for true {
        return 1
    }
    return 0
}
"#;
    assert!(run("old-for.as", src).is_err());
}
