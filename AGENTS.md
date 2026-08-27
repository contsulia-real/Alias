# Alias 项目知识库

**同步日期：** 2026-08-28  
**基线分支：** `main`  
**实现基线提交：** `a54ad7b0800cafb861e189630898006a30357df1`（typed HIR 单次类型投影）

> 本文件只描述 Alias **当前状态**。历史阶段、已删除实现和旧裁决仅记录在 `MIGRATION.md`，不得从历史条目反推当前行为。

## 1. 项目定位与唯一执行模型

Alias 是用 Rust 实现的自研静态类型编译语言，源文件扩展名为 `.as`。

当前唯一管线：

```text
source.as
  → lexer
  → parser AST
  → sema
  → CheckedProgram typed HIR
  → Ty → VTy 单次投影
  → Cranelift Object / COFF
  → rust-lld
  → 独立 Windows x64 .exe
  → 新进程执行
```

不存在解释器、JIT、宿主函数执行后端或进程内调用生成机器码的路径。`run` 也必须先完成完整编译和链接，再启动临时 exe；`build` 输出持久 exe。

当前链接路径是 **x86_64 Windows MSVC**：`src/linker.rs` 固定定位 `x86_64-pc-windows-msvc/bin/rust-lld.exe`，链接 `kernel32.lib`，无 CRT。当前还不是跨平台编译器。

## 2. 当前源码结构

```text
src/
├── main.rs                 # CLI：alias <file> / run / build
├── lib.rs                  # run/build 编排；AliasError / Span
├── lexer.rs                # Token、插值拆分、输入规模限制
├── ast.rs                  # parser AST：只表达语法
├── parser/
│   ├── mod.rs
│   ├── items.rs
│   ├── stmts.rs
│   └── exprs.rs
├── sema/
│   ├── mod.rs              # check(Program) -> CheckedProgram
│   ├── decls.rs
│   ├── stmts.rs
│   ├── exprs.rs
│   ├── types.rs            # Ty 与类型槽检查
│   └── hir.rs              # typed HIR + resolved call targets
├── codegen/
│   ├── mod.rs              # compile_to_object(CheckedProgram)
│   ├── abi.rs              # Ty→VTy 投影、ValueAbi、结构体布局
│   ├── emit.rs             # HIR 表达式/语句发射
│   ├── funcgen.rs          # 用户函数/闭包函数生成
│   ├── runtime.rs          # RUNTIME_CONTRACTS 唯一机器契约表
│   └── native_runtime.rs   # 原生产物内 runtime 实现
└── linker.rs               # COFF → exe 的唯一链接拥有者
```

测试按语言法律拆分，当前包括：`golden`、`sema_laws`、`struct_laws`、`result_laws`、`method_laws`、`array_laws`、`pattern_laws`、`control_flow_operator_laws`、`operator_pub_laws`、`conversion_laws`、`function_value_laws`、`this_laws`、`typeof_laws`、`unit_laws`、`native_pipeline`、`native_parity`、`security_regressions`、`destructive_codegen`、`smoke` 等。

## 3. 当前编译器层级边界

### 3.1 parser AST

`src/ast.rs` 只保存语法结构，不拥有最终静态类型，也不负责调用目标解析。

### 3.2 sema / typed HIR

`sema::check` 成功后产出 `CheckedProgram`。这是进入后端前的语义完成态：

- 每个 HIR 表达式携带最终静态 `Ty`；
- 普通调用携带已解析 `CallTarget`；
- 方法调用携带已解析 `MethodTarget`；
- 匿名函数字面量的返回类型在 sema 完成合并；
- 目标类型传播、名字解析、调用归属、Pattern coverage 等都必须在这里完成。

禁止 codegen 根据 AST 形态、名称、函数体或诊断文本重新猜类型或调用目标。

### 3.3 codegen 类型投影

`src/codegen/abi.rs::project_ty(&CheckedProgram)` 是唯一 `Ty → VTy` 投影入口。进入 codegen 时对整棵 HIR 恰执行一次，后续发射只读取投影表。

`Unknown` 是显式不变式状态；不存在 `VTy::Other` 或“默认退回 I64”。需要值 ABI 的 `Unknown` 或 `unit` 若到达后端，属于 sema 缺口，必须失败。

### 3.4 ABI 与 runtime 单源

- `codegen/abi.rs`：寄存器表示、存储宽度、对齐、参数/返回 ABI、结构体字段布局和 result/array 载荷字编码的唯一真相源；
- `codegen/runtime.rs::RUNTIME_CONTRACTS`：所有 `alias.*` / `rt.*` runtime 符号签名及可空性的唯一机器契约；
- `native_runtime.rs` 的实际定义集合必须与契约表精确一致。

## 4. 当前类型系统

可写类型：

- 有符号整数：`i8 i16 i32 i64`
- 无符号整数：`u8 u16 u32 u64`
- 浮点：`f32 f64`
- `bool`
- `string`
- 函数类型（内部保留完整参数/返回签名；类型槽 `func` 表示多态函数槽）
- 用户 `struct`
- `result<T,E>`
- `array<T>`
- `iterator<T>`

