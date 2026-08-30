// ============================================================
// demos/arrays.as — 当前 array<T> 演练夹具
// 覆盖: 字面量构造 / 下标读 / push 越初始容量增长 (realloc 路径) /
//   len / pop LIFO / owning binding DeepClone / 嵌套数组 /
//   数组元素为结构体 (字段读 + 经下标的 var 字段写) / 字符串元素 /
//   插值洞内下标 / <array> 显示
// ============================================================

struct point {
    val i32 x = 0
    val i32 y = 0
}

struct cell {
    var i32 v = 0
}

func i32 main = () -> {
    // ---- 字面量 + 下标读 ----
    val array<i32> xs = [10, 20, 30]
    println xs[0]
    println xs[2]

    // ---- push 增长: 初始 len=cap=1, 再推 5 个必经换缓冲复制路径 ----
    var array<i32> grow = [7]
    grow.push(1)
    grow.push(2)
    grow.push(3)
    grow.push(4)
    grow.push(5)
    println grow.len()
    println grow[0]
    println grow[5]

    // ---- pop LIFO: 后进先出, len 随之收缩 ----
    println grow.pop()
    println grow.pop()
    println grow.len()

    // ---- owning binding: 稳定 array Place 读取建立独立 wrapper/backing ----
    val array<i32> alias = grow
    alias.push(99)
    println grow.len()
    println grow[grow.len() - 1]

    // ---- 嵌套数组: 外层元素仍是数组 ----
    val array<array<i32>> grid = [[1, 2], [3, 4, 5]]
    println grid[1][2]
    println grid[0].len()

    // ---- 数组元素为结构体: 字段读穿透下标 ----
    val array<point> ps = [point(x = 1, y = 2), point(x = 3, y = 4)]
    println ps[1].x

    // ---- 经下标写字段: 字段级可变性独立于绑定可变性 ----
    val array<cell> cs = [cell(), cell(v = 5)]
    cs[1].v = 50
    println cs[1].v

    // ---- 字符串元素: 内建方法与插值同通道 ----
    val array<string> words = ['忠', '犬', 'bc']
    println words[0]
    println words[0].len()
    println '${words[1]}${words[2]}'

    // ---- 打印数组值 → 固定 "<array>" (与 <struct> 对称) ----
    println xs

    return 0
}
