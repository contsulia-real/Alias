# Alias 当前语言规范

**状态：** 规范性文档  
**同步日期：** 2026-08-30
**实现基线：** 当前 `main` HEAD

> 本文只描述 Alias **当前有效语义**。历史阶段、已删除语法、旧后端和被后续裁决覆盖的行为不得保留为可与本文竞争的规范。

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

`alias <source.as>` 与 `alias run <source.as>` 是两个正式的一等 CLI 入口，不是兼容别名。`build` 只接受 `.as` 输入。当前对象 ISA、链接器和 SDK 均显式固定到 `x86_64-pc-windows-msvc`：Cranelift 不使用宿主 ISA 探测，链接使用 Windows SDK `kernel32.lib` 和对应目标工具链中的 `rust-lld.exe`；无 CRT，当前平台边界为 Windows x64。

## 2. 前端、typed HIR 与后端边界

### 2.1 parser AST

parser AST 只表达语法，不保存最终静态类型，也不决定调用最终归属。

### 2.2 sema

`sema::check(Program)` 成功后产出 `CheckedProgram`：

- 每个 HIR 表达式携带最终 `Ty`；
- `Call` 携带已解析 `CallTarget`；
- `MethodCall` 携带已解析 `MethodTarget`；
- `CheckedProgram.main_id` 携带 sema 已解析的入口 `BindingId`；
- 结构体构造实参与字段索引的对应关系已经固化，不把 label/name 带到后端重新查找；
- 赋值 target 固化为递归 Place projection；当前可表达 `Local`、`Field(base Place)` 与 `Index(base Place, index fact)`，codegen 不从 receiver Expr 形状恢复 storage identity；
- 当前已知 owning slot 从稳定 Place 普通读取时，source Place 与递归 `DeepClonePlan` 固化为专用 `ReadPlace` HIR；
- 函数字面量返回类型在 sema 合并完成；
- 名字解析、目标类型传播、Pattern 合法性与覆盖、调用目标解析均在进入后端前完成。

显式 `clone`、owning-slot `ReadPlace` 与显式 `shallow` 的 capability 判定及递归执行计划同样在 sema 完成，并固化为对应 plan；codegen 只能执行已解析 plan，不能按 `Ty` / `VTy` 重新决定一个类型是否允许 clone/shallow。

禁止后端通过 AST 形态、名字、函数体、诊断字符串或 fallback 重新推断静态语义。

### 2.3 `Ty → VTy` 单次投影

`src/codegen/abi.rs::project_ty(&CheckedProgram)` 是唯一类型投影入口。codegen 开始时对整棵 HIR 恰执行一次并得到只读投影表；发射阶段只能读取该表。

投影递归保留完整函数签名以及 `result/array/iterator` 内层类型。`Unknown` 是显式不变式状态；不存在 `Other` 或默认回退为 I64。任何需要值 ABI 的 `Unknown` / `unit` 到达 codegen 都属于 sema 缺口，必须失败。

### 2.4 ABI 与 runtime 契约

- `codegen/abi.rs` 是值物理表示、存储宽度、对齐、用户函数签名、结构体布局和载荷字编码的唯一来源；
- `codegen/runtime.rs::RUNTIME_CONTRACTS` 是所有 `alias.*` / `rt.*` runtime 符号参数、返回值和可空性的唯一机器契约；
- `native_runtime.rs` 实际定义集合必须与契约表精确一致。
- 每个函数必须在 `define_function` 前无条件执行 `Context::verify`；禁止使用受 flag 控制、可能跳过检查的 `verify_if`。verifier 失败必须把错误与 IR 作为内部编译错误返回；有返回值函数或 runtime shim 若未显式终止，也必须作为不变式失败，禁止补零返回。

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

### 3.2 显式 deep clone

当前已实现显式 deep clone intrinsic：

```alias
clone(expr)
clone expr
```

规则：

- 恰好接受一个实参，不接受命名实参；
- clone 结果的静态类型与 source 相同；
- 外层已知目标类型会向 source 传播，因此 `val array<i32> a = clone([])` 合法；
- `clone(f)` 中直接函数名 `f` 按函数值本身读取，不触发零参数裸名隐式调用；
- `clone` 是预定义保留名，不能被用户声明 shadow。

