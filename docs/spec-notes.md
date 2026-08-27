# Alias 当前语言规范

**状态：** 规范性文档  
**同步日期：** 2026-08-28  
**实现基线：** `main@a54ad7b0800cafb861e189630898006a30357df1`  
**当前架构里程碑：** M69 — `CheckedProgram` typed HIR + `Ty → VTy` 单次投影

> 本文只描述 Alias **当前有效语义**。历史阶段、已删除语法、旧后端和被后续裁决覆盖的行为只在 `MIGRATION.md` 中保留为历史记录，不得用历史条目覆盖本文。

---

## 1. 编译与执行模型

Alias 是静态类型、原生编译语言。当前唯一执行管线：

```text
lexer → parser AST → sema → CheckedProgram typed HIR
      → Ty→VTy 单次投影 → Cranelift COFF
      → rust-lld → Windows x64 .exe → 独立进程
```

- 不存在解释器；
- 不存在 JIT；
- 不存在宿主 runtime 执行路径；
- 编译器进程不加载或调用生成机器码；
- `run` 与 `build` 共用同一完整编译/链接管线。

CLI：

```text
alias <source.as>          # 等价 alias run <source.as>
alias run <source.as>      # 临时 exe，执行后清理
alias build <source.as>    # 同目录同名 .exe，成功静默
```

`build` 只接受 `.as` 输入。当前链接实现固定使用 Windows SDK `kernel32.lib` 和 `x86_64-pc-windows-msvc` 工具链中的 `rust-lld.exe`；无 CRT，当前平台边界为 Windows x64。

## 2. 前端、typed HIR 与后端边界

### 2.1 parser AST

parser AST 只表达语法，不保存最终静态类型，也不决定调用最终归属。

### 2.2 sema

`sema::check(Program)` 成功后产出 `CheckedProgram`：

- 每个 HIR 表达式携带最终 `Ty`；
- `Call` 携带已解析 `CallTarget`；
- `MethodCall` 携带已解析 `MethodTarget`；
- 函数字面量返回类型在 sema 合并完成；
- 名字解析、目标类型传播、Pattern 合法性与覆盖、调用目标解析均在进入后端前完成。

禁止后端通过 AST 形态、名字、函数体、诊断字符串或 fallback 重新推断静态语义。

### 2.3 `Ty → VTy` 单次投影

`src/codegen/abi.rs::project_ty(&CheckedProgram)` 是唯一类型投影入口。codegen 开始时对整棵 HIR 恰执行一次并得到只读投影表；发射阶段只能读取该表。

投影递归保留完整函数签名以及 `result/array/iterator` 内层类型。`Unknown` 是显式不变式状态；不存在 `Other` 或默认回退为 I64。任何需要值 ABI 的 `Unknown` / `unit` 到达 codegen 都属于 sema 缺口，必须失败。

### 2.4 ABI 与 runtime 契约

- `codegen/abi.rs` 是值物理表示、存储宽度、对齐、用户函数签名、结构体布局和载荷字编码的唯一来源；
- `codegen/runtime.rs::RUNTIME_CONTRACTS` 是所有 `alias.*` / `rt.*` runtime 符号参数、返回值和可空性的唯一机器契约；
- `native_runtime.rs` 实际定义集合必须与契约表精确一致。

---

## 3. 类型系统

当前可用类型：

| 类别 | 类型 |
|---|---|
| 有符号整数 | `i8 i16 i32 i64` |
| 无符号整数 | `u8 u16 u32 u64` |
| 浮点 | `f32 f64` |
| 标量 | `bool string` |
| 函数 | 完整函数签名；类型槽 `func` 为多态函数槽 |
| 用户类型 | `struct` |
| 内建泛型 | `result<T,E>`、`array<T>`、`iterator<T>` |

除上述三种内建泛型外，其它泛型类型尚未实现。

### 3.1 `unit`

`unit` 不是值类型，只表示函数/方法没有返回值。