当前其它泛型类型未实现。

`unit` **不是值类型**，只表示函数没有返回值：

- 仅可单独作为函数/方法返回类型；
- `()` 不是值表达式；
- unit 调用只能作为独立语句；
- unit 不得绑定、存储、传参、进入数组/result、转换、插值、打印或 `typeof`；
- 原生函数 ABI 不包含返回槽。

## 5. 绑定、函数与闭包

- `val`：不可重新绑定；
- `var`：可重新绑定；
- 参数是隐式不可变绑定；
- 所有类型槽显式，不存在一般的声明类型推断；
- `func T name = (...) -> ...` 中 `T` 是函数返回类型；当前 `func` RHS 必须直接是函数字面量；
- 顶层命名函数在检查自身函数体前登记自己的完整签名，因此可以按名字递归；这不开放后续声明的前向引用；
- `this` 是每个 func 体内的不可变当前函数自引用，携带完整签名；嵌套 func 重新绑定自己的 `this`；
- 闭包按引用捕获外层绑定单元格，读取捕获变量的最新值；
- 任意静态类型为完整函数签名的表达式均可作为被调方，包括标识符、`this`、函数字面量、三元和 `match` 结果。

非 unit 函数不存在隐式返回：所有可达落空路径必须由显式 `return <value>` 终止。循环不用于证明必返回。

## 6. struct 与扩展方法

### struct

- 顶层定义；
- 字段分别声明 `val` / `var`；
- 实例为共享引用语义；赋值、传参、闭包捕获共享同一实例；
- 字段是否可写只由字段自身 `val/var` 决定，与持有实例的绑定是否为 `val/var` 无关；
- 构造使用命名实参，字段默认值按字段声明目标类型检查；
- 结构体值显示为 `<struct>`。

### 扩展方法

```alias
pub func Ret Receiver.method = (...) -> ...
```

- 方法只能顶层定义；
- `self` 是方法体内隐式不可变接收者；
- 方法按完整接收者静态类型分派；
- `pub` 是唯一公开关键字，只允许顶层；旧 `public` 已删除，不是兼容别名；
- 内建方法不可被用户覆盖。

当前内建包括字符串 `len/upper/lower/trim`，数组 `len/push/pop/iterator`，数值 `plus/minus/times/div`，以及 `bool.not`。

## 7. result / match / Pattern / `?`

`result<T,E>` 是当前内建二参数泛型。`ok(expr)` / `err(expr)` 为构造器。

`match` 主语不限制为 result。当前 Pattern：

- `_`
- 普通标识符整体绑定
- 整数字面量
- `true` / `false`
- 纯字符串字面量
- `ok(name|_)`
- `err(name|_)`

规则：

- `_` 与普通标识符都是 catch-all；普通标识符建立不可变绑定；
- bool 可由 `true + false` 穷尽；
- result 可由 `ok + err` 穷尽；
- 整数/string 等开放域必须存在 catch-all；
- 重复 Pattern、完整覆盖后的后续 arm 均为编译错误；
- guard、struct Pattern、嵌套 constructor payload Pattern、用户 Pattern 构造器尚未实现。

`expr?` 仅用于同错误类型的 `result` 传播。

## 8. array / iterator

`array<T>` 为共享 wrapper 引用语义；别名、传参和闭包捕获共享同一数组状态。

- 字面量：`[a, b, c]`；
- 下标当前只读；`arr[i] = x` 明确拒绝；
- `len()` / `push(v)` / `pop()` / `iterator()` 为内建；
- push/pop 属于结构修改，推进共享版本号；
- iterator 保存创建时版本号，任一别名修改数组结构后旧 iterator 再消费即 fail-fast；
- 越界、负下标、空 pop、失效 iterator 都由编译产物中止并输出中文诊断。

`for Type name in Expr { ... }` 当前消费 `array<T>` 或 `iterator<T>`；循环变量为不可重新绑定的 `val`。

## 9. 控制流与运算

当前控制流：

- `if / else if / else`
- `while`
- `for Type name in Expr`
- `break / continue`
- `&& / ||` 运行时短路
- `?:` 三元，仅求值被选中的分支
- `match`

整数规则：

- 不允许隐式数值混算；
- `%` 仅整数；
- `& | ^ ~ << >>` 仅整数；
- `+ - *`、一元负号、`<<`、`increase/decrease` 使用声明宽度 checked 语义；
- `INT_MIN / -1` 属于整数溢出；除数为零单独报「除以零」；
- `& | ^ ~ >>` 保持固定位宽位模式语义；
- 不提供复合赋值。

`increase name` / `decrease name` 是独立语句，不是表达式。目标必须为可变数值绑定；整数 checked ±1，浮点同型 ±1.0。

## 10. 转换与目标类型传播

当前转换入口：