当前 `DeepCloneable(T)`：

| 类型 | 当前显式 clone 语义 |
|---|---|
| integer / unsigned / float / bool | 普通值复制；结果仍是 `InlineValue`，不产生独立 ownership capability |
| `string` | 复制独立字符串内容 storage |
| `struct S` | 仅当全部字段均 DeepCloneable；按字段递归 clone |
| `array<T>` | 仅当 `T` DeepCloneable；创建独立 wrapper/backing，并递归 clone 元素 |
| `result<T,E>` | 仅当 `T` 与 `E` 都 DeepCloneable；只递归 clone 当前 active payload |
| `iterator<T>` | 不支持 |
| 函数 / closure / `func` 槽 | 不支持 |

对携带独立动态 ownership 的 DeepCloneable 类型，显式 clone 结果是新的 `OwnedTemporary`，并具有 `Available` ownership capability；对 inline 标量，clone 结果仍是 `InlineValue`，capability 为 `None`。因此“deep clone”不等价于“所有类型都创建动态 owner”。

sema 会把显式 clone 固化为递归 `DeepClonePlan`；final-HIR gate 会重新以同一 DeepCloneable 规则验证该 plan。后端只能执行 plan，不能把 aggregate clone 退化成引用 bit-copy。

### 3.3 显式 shallow clone

当前已实现显式 shallow clone intrinsic：

```alias
shallow(expr)
shallow expr
```

`ShallowCloneable(T)` 与“用户可以直接写 `shallow(T-value)`”是两个不同概念。递归 capability 谓词允许 inline 标量作为安全叶子，但标量本身没有需要新建的独立 aggregate ownership root，因此直接 `shallow(1)` / `shallow(true)` 不开放。

规则：

- 恰好接受一个实参，不接受命名实参；
- shallow 结果的静态类型与 source 相同；
- 外层已知目标类型会向 source 传播；
- `shallow(f)` 中直接函数名 `f` 按函数值本身读取，不触发零参数裸名隐式调用；
- `shallow` 是预定义保留名，不能被用户声明 shadow；
- 每个合法 user-level shallow 都创建新的 aggregate root，因此结果是 `OwnedTemporary`，capability 为 `Available`。

当前递归 `ShallowCloneable(T)` / user-level root 规则：

| 类型 | 递归 shallow 安全性 / 当前入口 |
|---|---|
| integer / unsigned / float / bool | 递归叶子安全；**不能作为 user-level shallow 根** |
| `struct S` | 仅当全部字段递归 shallow-safe；可作为 shallow 根 |
| `result<T,E>` | 仅当 `T` 与 `E` 都递归 shallow-safe；可作为 shallow 根，只复制 active payload 路径 |
| `string` | 不支持 |
| `array<T>` | 不支持 |
| `iterator<T>` | 不支持 |
| 函数 / closure / `func` 槽 | 不支持 |

当前 struct/result 的物理表示仍是 heap pointer root，因此后端执行合法 `ShallowClonePlan` 时会为每个需要独立 ownership 的 shallow-safe aggregate 层建立新的 root；它不会简单复制旧 pointer bit pattern。这样当前实现与目标 ownership 语义保持一致，不会把一个旧 root 指针复制成两个未来 owner。

sema 会把显式 shallow 固化为递归 `ShallowClonePlan`；final-HIR gate 会用同一 capability owner 重新验证 plan。codegen 不重新判断 ShallowCloneable。

### 3.4 owning slot 的稳定 Place 普通读取

当前已经落地的 owning target 包括：非 func binding 初始化、local/field replacement、struct 字段默认值与构造实参、array 字面量元素与 `push` 实参、result 构造 payload。若这些位置的 RHS 是稳定 `Local/Field/Index` Place（允许外层透明 identity conversion），则按最终目标类型执行：

```text
read stable Place
-> DeepClonePlan
-> InlineValue 或 OwnedTemporary
-> 写入 owning target
```

source Place 与递归 plan 在 sema 固化为 `ReadPlace` HIR，final-HIR gate 重新验证 Place/type/plan 一致性，后端只执行已解析计划。动态 DeepCloneable 值得到独立 owner；inline 标量仍是普通值复制。若类型不满足 3.2 的 `DeepCloneable(T)`，这些 owning-slot 普通读取会静态拒绝，不退回引用 bit-copy。