- 只允许单独作为函数或方法返回类型；
- `()` 在参数表表示零参数，但不是 unit 值；
- `()` 出现在值语法位置是编译错误；
- unit 函数可自然落空或裸 `return`；
- unit 函数不能 `return <expr>`；
- unit 调用只能作为独立表达式语句；
- unit 不得用于绑定、赋值、参数、字段、数组/result/iterator、转换、插值、打印、`typeof` 或其它值位置；
- unit 用户函数的原生签名没有返回槽。

---

## 4. 绑定、作用域与函数

绑定种类：

- `val T name = expr`：不可重新绑定；
- `var T name = expr`：可重新绑定；
- `func T name = (...) -> body`：函数绑定，`T` 表示返回类型；
- 函数参数是隐式不可变绑定。

类型槽必须显式书写。Alias 不提供一般声明类型推断。

当前 `func` 绑定 RHS 必须直接是函数字面量；不能用既有函数值初始化另一个 `func` 绑定。

### 4.1 返回控制流

- 非 unit 函数所有可达落空路径都必须显式 `return <value>`；
- 仅检查最后一条语句不足以证明必返回；
- 循环永不用于证明必返回；
- 全部 arm 都直接返回的 `match` 可作为终结控制流。

### 4.2 当前函数自引用 `this`

每个 func 体内存在不可变 `this`，静态类型为当前函数完整签名。

- 函数改名不影响 `this` 递归；
- 嵌套 func 进入后重新绑定到内层函数；
- func 体外使用 `this` 非法。

### 4.3 命名递归与前向引用

顶层命名函数在检查自己的函数体前先登记自身完整签名，因此可按绑定名直接递归。该规则只建立**自身递归**，不开放后续声明的普通前向引用。

### 4.4 闭包与函数值

闭包引用捕获外层绑定单元格，因此观察到捕获变量的最新值。

任何静态类型为完整函数签名的表达式都可作为调用后缀的被调方，包括：

- 标识符；
- `this`；
- 函数字面量；
- 三元表达式结果；
- `match` 结果。

后端统一发射闭包间接调用，不保留按表达式形态分类的白名单。

---

## 5. `struct`

示意：

```alias
struct User {
    val string name = 'unknown'
    var i32 score = 0
}
```

规则：

- `struct` 只能顶层定义；
- 字段独立声明 `val` 或 `var`；
- 已登记结构体名可进入类型槽；
- 实例采用共享引用语义；赋值、传参、闭包捕获共享同一实例；
- 字段写权限只由字段自身 `val/var` 决定，与持有实例的绑定是否为 `val/var` 无关；
- 构造使用命名实参；
- 未显式提供的字段使用声明默认值；
- 字段默认值、构造实参和字段赋值均按字段声明类型进行目标检查；
- 结构体值显示为 `<struct>`；
- 结构体名与普通顶层绑定处于同一名字空间，局部绑定可遮蔽构造器名字。

结构体物理布局由 `codegen/abi.rs` 按字段声明顺序、各自 size/align 和最大对齐计算，不允许其它模块复制偏移规则。

---

## 6. 扩展方法与 `self`

定义：

```alias
pub func Ret Receiver.method = (Args...) -> body
```

- 方法只能顶层定义；
- RHS 必须为函数字面量；
- `self` 是方法体内隐式不可变接收者，不写入显式参数列表；
- 方法按完整接收者静态类型分派；
- 同一接收者类型内方法名唯一；
- 单编译单元内 `pub` 不改变可调用性，未来模块/import 语义使用该标志；
- `pub` 只允许顶层；
- `public` 已物理退役，不是关键字、别名或兼容语法。

当前编译器内建方法包括：

- `string.len/upper/lower/trim`
- `array<T>.len/push/pop/iterator`
- 数值 `plus/minus/times/div`
- `bool.not`

编译器内建方法不可被同类型用户方法覆盖。非对应内建接收者类型仍可定义自己的同名扩展方法。

---

## 7. `result<T,E>`、`match`、Pattern 与 `?`

### 7.1 result

`result<T,E>` 为二参数内建泛型。`ok(expr)` 与 `err(expr)` 是 result 构造器，不是普通名字分派函数。

单侧构造可暂含 `Unknown`，但必须在目标上下文或后续统一中确定为完整可用类型后才能进入需要 ABI 的路径。

result 值显示为 `<ok>` / `<err>`。