- `(T) value`：显式指定目标类型；
- `from(value)` / `from value`：必须由上下文提供目标类型；
- `try_from(value)`：若存在转换关系则转换；若不存在关系则保留源表达式类型，再由外层槽正常检查。

旧 `to_*` 内建已物理删除。

转换关系当前覆盖：

- 数值族互转；
- 所有具有显示规则的具体值 → `string`。

整数目标转换做值域检查；越界报「转换越界」，不会回绕或静默截断。

目标类型必须从声明、赋值、字段默认值、结构体字段、return、函数/方法实参、array 元素、result 载荷、match/三元分支、字符串插值和数值复合表达式持续向内传播。不得为拼诊断再次执行无目标类型检查。

## 11. `typeof`

`typeof(expr)` / `typeof expr` 返回表达式的**静态类型名**字符串。

实参仍必须通过名字解析和静态类型检查，但生成代码不求值实参，因此不能触发副作用、除零或其它运行时中止。

## 12. 显示规则

- 整数：十进制
- 浮点：runtime 规范化十进制表示
- bool：`true` / `false`
- string：原文字节，不附加引号
- func：`<func>`
- struct：`<struct>`
- array：`<array>`
- result：`<ok>` / `<err>`

`unit` 不在显示域。

## 13. 诊断与 Span

用户可见诊断统一为简体中文。

当前 `Span` 只包含 `line / col / len`，尚不包含源文件路径。行号从 1 开始。列坐标必须按 **当前 lexer 的实际算法**理解：内部 `col` 游标从 1 开始，但 token 起点在消费前通过 `span_here(1)` 计算，即 `col.saturating_sub(1).max(1)`；因此非首列 token 通常表现为视觉列减 1，而最小列仍为 1。这不是标准的纯 0-based 或纯 1-based 坐标系，文档和测试不得把它简化成其中任一种。当前黄金锚点包括：`return 1 / 0` 中的 `1` 为 `2:11`，四格缩进后的赋值目标 `a` 为 `3:4`。

`Span::default()` 的全零值只作为无具体源码位置的哨兵，例如缺少顶层 `main`，此时 `AliasError::Display` 省略 `错误 @ line:col —` 前缀。

运行时错误由已编译产物根据内嵌 span 数据输出；编译器进程不执行语言 runtime。

## 14. CLI 与平台约束

```text
alias <source.as>          # 等价 run
alias run <source.as>      # 编译临时 exe → 启动 → 清理
alias build <source.as>    # 输出同目录同名 .exe
```

- `build` 输入必须为 `.as`；
- `main` 必须存在、零参数、返回 `i32`；
- CLI 最终退出码把 main/子进程退出码 clamp 到 0–255；
- import 当前只解析，不执行模块加载；标准库尚未接入；
- 当前链接器、SDK 探测和产物格式只支持 Windows x64。

## 15. 输入健壮性边界

- 源文件最大 8 MiB；
- token 最大 200000；
- 语法/类型/字符串插值嵌套最大 128；
- 表达式链等受 256 级上限保护；
- 超限、整数字面量越界、非有限浮点等必须产生中文 `AliasError`，不得 panic。

## 16. 开发硬规则

### 禁止防御性兼容

- 新裁决替换旧状态后直接删除旧语法、旧 AST、旧分支、旧诊断和旧测试夹具；
- 不保留兼容别名、桥接层、fallback、双路径或“以后可能用”的过渡结构；
- 不为未批准未来特性提前铺公共语法或兼容字段；
- 未定义行为按当前规范拒绝，不猜测旧调用方；
- 历史事实只放 `MIGRATION.md`，当前代码和当前规范只能存在一个版本。

### 禁止字符串承载语义控制流

类型、调用解析、错误分类必须使用结构化/类型化通道；不得解析中文诊断字符串恢复类型或控制流信息。

### CI 永久禁用

以 `NO_CI.md` 为硬规则：不得新增、恢复、询问启用 GitHub Actions 或任何其它 CI。验证只能显式手动执行。

## 17. 手动验证命令

```bash
cargo check
cargo build
cargo test --all-targets
cargo clippy --all-targets
```

是否执行哪些命令由当前任务决定；不得把它们包装成 CI。

## 18. 文档责任边界

- `docs/spec-notes.md`：**当前规范**，只写现在时；
- `AGENTS.md`：当前工程结构、边界和维护规则；
- `MIGRATION.md`：历史迁移账本，旧阶段描述允许保留，但不得作为当前规范；
- `docs/pattern-match-foundation.md`：Pattern 专题，必须与当前 Pattern 集一致；
- `docs/recursion-literal-noparen-fixes.md`：相关历史修复专题，必须标注其后续已被哪些当前能力扩展；
- `NO_CI.md`：CI 永久禁用硬规则。

任何语言语义或架构变化必须在同一批改动中同步所有受影响的当前文档；不得只改 `MIGRATION.md` 或只在代码注释里留下新事实。
