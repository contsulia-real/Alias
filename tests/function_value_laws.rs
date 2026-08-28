use alias::run;

#[test]
fn ternary_produced_function_value_is_callable() {
    let src = r#"
func i32 plus_one = (i32 value) -> return value + 1
func i32 plus_two = (i32 value) -> return value + 2
func i32 main = () -> {
    val bool first = true
    val bool second = false
    return (first ? plus_one : plus_two)(20) + (second ? plus_one : plus_two)(20)
}
"#;
    assert_eq!(run(src).unwrap(), 43);
}

#[test]
fn match_produced_function_value_is_callable() {
    let src = r#"
func i32 plus_one = (i32 value) -> return value + 1
func i32 plus_two = (i32 value) -> return value + 2
func i32 main = () -> {
    val bool choose = false
    return (match choose {
        true -> plus_one,
        false -> plus_two,
    })(40)
}
"#;
    assert_eq!(run(src).unwrap(), 42);
}

#[test]
fn immediately_invoked_function_literal_uses_the_same_call_path() {
    let src = r#"
func i32 main = () -> {
    return ((i32 value) -> return value + 1)(41)
}
"#;
    assert_eq!(run(src).unwrap(), 42);
}