### 7.2 Pattern

当前 Pattern 集：

```alias
_
name
0
true
'text'
ok(value)
ok(_)
err(error)
err(_)
```

- `_`：匹配任意值，不绑定；
- 普通标识符：匹配任意值，并以不可变绑定保存整个主语；
- 整数字面量：仅适用于整数主语，并按主语静态整数类型检查范围；
- bool 字面量：仅适用于 bool；
- 纯字符串字面量：仅适用于 string；插值字符串不能做 Pattern；
- `ok(name|_)` / `err(name|_)`：仅适用于 result，名字绑定对应 payload。

`match` 主语可以是一般静态类型，不限 result。

### 7.3 穷尽性与不可达

- `_` 与普通绑定 Pattern 都是 catch-all；
- catch-all 后的 arm 不可达；
- bool 可由 `true + false` 或 catch-all 穷尽；
- result 可由 `ok + err` 或 catch-all 穷尽；
- 整数/string 等开放域不能通过有限字面量穷尽，必须有 catch-all；
- 重复字面量、重复 `ok/err`、以及完整覆盖后的后续 arm 均为编译错误。

当前未实现：match guard、struct Pattern、嵌套 constructor payload Pattern、用户自定义 Pattern 构造器。

### 7.4 `?`

`expr?` 只适用于 result，并要求当前函数声明返回的 result 错误类型与被传播表达式错误类型相同。语义等价于成功时解包，失败时原样提前返回错误 result。

---

## 8. `array<T>` 与 `iterator<T>`

数组值为共享 wrapper 引用语义。赋值、传参和闭包捕获共享同一 wrapper。

### 8.1 数组

- 字面量：`[e1, e2, ...]`；
- 空字面量允许由目标 `array<T>` 提供元素类型；
- 非空元素必须统一到同一元素类型；
- 下标读：`arr[index]`；
- 下标当前只读，`arr[i] = x` 明确拒绝；
- `len()` 返回 `i32`；
- `push(v)` 无返回值；
- `pop()` 返回元素，空数组中止；
- `iterator()` 返回 `iterator<T>`；
- 数组值显示为 `<array>`。

当前元素 runtime 载荷槽统一为 8 字节 word；具体静态类型与 ABI 装箱/拆箱由 `abi.rs` 负责。

### 8.2 iterator fail-fast

数组 wrapper 维护共享结构版本号。`push/pop` 推进该版本；iterator 保存创建时的 expected version。

任一别名发生结构修改后，旧 iterator 再消费时必须中止并报：

```text
遍历期间集合结构已修改
```

### 8.3 `for`

当前集合迭代语法：

```alias
for T item in iterable {
    ...
}
```

- iterable 当前接受 `array<T>` 或 `iterator<T>`；
- 循环变量为不可重新绑定的隐式 `val`；
- 旧 condition-for 与 C 风格 for 均不存在；条件循环使用 `while`。

---

## 9. 控制流

当前控制流包括：

- `if / else if / else`
- `while bool_expr`
- `for T name in expr`
- `break`
- `continue`
- `match`
- `&& / ||` 短路逻辑
- `condition ? then : else`

`&&` / `||` 只在需要时求值 RHS。三元只求值被选择的值分支。

---

## 10. 数值与运算符

### 10.1 默认整数字面量类型

无目标上下文的正整数字面量：

1. 能放入 `i32` → `i32`
2. 否则能放入 `i64` → `i64`
3. 否则在 `u64` 范围内 → `u64`

负整数字面量最大范围为 `i64`。进入已知整数目标槽时必须先按目标类型检查范围，不允许截断。

### 10.2 类型一致性

不同数值类型之间不做隐式混算。已有变量的静态类型不会因为外层目标而被改型；目标类型只允许影响字面量、`from/try_from` 和可递归传播的复合表达式。

### 10.3 运算符

- 算术：`+ - * / %`
- 比较：`< <= > >= == !=`
- 位：`& | ^ ~ << >>`
- 逻辑：`! && ||`

`%` 仅整数。位运算和移位仅整数，要求操作数同型同宽度。

### 10.4 checked 整数算术

