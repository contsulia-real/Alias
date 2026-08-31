// ============================================================
// demos/structs.as — 当前 struct 演练夹具
// 覆盖: 定义 (默认值 + 必填字段) / 乱序命名构造 / 字段读 /
//   var 字段变异 (字段级可变性独立于绑定可变性) / owning binding DeepClone /
//   WriteBorrow 参数 / 嵌套结构体 / 闭包引用捕获 / <struct> 显示
// ============================================================

struct point {
    val i32 x = 0
    val i32 y = 0
}

struct stat {
    val i32 lines = 42
    var i32 bytes
}

struct frame {
    val stat inner
    val point origin
}

// WriteBorrow: caller 为稳定 Place 建立独占 call loan，字段改动对调用方可见
func i32 add_ten = (stat s) -> {
    s.bytes = s.bytes + 10
    return s.bytes
}

func i32 main = () -> {
    // 乱序命名构造: lines 省略 → 取声明默认值 42
    val stat a = stat(bytes = 7)
    println a.lines
    println a.bytes

    // var 字段可变 — 即使绑定本身是 val (字段级可变性)
    a.bytes = 100
    println a.bytes

    // 显式命名实参覆盖默认值
    val stat b = stat(lines = 3, bytes = 4)
    println b.lines

    // owning binding: 稳定 struct Place 读取建立递归独立副本
    val stat alias = a
    alias.bytes = 555
    println a.bytes

    // add_ten 的 s 推导为 WriteBorrow；函数内 +10，调用后可见
    println add_ten(a)
    println a.bytes

    // 嵌套结构体: 稳定 Place 构造实参递归 clone
    val frame f = frame(inner = a, origin = point(y = 9))
    println f.origin.y
    println f.inner.bytes

    // 闭包捕获结构体绑定: 引用捕获读到最新字段值
    var i32 probe = 0
    func bool is_big = () -> {
        probe = f.inner.bytes
        return f.inner.bytes > 500
    }
    println is_big()
    println probe

    // 打印结构体值 → 固定 "<struct>" (与 <func> 对称)
    println a

    return 0
}