在 function effect 完成后，sema 还会为每个 local/field assignment 固化 destination-side ownership operation：owning Place replacement 明确区分 `InlineCopy` 与 `OwnershipTransfer`，直接赋值给 `var` borrowed alias 固化为 `RebindBorrowedAlias`。ownership CFG、final-HIR gate 与 codegen 共同消费这份结构化事实；后端不再通过 RHS category、target 形状或机器位模式重新猜 replacement / rebind。Binding 初始化与容器写入仍需各自完整 effect/operation 合同，不能从这条 assignment 纵切反推为已经完成。

普通用户函数实参与用户方法 receiver/实参已经按 4.5 的 parameter effect 固化 caller-side ownership/loan 行为，函数返回已经按 4.6 的 return effect 固化 caller-side ownership/loan 行为，不再依赖“当前机器表示碰巧共享”的隐式规则。`for` 循环变量作为新的 owning binding，会按元素静态类型消费 sema 固化并由 final-HIR gate 复核的 `DeepClonePlan`；动态元素不会与容器中仍 live 的 owning element 共用 root。match/Pattern binding 按 7.2 节固化 `InlineCopy / DeepClone / OwnershipTransfer`；for iterable source 尚未完成自身 effect 合同，不能反推为长期语义。局部 borrow/loan 已按 3.6 落地，closure capture loan 已按 4.4 落地，但完整 destruction / free 仍未落地。

### 3.5 显式 move

当前已实现 ownership-transfer intrinsic：

```alias
move(place)
move place
```

当前纵切边界：

- source 必须是完整 local Place；普通 struct field 与 array element 不能被 move-out 后留下 partial-move hole；
- 对 string、struct、array、result、iterator、function/closure 等携带动态 ownership 的 owning local，结果为 `OwnedTemporary + Available`，source capability 进入 moved 状态；
- moved local 在重新初始化前不能读取或再次 move；控制流 merge 与 loop back-edge 采用 may-be-moved 的 fail-closed join；
- 已解析为 owning-slot `ReadPlace` 的读取产生独立副本，不暴露 source alias，因此不阻止后续 move；closure capture 只在其 NLL live region 内阻止冲突 move，尚未解析 effect 的其它共享读取仍按 fail-closed exposure 处理；
- `var` local 可由新的 `OwnedTemporary` 重新初始化；
- scalar `move` 等价于普通 Place 读取，可用于 readable local/parameter/capture/global，不制造或消费动态 ownership capability；
- `target = move(source)` 要求 canonical Place relation 为 `Disjoint`，因此 `x = move(x)` 编译失败；
- `Owned` parameter 是当前函数内的 owning local capability，因此允许 dynamic `move(parameter)` 并由 caller 显式 transfer；captured Place 与 global 的 dynamic move 仍等待各自 transfer source 固化，不会把缺失 effect 当 owning fallback；
- `move(place)` 与 `move place` 使用同一 semantic resolution、resolved Place 和 ownership dataflow。

sema 把操作固化为携带 resolved `Place` 的专用 Move HIR；显式 CFG/worklist ownership analysis 决定程序点 capability，codegen 只读取该 Place 的既有物理值。当前 runtime 仍未实现 destruction/deallocation，所以 move 的静态唯一性已经生效，但资源释放仍受第 18 节的当前泄漏式实现限制。

### 3.6 局部 borrow 与 NLL loan

当前已实现两种等价写法：

```alias
borrow(place)
borrow place
```

当前纵切边界：