以下操作按声明宽度 checked：

- `+ - *`
- 一元负号
- 左移 `<<`
- `increase/decrease`
- `INT_MIN / -1`

结果超出声明类型范围、无符号下溢、左移丢失有效位或移位数不小于宽度时，编译产物按表达式 Span 输出：

```text
整数溢出
```

并退出 1。不得回绕。

除数为零独立报「除以零」。`& | ^ ~ >>` 保持固定位宽位模式；有符号右移为算术右移，无符号右移为逻辑右移。

Alias 当前不提供复合赋值运算符。

### 10.5 `increase` / `decrease`

```alias
increase x
decrease x
```

它们是独立语句，不是表达式。

- 目标必须是可变数值绑定；
- 整数执行 checked ±1；
- `f32/f64` 执行同型 ±1.0；
- 不能放入赋值、绑定、return、参数、插值或其它值位置。

---

## 11. 转换与目标类型

### 11.1 三种转换入口

```alias
(T) value
from(value)
try_from(value)
```

无括号 `from value` 也合法。

- `(T) value`：显式目标；
- `from`：必须从上下文取得目标类型；若不存在定义的转换关系则编译错误；
- `try_from`：存在转换关系时与 `from` 相同；不存在关系时保留源表达式类型，再由外层普通类型检查决定是否合法。

旧 `to_i8`、`to_i16`、…、`to_f64` 等入口已删除，不保留兼容别名。

### 11.2 当前转换关系

- 所有数值类型之间；
- 所有具有显示规则的具体值 → `string`。

整数目标转换必须检查值域；越界报「转换越界」。浮点转整数同时拒绝非有限值和目标范围外值。已存在转换关系时 `try_from` 的运行时越界不会退回源类型。

### 11.3 目标传播矩阵

| 语境 | 目标来源 |
|---|---|
| `val/var` 初始化 | 声明类型 |
| 普通赋值 | 绑定静态类型 |
| 字段默认值 | 字段声明类型 |
| 结构体构造 / 字段赋值 | 字段类型 |
| `return` | 函数声明返回类型 |
| 函数 / 方法实参 | 形参类型 |
| array 元素 | `T` |
| result payload | `T` 或 `E` |
| match / 三元 | 外层目标类型 |
| 字符串插值孔 | `string` |
| 数值/位复合表达式 | 外层数值目标向字面量与转换递归传播 |

目标表达式检查的类型不一致使用结构化错误携带 `expected / actual / Span`。领域调用方可以把该结构化信息改写成字段、数组元素或实参诊断，但禁止解析已格式化中文文本，也禁止为了取 actual 再执行一次无目标检查。

---

## 12. `typeof`

`typeof(expr)` 与 `typeof expr` 返回静态类型名的 `string`。

- 实参必须通过正常名字解析与静态类型检查；
- 生成代码不求值实参；
- 因此 `typeof(1 / 0)` 不执行除法；
- `result/array/iterator` 返回完整泛型类型名；
- unit 无返回值表达式不能进入 `typeof`。

---

## 13. 无括号调用

Alias 支持已冻结的无括号单参数调用/方法中缀语法，但绑定规则不能被随意放宽。

示例：

```alias
val i32 y = dup 5
value plus 1
println fact 0
```

关键规则：

- 零参数调用必须写括号：`five()`；
- 函数值作为值传递时使用显式括号，例如 `f(g)`；
- `dup 5 + 1` 不解释为 `(dup 5) + 1`，需要显式写 `(dup 5) + 1`；
- `println f 0` / `print f 0` 允许其唯一输出实参本身为普通单参数无括号调用。

---

## 14. 显示与字符串插值

显示规则：

| 静态值类型 | 显示 |
|---|---|
| 有符号整数 | 十进制 |
| 无符号整数 | 无符号十进制 |
| `f32/f64` | runtime 规范化十进制 |
| `bool` | `true` / `false` |
| `string` | 原始字符串字节，不加引号 |
| 函数值 | `<func>` |
| struct | `<struct>` |
| array | `<array>` |
| result | `<ok>` / `<err>` |

`unit` 不属于显示域。

字符串使用单引号并支持插值，例如：

