// ============================================================
// demos/recursion.as — 当前递归 / Pattern 演练夹具
// 覆盖: this 当前函数自引用 / 整数字面量 Pattern / _ catch-all /
//   match 表达式直接产值
// ============================================================

func i32 fact = (i32 n) -> return match n {
    0 -> 1                         // match 是表达式，分支直接产出值
    _ -> n * this(n - 1)           // this 指当前 fact，函数改名不影响递归
}

func i32 main = () -> {
    print('5! = ${fact(5)}')       // 此处 this 指 main，不是 fact
    return 0
}