- source 必须是当前函数内 owning local 所根植的 resolved `Local/Field/Index` Place；borrowed alias、parameter、capture 与 global 作为 source 仍 fail-closed；
- borrow 结果为 `BorrowedValue`，只能进入 local borrowed binding，不能进入 top-level/global binding、struct field、array element、result payload 或其它 owning storage；
- borrowed slot 的 `val` / `var` 只决定 slot 能否重新绑定，不决定 loan kind；其 NLL live region 内仅读取 referent 时为 `ReadLoan`，实际 write-through 时为 `WriteLoan`；
- 多个重叠 `ReadLoan` 可以共存；`WriteLoan` 对重叠区域独占；owner 对 live loan 的冲突 write/move/reinitialize 会静态失败；冲突判断统一消费 canonical Place relation，`Unknown` 按冲突处理；
- loan 在 alias 最后一次实际使用后结束；控制流分支、merge 与 loop back-edge 由显式 CFG/worklist 计算，不按词法块寿命粗放延长；
- `var` borrowed slot 重新绑定只替换 alias 本身，不算对旧 referent write-through，并为新 source 建立独立 loan generation；
- 普通读取 borrowed Place 进入 owning slot 时执行 `DeepClone`，不把 borrow 偷渡进 owning object graph；
- borrowed alias capture、显式 BorrowedValue 作为用户调用 receiver/argument 时所需的 referent-loan forwarding、borrowed alias generation 的 return forwarding、reborrow 与 terminal Index write-through 尚未开放，缺失合同不会被当作共享引用 fallback；用户函数按 4.6 返回直接 parameter/self/global borrow 已经开放。

sema 将 borrow 固化为携带 `LoanId`、resolved `Place` 与最终 `ReadLoan/WriteLoan` kind 的专用 HIR；final-HIR gate 重算 loan facts 并拒绝漂移。codegen 只物化 borrowed alias cell 与 canonical referent address，不执行 borrow checker，也不从机器地址猜 relation。

---

## 4. 绑定、作用域与函数

绑定种类：

- `val T name = expr`：不可重新绑定；
- `var T name = expr`：可重新绑定；
- `func T name = (...) -> body`：函数绑定，`T` 表示返回类型；
- 函数参数是隐式不可变绑定。

类型槽必须显式书写。Alias 不提供一般声明类型推断。

名字唯一性规则：

- 不同 lexical scope 允许 shadow；
- 同一 lexical scope 禁止重复声明同名 binding；
- 同一参数列表禁止重复参数名；
- 顶层普通 binding 与命名 func 共享名字空间，禁止重名；
- `main` 必须且只能声明一次。

预定义语言名字不属于普通可 shadow 的词法绑定，不能用于用户声明、参数、for 变量或 Pattern 绑定：

- 调用/语句内建：`print`、`println`、`from`、`try_from`、`typeof`、`increase`、`decrease`、`clone`、`shallow`、`borrow`、`move`；
- result 构造器：`ok`、`err`；
- 内建类型名：`i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 bool string unit func result array iterator`。

用户定义的 struct 名不属于上述预定义集合：它与顶层 binding/func 冲突，但词法子作用域中的普通 binding、参数或 Pattern binding 可遮蔽 constructor 名字。

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

闭包环境保存外层 binding cell 的地址；这只是物理寻址方式，不授予绕过静态 loan 的共享修改权限。每个 owning capture 在 HIR 中固化独立 `LoanId`、root `Local Place` 与由闭包体实际使用推导的 `ReadLoan/WriteLoan`：

- capture loan 在 closure value 创建时建立，到该 closure value 的最后一次实际使用结束；未使用闭包不会把 loan 粗放延长到词法块末尾；
- 立即调用的捕获函数字面量使用 expression-temporary loan holder；命名 closure 使用其 binding holder；
- 嵌套 closure 捕获另一个 closure 时，外层 holder 的 live region 会传递保持内层 capture loans，不允许通过转捕获提前结束 referent loan；
- `ReadLoan` 允许其它只读访问，但拒绝冲突 owner write 与 dynamic move；`WriteLoan` 对重叠 Place 独占；inline scalar 的 `move` 仍是 read，因此可与 `ReadLoan` 共存、与 `WriteLoan` 冲突；
- 捕获 closure 不能逃逸进普通持久字段/容器；捕获 borrowed alias 仍因无法固化 rebind-sensitive referent loan generation 而静态拒绝。
- captured dynamic Place 进入 `ReadBorrow` / `WriteBorrow` 用户调用时由 caller argument plan 建立相应 call loan；若 callee 要求 `Owned`，则仍必须先显式产生独立 owner。capture 不是 4.6 当前支持的 borrowed return source，因此直接返回 captured dynamic Place 仍静态拒绝；显式 `clone` 等产生 `OwnedTemporary` 的返回不受此限制。

因此，闭包执行时会通过 cell 观察其获准 live region 内的当前值；但创建读捕获之后再从外部写同一 Place，并不是合法的“按引用更新”捷径。