```alias
'name=$name, n=$i'
'${from value}'
```

插值孔的目标类型为 `string`，因此可以触发上下文 `from/try_from` 转换。

---

## 15. main、顶层初始化与进程退出

- 必须存在顶层 `func main`；
- `main` 必须零参数；
- `main` 唯一合法返回类型为 `i32`；
- 顶层绑定按源码顺序初始化，并在 `main` 之前执行；
- CLI 把最终进程退出码 clamp 到 `0..=255`。

缺少 main 使用 `Span::default()`，诊断只输出：

```text
找不到顶层 func main
```

不附加位置前缀。

---

## 16. import / module 当前边界

`import` 目前只被 lexer/parser 接受，不执行真正模块解析、文件加载或标准库导入。

当前编译产物会针对已解析 import 输出“标准库尚未接入”的注意信息。`pub` 已记录在绑定/方法元数据中，但真正跨模块可见性尚未落地。

不得把尚未实现的模块语义提前塞入 parser、sema 或兼容层。

---

## 17. Span 与用户诊断

所有面向用户的编译/运行时诊断使用简体中文。

`Span` 当前为：

```text
line / col / len
```

不保存源文件路径。

**当前 lexer 的真实 span 行号和列号都从 1 开始。** `Span::default()` 的 `0:0:0` 只作为无具体源码位置的哨兵。普通错误显示：

```text
错误 @ {line}:{col} — {message}
```

运行时错误由编译产物使用内嵌 span 表输出，然后以 1 退出；编译器进程本身不执行 runtime。

---

## 18. Runtime 与内存模型当前事实

当前 runtime 采用泄漏式分配，没有回收机制：

- 绑定使用清零单元格；
- 闭包环境保存捕获单元格指针；
- 字符串为复制后的泄漏块；
- struct/result/array/iterator 等对象也由原生 runtime/调用端分配并不回收。

这是**当前实现事实**，不是“已经确定的长期内存管理方案”。在正式加入生命周期管理前，不得在文档中写成 GC、引用计数或所有权系统。

当前字符串空值、数组空缓冲等可空边由 `RUNTIME_CONTRACTS` 显式规定；调用点不得自行扩大 nullable 范围。

---

## 19. 输入健壮性限制

当前硬限制：

- 源文件最大 8 MiB；
- token 最大 200000；
- 组合语法/泛型类型/字符串插值嵌套最大 128；
- 表达式链等最大 256；
- u64 字面量越界、负整数超过 i64、非有限浮点字面量等必须返回带 span 的中文编译错误；
- 不可信输入不得通过这些边界造成 panic 或编译器栈溢出。

---

## 20. 当前未实现/未承诺能力

以下不是当前语言能力：

- 真正 import/module/标准库加载；
- 除 `result/array/iterator` 之外的一般泛型；
- match guard；
- struct Pattern；
- 嵌套 constructor payload Pattern；
- 用户自定义 Pattern 构造器；
- 下标赋值；
- 复合赋值；
- unit 值；
- 旧 `public`；
- 旧 `to_*` 转换入口；
- 解释器/JIT/进程内机器码执行；
- 非 Windows x64 的当前链接产物；
- 已确定的内存回收方案。

未实现能力不得通过“防御性兼容”、隐藏 fallback 或预留语法被提前加入。

---

## 21. 文档与变更纪律

1. 本文件只写**当前规范**，不再用“Phase 0/Phase 2 当前……”这类历史层层覆盖的方式描述现在。
2. `MIGRATION.md` 记录历史演进；历史行中的 JIT、`public`、wrapping、自旧 Pattern 等内容只表示当时状态。
3. `AGENTS.md` 记录当前工程结构和维护规则。
4. 专题文档必须明确其当前有效范围以及后来扩展；不能让旧专题描述看起来比本规范更新。
5. 每次语义或架构变更必须在同一批改动中同步代码、法律测试、当前规范和所有受影响专题文档。
6. 项目禁止防御性兼容：新裁决覆盖旧裁决时直接迁移当前状态，历史只留在迁移账本。
7. 项目永久禁用 CI；验证只能显式手动执行，详见 `NO_CI.md`。
