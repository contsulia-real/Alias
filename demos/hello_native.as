// Phase 3 原生后端演练夹具 — 全量 Phase-1 对等: 算术(含除法)、循环、
// increase/decrease、函数定义+调用、字符串+插值、闭包引用捕获、
// 一等函数值、println i32/bool/string、Q⑥ 顶层副作用。
// structs/match/channels/imports-execution 不在语言当前范围。

func i32 square = (i32 x) -> return x * x

func i32 fib = (i32 n) -> {
    var i32 a = 0;
    var i32 b = 1;
    var i32 k = 0;
    while k < n {
        val i32 t = a + b;
        a = b;
        b = t;
        increase k
    }
    return a
}

func i32 noisy = () -> {
    println 999
    return 0
}

val i32 marker = noisy()

func string wrap = (string s) -> return '[$s]'


func i32 counter = () -> {
    var i32 c = 0;
    func i32 bump = () -> {
        increase c
        return c
    }
    return bump() + bump()
}

func i32 main = () -> {
    println square(7)
    println fib(10)

    var i32 i = 3;
    while i < 6 {
        println i
        increase i
    }

    var i32 d = 100 / 7;
    println d
    decrease d
    println d

    println (0 - 5)
    println (1 < 2)
    println (2 < 1)
    println (3 == 3)

    val string greet = 'hey'
    println greet
    println 'n=$i'
    println (wrap 'yo')
    println ('abc' == 'abc')
    println ('b' > 'a')

    println counter()
    val i32 imm = (() -> return 42)()
    println imm

    while false {
        return 7
    }
    return 11
}