任何静态类型为完整函数签名的表达式都可作为调用后缀的被调方，包括：

- 标识符；
- `this`；
- 函数字面量；
- 三元表达式结果；
- `match` 结果。

后端统一发射闭包间接调用，不保留按表达式形态分类的白名单。

### 4.5 函数 parameter effects

每个完整函数类型都在 final HIR 前固化与参数同序的 effect：

- `ReadBorrow`：callee 只读，稳定 owning Place 实参在调用期间建立 `ReadLoan`；
- `WriteBorrow`：callee 可写，稳定 owning Place 实参在调用期间建立独占 `WriteLoan`；
- `Owned`：callee 取得 dynamic ownership capability；稳定 owning Place 不能被普通调用暗中 move，实参必须是 `move(place)`、显式 `clone`、合法 `shallow` root 或其它 `OwnedTemporary + Available`；
- inline scalar 仍按值传递，其 canonical effect 为 `Owned`，但不会制造 dynamic ownership capability。

effect 由函数体、用户函数调用和用户方法调用组成的有限格 fixed-point 推导；命名递归、`this`、函数字面量、三元与 `match` 函数值均消费完整 effect signature。求解过程中的保守 lattice join 只用于让递归依赖收敛，不是用户可观察的 effect merge；收敛后，不同分支产生的函数值必须具有完全相同的 parameter effects，不做 effect subtyping 或隐式适配。

每个用户调用在 HIR 中固化 caller-side argument pass：inline、borrow stable Place、borrow temporary 或 ownership transfer。多个 call loan 的 NLL region 覆盖从实参求值到该次调用消费 holder 的区间，并与 local/capture/returned loan 使用同一 canonical Place overlap 规则。显式 BorrowedValue/reborrow forwarding 仍未落地，因此不会借 parameter effect 名义偷渡逃逸引用。

### 4.6 函数 return effects

每个完整函数类型还在 final HIR 前固化返回 effect：

- `Inline`：返回 inline 值；
- `Owned`：返回新的 dynamic owner；`OwnedTemporary` 可直接返回，函数内完整 owning local 可在 `return` 语境隐式 transfer，但该特例不扩散到普通赋值或普通 `Owned` 实参；
- `Borrowed(source)`：返回现有 storage 的借用，当前 source 只允许唯一的 `Parameter(index)`、`Self` 或 `Global(binding)`。同一函数不同返回路径若不能收敛到同一个 source，静态拒绝。

return effect 与 parameter effect 一起按有限 fixed-point 求解。调用方把 borrowed source 精确映射回 receiver/实参/global Place，建立独立 `LoanId`，并以实际最后一次 referent use 决定 NLL region 和 `ReadLoan/WriteLoan`。borrowed return 不能接受 temporary argument，也不能制造 source 原本没有的写权限；返回的 borrowed 标量在机器 ABI 上携带 referent address，不会因声明类型较窄而被截断成普通值。当前 inline scalar parameter 仍按值传参，因此不能作为 borrowed return source；具有稳定 storage 的 global scalar 可以。`main` 必须保持 `Inline i32` return effect，不能把借用地址当成进程退出码。

函数字面量、命名函数、`this`、用户方法，以及三元/`match` 产生的函数值都比较完整 semantic signature：参数类型、parameter effects、返回类型、return effect 与 borrow source 必须精确一致，不做 effect merge、subtyping 或隐式适配。当前仍不支持 borrowed alias generation forwarding、capture 作为 borrowed return source 或多 source lifetime 合并。

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
- owning binding 初始化、local/field replacement、字段默认值与构造实参从稳定 Place 读取时按 3.4 节递归 DeepClone；closure capture 按 4.4 节建立 loan；用户函数参数与方法 receiver/实参按 4.5 节建立 call loan 或显式 transfer，函数返回按 4.6 节建立 caller ownership/loan，Pattern binding 按 7.2 节执行已解析 ownership operation；显式 `clone(instance)` 按 3.2 节创建递归独立副本；满足 3.3 递归 shallow-safe 条件时，显式 `shallow(instance)` 创建独立 aggregate root；
- 字段写权限只由字段自身 `val/var` 决定，与持有实例的绑定是否为 `val/var` 无关；
- 字段赋值的 receiver 链必须解析为已有 storage Place；当前允许从 binding root 经 Field/Index 继续投影，因此 `cells[0].value = 3` 合法，但 constructor/call/ternary 等临时 Value 不能作为字段赋值 receiver，例如 `cell().value = 1` 是编译错误；
- 构造使用命名实参；
- 未显式提供的字段使用声明默认值；
- 字段默认值、构造实参和字段赋值均按字段声明类型进行目标检查；
- 结构体值显示为 `<struct>`；
- 结构体名与普通顶层 binding 处于同一名字空间，局部 binding/参数/Pattern binding 可遮蔽 constructor 名字。

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
- 当前是单编译单元；`pub` 是顶层声明修饰语，不改变单元内可调用性；
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

