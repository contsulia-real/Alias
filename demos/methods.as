// ============================================================
// demos/methods.as — Phase 2c 扩展方法演练夹具
// 覆盖: 方法定义文法 (pub? func <Ret> <Recv>.<name>) /
//   self 隐式不可变绑定 / 结构体方法经 self 写 var 字段 /
//   同名方法跨类型命名空间共存 / 方法链式调用 /
//   内建 len·upper·lower·trim 往返 (含 trim 边界)
// ============================================================

struct counter {
    var i32 n = 0
    val string tag = 'c'
}

// 用户扩展方法: string 拼接 (helper.as 冻结形状)
pub func string string.append = (string tail) -> return '${self}${tail}'

// 同名方法跨类型共存: 命名空间按接收者类型划分
pub func i32 counter.bump = (i32 by) -> {
    self.n = self.n + by
    return self.n
}

// 无参方法 + self 只读 (字段读经插值洞)
pub func string counter.label = () -> return '${self.tag}(${self.n})'

func i32 main = () -> {
    // ---- 用户字符串方法 ----
    val string s = '忠'
    println s.append('犬')

    // ---- 内建四件套往返 ----
    println 'abc'.upper()
    println 'ABC'.lower()
    println s.len()
    println '  hi '.trim()

    // trim 边界: 全空白 → 空串; 无空白 → 原样
    println '[${' \t\r\n '.trim()}]'
    println '[${'plain'.trim()}]'

    // ---- 链式调用: 值流动, 返回类型逐级流入分派 ----
    println 'aBc'.upper().lower().len()
    println '  Hi '.trim().append('!')

    // ---- 结构体方法: self 是不可变绑定, var 字段仍可经它写入 ----
    val counter c = counter()
    println c.bump(5)
    println c.bump(2)
    println c.label()

    // 引用语义穿透方法: 经 self 的字段改动在实例上可见
    println c.n

    return 0
}
