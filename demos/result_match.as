// ============================================================
// demos/result_match.as — 当前 result / match 演练夹具
// 覆盖: result<T,E> 内建枚举 / ok·err 构造 / match 表达式
//   (值产出臂 + never 流臂) / ? 同型错误传播 (含穿透循环) /
//   字符串转义 \t \n \' \" \\ \0 / <ok>·<err> 显示
//
// 【边界】无 if 分支 — while 单轮即守卫 (既有惯例);
//   错误换型 (io_error→string 类) 手写 match, ? 只管同型传播 (P6)
// ============================================================

struct stat {
    val i32 lines = 0
    var i32 bytes
}

func result<i32, string> inv100 = (i32 n) -> {
    while n == 0 {
        return err('n 为零')
    }
    return ok(100 / n)
}

// ? 用法: 同型错误 (E=string) 跨 T 传播 — i32 载荷换成 string 载荷
func result<string, string> describe = (i32 n) -> {
    val i32 v = inv100(n)?
    return ok('100/$n = $v')
}

// ? 穿透循环: n==0 那轮的 err 经 ? 直接传出函数 (脱糖的 return 流)
func result<i32, string> probe = (i32 n) -> {
    while n >= 0 {
        val i32 v = inv100(n)?
        return ok(v)
    }
    return err('不可达')
}

func result<stat, string> mk_stat = (i32 ln, i32 by) -> {
    while ln < 0 {
        return err('行数不能为负')
    }
    return ok(stat(lines = ln, bytes = by))
}

// never 流臂: 两臂皆 return 收尾 — 全函数无落空路径 (Q③ 终结性扩展)
func i32 never_arm_demo = () -> {
    match inv100(0) {
        ok(v) -> {
            println v
            return 0
        }
        err(e) -> {
            println e
            return 9
        }
    }
}

func i32 main = () -> {
    // ---- 值产出臂: match 是表达式, 两臂同型给值 ----
    val string d = match describe(4) {
        ok(msg) -> msg
        err(e) -> '失败: $e'
    }
    println d

    // ---- ? 穿透循环: probe(0) 在循环内触发 err 传播 ----
    match probe(5) {
        ok(v) -> println('probe(5) = $v')
        err(e) -> println('probe(5) 出错: $e')
    }
    match probe(0) {
        ok(v) -> println('probe(0) = $v')
        err(e) -> println('probe(0) 出错: $e')
    }

    // ---- 结构体载荷: 臂绑定字段读 + var 字段写 (字段级可变性) ----
    match mk_stat(3, 120) {
        ok(s) -> {
            println s.lines
            s.bytes = 999
            println s.bytes
        }
        err(e) -> println(e)
    }

    // ---- 穷尽匹配取值: 本例命中 err 臂 ----
    val string label = match mk_stat(-1, 0) {
        ok(s) -> '有 ${s.lines} 行'
        err(e) -> e
    }
    println label

    // ---- <ok>/<err> 显示 (运行时 tag 分派, 载荷不泄露) ----
    val result<i32, string> good = ok(7)
    val result<i32, string> bad = err('坏')
    println good
    println bad

    // ---- 转义序列 ----
    println 'tab:[\t]'
    println 'nl:[\n]'
    println 'quotes:[\'\"]'
    println 'backslash:[\\]'
    println 'NUL 可比较: ${'x\0y' == 'x\0y'}'

    return never_arm_demo()
}