显式 `clone(result)` 会创建新的 result block，并只对当前 active payload 执行对应递归 DeepClonePlan。`ok/err` 构造 payload 从稳定 Place 读取时同样按 3.4 节 clone。若 `T/E` 都满足当前 shallow-safe 规则，显式 `shallow(result)` 建立新的 result root，并只对 active payload 执行 `ShallowClonePlan`。函数返回按 4.6 节处理；constructor payload Pattern binding 按 7.2 节 clone active payload，不从仍 live 的 result 中 partial move。

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

每个实际产生绑定的 arm 都在 sema 固化 ownership operation，并由 ownership CFG、final-HIR gate 与 codegen 共同消费：inline 标量使用 `InlineCopy`；普通整值绑定若主语是 `OwnedTemporary + Available`，使用 `OwnershipTransfer`；若主语是稳定 Place 或 borrowed value，则按绑定类型固化并执行 `DeepClonePlan`。`ok(name)` / `err(name)` 的 payload 仍属于 live result storage，因此动态 payload 始终 clone，禁止借 Pattern 暗中 partial move。无法证明上述 operation 或绑定类型不满足 DeepCloneable 时静态拒绝，不按当前 pointer bit pattern 退回共享。

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

数组值当前使用 wrapper 引用表示。owning binding/local/field 从稳定 array Place 读取，以及 array 字面量元素、`push` 实参或 `for` 循环变量从稳定 owning element 读取时，按 3.4 节递归 clone；`for` 的 element `DeepClonePlan` 固化在 HIR，不能由后端按机器表示猜测。closure capture 按 4.4 节借用现有 wrapper；用户函数参数与方法 receiver/实参按 4.5 节建立 call loan 或显式 transfer，函数返回按 4.6 节建立 caller ownership/loan，Pattern binding 按 7.2 节执行已解析 ownership operation。显式 `clone(array)` 按 3.2 节执行相同递归复制。`array<T>` 当前不支持显式 shallow。

### 8.1 数组

- 字面量：`[e1, e2, ...]`；
- 空字面量允许由目标 `array<T>` 提供元素类型，包括 `clone([])` 的外层 array 目标；
- 非空元素必须统一到同一元素类型；
- 下标读：`arr[index]`；
- 下标当前只读，`arr[i] = x` 明确拒绝；
- `len()` 返回 `i32`；
- `push(v)` 无返回值；
- `pop()` 返回元素，空数组中止；
- `iterator()` 返回 `iterator<T>`；
- `iterator<T>` 当前不支持显式 clone/shallow；
- 数组值显示为 `<array>`。

当前元素 runtime 载荷槽统一为 8 字节 word；具体静态类型与 ABI 装箱/拆箱由 `abi.rs` 负责。这是当前实现事实，不覆盖 `docs/plan.md` 已冻结的未来 typed stride 目标。

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

不同数值类型之间不做隐式混算。已有变量的静态类型不会因为外层目标而被改型；目标类型只允许影响字面量、`from/try_from`、显式 `clone/shallow` source 和可递归传播的复合表达式。

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
| `clone(expr)` / `shallow(expr)` source | copy intrinsic 表达式的外层目标类型 |
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
 val string copied = clone original
 val Leaf copied_leaf = shallow original_leaf
 val i32 alias = borrow original
 val string transferred = move original
```

关键规则：

- 静态签名为零参数函数的直接标识符或 `this` 可省略调用括号，例如 `val i32 n = five`；`five()` 仍合法；
- 预定义 `clone` / `shallow` / `borrow` / `move` 均支持无括号单参写法，与各自的括号形式语义相同；
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

- 必须存在且只能存在一个顶层 `func main`；
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

## 16. module 当前边界

当前语言没有 `import` 语法、模块加载或标准库导入。源码中的 `import` 按普通未定义语法拒绝；parser AST、sema 和 codegen 均不保留 no-op import 状态。模块能力真正立案前，不得预埋公开语法、静默丢弃插值或在编译时打印阶段提示后继续执行。

---

## 17. Span 与用户诊断

所有面向用户的编译/运行时诊断使用简体中文。

`Span` 当前为：

```text
line / col / len
```

不保存源文件路径。行号从 1 开始。

列坐标按当前 lexer 的实际实现冻结，而不是标准的纯 0-based 或纯 1-based：内部 `col` 游标从 1 开始；token 起点在消费前调用 `span_here(1)`，计算为 `col.saturating_sub(1).max(1)`。因此非首列 token 通常显示为视觉列减 1，同时真实 span 的 `col` 最小仍为 1。

当前可观察锚点：

- `return 1 / 0` 中的 `1` 报 `2:11`；
- 四格缩进后的赋值目标 `a` 报 `3:4`；
- `var x = 1` 中四格缩进后的 `x` 报 `2:8`。

文档和测试不得把该现状简写为“列从 0 开始”或“列从 1 开始”；若未来要正规化坐标系，属于独立可观察行为变更，必须同步实现、黄金记录、规范和迁移记录。

`Span::default()` 的 `0:0:0` 只作为无具体源码位置的哨兵。parser 的 EOF 诊断使用由最后一个 token 推导出的真实 EOF span，不得退回默认 span。普通错误显示：

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
- struct/result/array/iterator 等对象也由原生 runtime/调用端分配并不回收；
- 显式 dynamic `clone` 与合法 aggregate `shallow` 会分配新的相关 storage/root，但当前仍不会在生命周期结束时回收这些 block。

这是**当前实现事实**，不是“已经确定的长期内存管理方案”。在正式加入生命周期管理前，不得在文档中写成 GC、引用计数或已经完整落地的所有权系统。

当前字符串空值、数组空缓冲等可空边由 `RUNTIME_CONTRACTS` 显式规定；调用点不得自行扩大 nullable 范围。

所有运行时中止，包括 iterator fail-fast，统一进入 `RUNTIME_CONTRACTS` 与 native runtime owner；普通 HIR emitter 不直接导入 Windows IO/进程 API。错误消息字节与长度来自同一静态数据表。runtime abort 调用后还会发射 trap，确保错误函数意外返回时仍 fail closed。

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
- `iterator` / function/closure 的显式 clone/shallow；
- string/array 的显式 shallow；
- 标量作为 user-level shallow 根；
- `free` 以及其余尚未落地的计划内显式 ownership/pointer 操作；dynamic capture/global move 仍等待对应 transfer source 分析；
- borrowed alias capture 的 referent-loan forwarding、显式 BorrowedValue 的用户调用 receiver/argument forwarding、borrowed alias generation 的 return forwarding、capture borrowed return source、reborrow、top-level/global borrow 与 terminal Index write-through；
- for iterable 中稳定 dynamic Place 普通读取的 effect 解析；
- 完整 destruction / free 生命周期；
- 旧 `public`；
- 旧 `to_*` 转换入口；
- 解释器/JIT/进程内机器码执行；
- 非 Windows x64 的当前链接产物；
- 已完整落地的内存回收系统。

未实现能力不得通过“防御性兼容”、隐藏 fallback 或预留语法被提前加入。

---

## 21. 文档与变更纪律

1. 本文件只写**当前规范**，不再用“Phase 0/Phase 2 当前……”这类历史层层覆盖的方式描述现在。
2. `AGENTS.md` 记录当前工程结构和维护规则。
3. 专题文档必须明确其当前有效范围以及后来扩展；不能让旧专题描述看起来比本规范更新。
4. 每次语义或架构变更必须在同一批改动中同步代码、法律测试、当前规范和所有受影响专题文档。
5. 项目禁止防御性兼容：新裁决覆盖旧裁决时直接迁移当前状态，并删除被替换的历史形态。
6. 项目永久禁用 CI；验证只能显式手动执行，详见 `NO_CI.md`。
