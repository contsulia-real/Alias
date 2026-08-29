# Alias 内存、所有权与指针模型

## 0. 文档状态

本文定义 Alias 下一阶段的内存、所有权、借用、销毁、raw allocation 与 pointer 语义设计。

本文不是对当前实现的兼容说明。当前实现中与本文冲突、语义错误、仅为历史阶段服务或无法支撑本文不变量的结构都可以重构或删除。

本文的目标是先冻结**语言语义与编译器合同**，再决定具体机器表示。

当前唯一明确保留为未决设计闸门的是：

```text
ptr<T> 的最终 runtime representation 与 ABI
```

在该闸门关闭前，sema、HIR 与中间层不得假设 pointer 是一个机器字、两个机器字、裸地址、句柄或其它具体表示。

---

# 1. 总体目标

Alias 使用编译期可验证的 ownership / borrow 模型，同时保留受控的底层内存能力。

核心目标：

* 一个动态 ownership resource 在任意时刻只有一个可消费的 ownership capability；
* borrow 不取得 ownership，也不延长 owner 生命周期；
* ownership 可以显式转移；
* 编译器阻止 use-after-move、use-after-free、double free、悬空 borrow、冲突 borrow 等错误；
* 不要求用户书写 Rust 风格生命周期参数；
* Alias 永远不提供 `unsafe`；
* 支持受控的 `malloc` / `free`、pointer arithmetic、`refer` / `deref` / `reinterpret`；
* 普通语言对象由编译器自动管理销毁；
* raw allocation 可以部分初始化，但初始化状态必须可验证；
* sema 决定最终内存语义，HIR 固化结果，codegen 不重新猜测。

---

# 2. 三个必须分离的层次

Alias 的实现必须严格区分：

```text
compile-time ownership capability
runtime allocation identity / provenance
runtime initialization metadata
```

它们不是同一个概念。

## 2.1 Compile-time ownership capability

ownership capability 是 sema / 数据流分析中的静态事实。

它回答：

> 当前 Place / Value 是否仍携带消费、转移或结束某个 ownership resource 的唯一权限？

概念状态至少包括：

```text
None
Available
Moved
Consumed
```

具体内部表示由实现决定，但 capability 不可复制。

## 2.2 Runtime allocation identity / provenance

一次真实程序执行中的每次 allocation 都具有自己的 runtime identity / provenance。

同一个 `malloc` 语法节点可以在循环、递归或多次调用中产生任意多个不同 allocation。

因此：

```text
compile-time ownership capability
!=
runtime allocation identity
```

sema 只能静态决定 ownership flow、pointer operation 与哪些 runtime check 必须存在；不能把某个动态 allocation identity 当成编译期常量。

## 2.3 Runtime initialization metadata

raw allocation 允许部分初始化和 hole，因此运行时可能需要维护哪些 byte range 当前承载合法对象。

这属于 allocation runtime metadata，不属于用户类型，也不是 ownership capability 本身。

---

# 3. Place 与 Value

Alias 内存语义必须首先区分：

```text
Place
Value
```

## 3.1 Place

Place 表示一个确定的存储位置。

典型 Place：

```text
local binding slot
struct field
array element
pointer dereference
pointer indexing
```

Place 本身不等价于“读取值”。

外层操作决定对 Place 执行：

```text
read
write
clone
shallow clone
borrow
move
refer
initialize
replace
destroy
```

## 3.2 Value

一个表达式完成求值后得到 Value。

语义上至少区分：

```text
InlineValue
OwnedTemporary
BorrowedValue
Null
```

pointer value 仍属于 Value，但其最终机器表示由 pointer ABI 设计闸门决定。

### InlineValue

整数、浮点、bool 等不具有独立动态 ownership 的普通值。

### OwnedTemporary

一个已经完整构造、携带唯一 ownership capability、尚未固定进入某个 owning Place 的临时值。

来源包括：

```text
struct / array / result 等新构造结果
函数 owned return
clone(...)
shallow(...)
move(place)
malloc(...) 成功产生的新 allocation root
其它未来产生新 owner 的表达式
```

`malloc` 不具有特殊赋值规则；它只是 `OwnedTemporary` 的一种来源。

### BorrowedValue

引用某个 live owner，但不携带 ownership capability 的值。

borrow 生命周期由 loan analysis 管理。

### Null

`null` 不携带 ownership，也不自行决定具体 nullable 类型。

---

# 4. Storage relation

用户可绑定位置具有 ownership relation：

```text
Owning
Borrowed
```

relation 是 slot 的长期合同，不由每次 RHS 任意改变。

`val` / `var` 只决定名字能否重新绑定，与 ownership relation 正交。

## 4.1 Owning slot

owning slot 可以持有：

```text
InlineValue
OwnedTemporary
null（若类型 nullable）
```

owning slot 不能直接保存 BorrowedValue。

## 4.2 Borrowed slot

borrowed slot 只能保存：

```text
BorrowedValue
null（若类型 nullable）
```

borrowed slot 永远不能获得 ownership capability。

## 4.3 Local relation

局部 binding 的 relation 在初始化时确定。

典型规则：

```text
OwnedTemporary    -> owning
clone(...)        -> owning
shallow(...)      -> owning
move(...)         -> owning
borrow(...)       -> borrowed
Borrowed return   -> borrowed
```

仅由 `null` 初始化的 nullable local：

```alias
var T? x = null
```

初始：

```text
relation = unresolved
nullness = null
```

第一次接收 non-null 值时固定为 owning 或 borrowed。

如果不同可达控制流路径要求同一个 unresolved local 同时成为 owning 与 borrowed，则编译失败。

---

# 5. 禁止 stored borrow

Alias 不允许 non-owning relation 被保存进用户持久对象图。

以下用户可持久存储位置全部是 owning slot：

```text
struct field
array element
result payload
其它普通容器 element / payload
```

它们不能保存 BorrowedValue，也不能保存由 owning Place 派生出的 non-owning pointer view。

因此 Alias v1 的普通用户对象图只包含 owning edge，不包含 persistent borrow edge。

borrow 只允许存在于可由静态数据流分析控制生命周期的语境，例如：

```text
local borrowed binding
function parameter borrow
function borrowed return
expression temporary
closure capture
```

这意味着 Alias v1 不提供通过普通字段/容器长期保存 parent pointer、observer pointer、weak back-reference 等 stored borrow 能力。

未来如果需要此类能力，必须作为独立设计引入稳定 handle / weak handle 或新的受控机制，不能偷偷放宽普通 field 的 relation。

---

# 6. Owning graph

普通用户 ownership graph 不允许 ownership cycle。

例如：

```text
A owns B
B owns C
```

合法。

但：

```text
A owns B
B owns A
```

非法。

inline value 不形成独立 ownership edge。

临时 loan relation 可以在分析上形成相互引用关系，但它们不是 stored object graph，且必须全部满足 lifetime / conflict 规则。

---

# 7. 基本值操作

正式核心操作：

```alias
clone(x)
shallow(x)
borrow(x)
move(x)
```

普通 `=` 仍提供便利语义，但不再把所有 RHS 简化成同一种“复制”。

## 7.1 `clone(x)`

`clone(x)` 执行 deep clone。

source 可以是合法 readable owning Place，也可以是合法 readable borrowed Place / BorrowedValue。

结果是新的：

```text
OwnedTemporary
```

deep clone：

* 递归复制 ownership subtree；
* 不取得 source ownership；
* 不改变 source 既有 loan；
* 新结果拥有独立 ownership capability。

只有 `DeepCloneable(T)` 为真时才合法。

## 7.2 `shallow(x)`

`shallow(x)` 产生新的 root owner，但只复制允许安全 shallow-copy 的直接 representation。

结果是：

```text
OwnedTemporary
```

是否允许由 `ShallowCloneable(T)` 决定。

判定必须递归考虑 inline 子对象是否包含 ownership capability，不能只检查“当前层是否直接有 dynamic child”。

如果 shallow copy 会复制任何唯一 ownership capability，则非法。

## 7.3 `borrow(x)`

`borrow(x)` 建立 non-owning loan。

结果：

```text
BorrowedValue
```

它不创建 owner，不延长 owner 生命周期。

borrow 不能逃逸到 stored owning field / container element。

## 7.4 `move(place)`

`move(place)` 从 owning Place 消费 ownership capability。

结果：

```text
OwnedTemporary
```

source 进入 moved 状态，之后不能作为 live value 使用，除非该 Place 被重新初始化。

borrowed Place 不能 move。

---

# 8. 普通 `=` 的统一语义

赋值 / 初始化先完整求值 RHS，再根据最终 Value category 决定行为。

对于 owning target：

| RHS | 行为 |
|---|---|
| `InlineValue` | 普通值复制 |
| readable owning Place 的普通读取 | `DeepClone` |
| readable borrowed Place / BorrowedValue 的普通读取 | `DeepClone` |
| `OwnedTemporary` | ownership transfer |
| `move(place)` | ownership transfer |
| `null` | nullable 写入；必要时销毁旧 payload |

关键不变量：

```text
fresh owned constructor result
function owned return
clone result
shallow result
move result
malloc allocation root
```

都统一为 `OwnedTemporary`，进入 owning target 时直接 transfer。

不存在：

```text
malloc 专属 transfer 规则
```

例如：

```alias
val User a = User(...)
```

语义：

```text
construct User
-> OwnedTemporary<User>
-> transfer into a
```

而：

```alias
val User b = a
```

语义：

```text
read stable Place(a)
-> DeepClone
-> OwnedTemporary<User>
-> transfer into b
```

---

# 9. Place initialization 与 replacement

## 9.1 首次初始化

对 uninitialized typed Place 的第一次合法写入建立对象：

```text
Uninitialized<T>
+ write compatible value
-> Initialized<T>
```

不额外引入用户可见 `init` / `construct_at` 语法。

## 9.2 Replacement

对已经 initialized 的 owning Place：

```text
target = rhs
```

顺序必须为：

```text
1. 完整求值 RHS
2. 成功准备新的 Value / owner
3. 销毁旧 target
4. commit 新值到 target
```

不能先销毁 target 再计算 RHS。

这样保证 self-read、deep-clone self-assignment 与失败路径不会提前破坏 target。

## 9.3 Move replacement overlap

```alias
x = move(x)
```

编译错误。

一般：

```alias
target = move(source)
```

要求 source ownership storage 与 target 可证明不重叠。

以下都属于 overlap：

```text
source == target
source 是 target ancestor
target 是 source ancestor
```

如果无法证明 disjoint，则 fail-closed 拒绝。

该裁决必须在 sema/HIR 前完成，codegen 不重新判断。

---

# 10. Place projection 与 overlap

Borrow checker 与 move checker 使用结构化 Place，而不是仅靠变量名或机器地址。

Place projection 至少表达：

```text
Local(binding)
Field(base, field_id)
Index(base, index_fact)
Deref(pointer_fact)
```

两个 Place 的关系为三态：

```text
Disjoint
Overlap
Unknown
```

典型规则：

```text
不同 local root
-> Disjoint

x.a vs x.b，且字段不同
-> Disjoint

x vs x.a
-> Overlap

arr[0] vs arr[1]
-> Disjoint

arr[i] vs arr[j]，无法证明 i != j
-> Unknown

deref(p) vs deref(q)，无法证明 byte range 不重叠
-> Unknown
```

对于 ownership / borrow conflict：

```text
Overlap -> 按冲突处理
Unknown -> 按冲突处理
```

Alias v1 不使用 runtime borrow checker 来放宽 `Unknown`。

因此：

> bounds / provenance / raw initialization 可以在必要时有受控 runtime check；ownership exclusivity 与 borrow exclusivity 必须由静态分析保证。

---

# 11. Borrow / loan 模型

内部 loan 至少区分：

```text
ReadLoan
WriteLoan
```

规则：

* 多个互不冲突的 ReadLoan 可以共存；
* 同一重叠区域可存在多个 ReadLoan；
* WriteLoan 对冲突区域必须独占；
* live WriteLoan 存在时不能建立冲突 loan；
* owner 在任何冲突 live loan 存在时不能 move、free 或结束生命周期；
* borrow 使用非词法生命周期，loan 在最后一次实际使用后结束。

loan conflict 使用第 10 节的 Place overlap 三态模型。

---

# 12. Closure capture

closure capture 直接复用普通 loan 模型，不另建 capture ownership system。

裁决：

> capture loan 从 closure value 创建时建立，到该 closure value 的最后一次使用结束。

例如：

```alias
var i32 x = 0
func unit show = () -> println x
```

捕获对 `x` 建立 ReadLoan。

```alias
var i32 x = 0
func unit change = () -> increase x
```

捕获对 `x` 建立 WriteLoan。

NLL 可以让 capture loan 在 closure 最后一次使用后提前结束。

closure 不能通过 stored borrow 规则逃逸进普通持久字段/容器。

---

# 13. Function semantic effect signature

函数的完整静态语义签名不仅包含参数和返回类型，还必须包含 ownership effects。

概念模型：

```text
FunctionSignature {
    parameters: [
        {
            type,
            effect
        }
    ],
    return_type,
    return_effect
}
```

## 13.1 Parameter effects

参数 effect：

```text
ReadBorrow
WriteBorrow
Owned
```

### ReadBorrow

调用时建立只读 loan，不取得 ownership。

### WriteBorrow

调用时建立独占写 loan，不取得 ownership。

### Owned

callee 取得 ownership capability。

稳定 owning Place 不能因为一次普通函数调用被偷偷 move。

因此 owned argument 必须来自：

```text
OwnedTemporary
move(place)
clone(place)
shallow(place)
其它明确产生新 owner 的表达式
```

## 13.2 Return effects

返回 effect：

```text
Inline
Owned
Borrowed(source)
```

borrowed return 必须记录来源。

v1 支持的来源至少：

```text
Parameter(index)
Self
Global(binding)
```

v1 不允许同一个函数的 borrowed return 在不同路径最终依赖多个不同 source。

例如若：

```text
then -> borrow(param0)
else -> borrow(param1)
```

且二者不是同一 source，则编译失败。

这样 caller 能精确建立返回值 loan，而不需要用户书写 lifetime parameter。

## 13.3 Return ownership transfer

Owned return 可以直接返回 `OwnedTemporary`。

对于函数内即将随 return 结束生命周期的 local owning Place，编译器允许在 `return` 语境执行隐式 return transfer，不要求写：

```alias
return move(local)
```

该隐式 transfer 只属于 owned return 语境，不意味着普通赋值或普通 owned argument 可以根据“最后一次使用”自动 move。

## 13.4 Function value compatibility

function-valued 三元、`match` 或其它 merge 必须要求完整 semantic signature 精确一致，包括：

```text
parameter types
parameter effects
return type
return effect
borrow source
```

不执行 effect merge、effect subtyping 或自动适配。

## 13.5 Effect inference

parameter / return effects 是 sema 事实，不要求用户显式标注。

实现必须在 final HIR 前求得稳定 effect。

递归调用可能形成 effect dependency，因此 inference 必须作为有限静态数据流 / fixed-point 问题处理；不能通过 codegen fallback、第一次遍历猜测或诊断文本推断。

若 effect 无法得到唯一安全解，则编译失败。

---

# 14. 核心类型 capability 矩阵

Alias 必须对每个核心类型明确：

```text
DeepClone
ShallowClone
Move
Borrow
Destroy
```

基础矩阵：

| 类型 | DeepClone | ShallowClone | Move | Borrow | Destroy |
|---|---|---|---|---|---|
| integer / float / bool | 值复制 | 不提供 | 等价普通值传递 | 可借用 Place | no-op |
| `string` | 独立复制内容 | 不允许 | 允许 | 允许 | 销毁内容 storage |
| `struct` | 递归 clone owning fields | 仅当 `ShallowCloneable` | 允许 | 允许 | owning fields 逆声明顺序销毁 |
| `array<T>` | 复制容器与所有元素 | 不允许 | 允许 | 允许 | elements 逆索引销毁后释放 backing |
| `result<T,E>` | clone active payload | 仅当 payload 可 shallow | 允许 | 允许 | 销毁 active payload |
| `iterator<T>` | 不允许 | 不允许 | 允许 | 允许 | 销毁自身状态，不拥有 array elements |
| closure / executable function value | 不允许 | 不允许 | 允许 | 允许 | 结束自身 capture obligations / storage |
| `T?` | 继承 payload 能力 | 继承 payload 能力 | 允许 | 允许 | non-null 时销毁 payload |
| allocation-root `ptr<T>` | **暂不开放普通 DeepClone** | 不允许 | 允许 | 允许 | 由 `free` 或 parent destruction 结束 allocation |
| borrowed `ptr<T>` view | 不拥有 allocation | 不允许 | 不存在 ownership move | 允许 | 不销毁 allocation |

## 14.1 `ShallowCloneable(T)`

shallow capability 必须递归计算。

原则：

> 复制 T 的 root representation 时，不得复制任何唯一 ownership capability。

例如：

```text
ShallowCloneable(scalar) = true
ShallowCloneable(string) = false
ShallowCloneable(array<T>) = false
ShallowCloneable(struct S) = every field is ShallowCloneable
ShallowCloneable(result<T,E>) = T and E are ShallowCloneable
```

若 inline child 内部包含 dynamic owner，则 parent 也不可 shallow。

## 14.2 allocation-root pointer clone gate

raw allocation 的任意 deep clone 牵涉：

```text
holes
heterogeneous initialized regions
pointer provenance
interior references
对象图复制策略
```

在单独完成该语义设计前，allocation-root `ptr<T>` 不开放普通 `DeepClone`。

因此对稳定 owning raw pointer Place 使用普通 `=` 时，若该操作需要 DeepClone，应编译拒绝；用户必须显式 move 或建立 borrow。

这与 pointer runtime ABI 未决是两个不同的设计闸门。

---

# 15. Struct / array / result owning slots

## 15.1 Struct field

普通 struct field 永远是 owning slot。

它可以接收：

```text
InlineValue
OwnedTemporary
clone(...)
shallow(...)（若类型允许）
move(...)
null（nullable）
```

不能接收 BorrowedValue。

普通 field 不允许单独 move-out 后留下 partial-move hole。

若需要提取 ownership，必须使用未来能够维持 aggregate invariant 的正式操作；不能让任意 `move(object.field)` 破坏仍 live 的 struct。

## 15.2 Array element

`array<T>` 的 `[0, len)` 全部必须是 initialized owning elements。

普通用户不能任意 move-out 中间元素留下 hole。

ownership extraction 必须通过维持容器不变量的结构操作，例如 `pop` / `remove` 类 API。

## 15.3 Result payload

active result payload 是 owning slot。

variant 切换必须先完整构造新 payload，再销毁旧 active payload，最后 commit 新 tag / payload。

---

# 16. Destruction model

销毁的是 ownership resource，不是变量名字。

普通 owner 生命周期结束时触发 destruction pipeline。

来源包括：

```text
scope end
owning Place replacement
parent destruction
explicit ownership consumption
function frame exit
```

## 16.1 Destroy 与 Deallocate 分离

必须区分：

```text
destroy
deallocate
```

`destroy`：

* 销毁 owned children；
* 结束对象生命周期；
* 运行语言/runtime 内部清理。

`deallocate`：

* 释放物理 storage。

stack / inline object 可能 destroy 但不 deallocate。

## 16.2 普通 ownership tree 顺序

普通 ownership tree：

```text
children first
parent last
```

同级 struct owning fields 按声明逆序销毁。

array elements 按索引逆序销毁。

---

# 17. Raw allocation

`malloc<T>(count)` 创建一段独立 provenance 的 raw allocation。

`malloc<T>()` 等价于 `malloc<T>(1)`。

`malloc` 是语言 intrinsic；本文不要求先实现一般用户泛型函数。

成功：

```text
OwnedTemporary<allocation root pointer>
```

失败：

```text
null
```

`count == 0` 视为 allocation failure 并返回 null。

`count * stride(T)` 必须按数学精确结果检查，不能先在 Alias 整数宽度内回绕。

新 allocation 初始全部为：

```text
Uninitialized raw storage
```

`malloc` 不自动构造 `T`。

---

# 18. Allocation root ownership

allocation ownership 属于整个 allocation，不属于某个 element 或当前 pointer offset。

独立 allocation root owner：

* 必须最终显式 `free`；或
* ownership transfer 进普通 owning parent。

若一个可达路径让独立 allocation root owner 在未 `free`、未 transfer 的情况下结束生命周期，则编译失败。

一旦 allocation root ownership transfer 进 struct field、container payload 或其它 owning parent，该 allocation 变成 parent ownership subtree 的 child，由 parent destruction / replacement 管理。

borrowed interior pointer 永远不能 `free`。

---

# 19. `free`

```alias
free(pointer_owner)
```

返回 `unit`，没有值。

只有仍作为独立 allocation root owner 存在的 capability 可以被 `free` 消费。

合法：

```text
non-null allocation root owner
nullable allocation root owner
```

非法：

```text
borrowed pointer
interior non-owner pointer
moved owner
consumed owner
已经 transfer 给 parent 的 allocation child
```

`free(null)` 合法且运行时 no-op，但静态 ownership capability 仍被消费，因此不能重复 free。

`free(non-null root)`：

```text
1. destroy 所有仍 live 的 initialized regions
2. deallocate allocation storage
3. 标记 allocation identity dead
4. consume ownership capability
```

---

# 20. Raw allocation initialization metadata

因为 raw allocation 支持：

```text
partial initialization
holes
heterogeneous typed objects
move-out -> hole
destroy -> hole
reinterpret 后建立新的 typed object
```

运行时必须抽象地存在 allocation descriptor。

最终物理布局未冻结，但语义上至少包含：

```text
AllocationDescriptor {
    base
    extent
    alignment
    allocation identity / provenance
    alive
    initialized regions
}
```

每个 live initialized region 概念上包含：

```text
InitializedRegion {
    start
    end
    runtime type descriptor
    destruction descriptor
    init_sequence
}
```

`runtime type descriptor` 是编译器/runtime 内部 identity，不是源码类型名字符串。

## 20.1 Region invariant

不同 live initialized object region 不允许 byte overlap。

例如：

```text
[0, 8)   Initialized<u64>
[8, 16)  Uninitialized
```

合法。

但不能同时存在：

```text
[0, 8) Initialized<u64>
[4, 8) Initialized<u32>
```

## 20.2 Dynamic metadata

若具体 offset / region 只能在运行时知道，编译器可以发射受控 runtime metadata check。

这不等于 Alias 存在 `unsafe`。

check 失败必须 runtime abort，不能产生未定义行为。

---

# 21. Raw object lifecycle

## 21.1 First write

对合法 uninitialized typed range 首次写入：

```text
check bounds/alignment/no-overlap
-> construct value
-> insert Initialized<T> region
```

## 21.2 Replacement

若目标完整对应一个现有 `Initialized<T>`：

```text
1. 完整准备 RHS
2. destroy old T
3. commit new T
4. region 保持 Initialized<T>
```

不能用 replacement 把 `Initialized<S>` 直接冒充为 `Initialized<T>`。

## 21.3 Move-out

从 raw initialized object 完整 move-out：

```text
Initialized<T>
-> remove region metadata
-> byte range becomes Uninitialized
-> moved object 作为 OwnedTemporary 继续存在
```

## 21.4 Destroy

显式/隐式 destroy raw object：

```text
Initialized<T>
-> destroy T
-> remove region metadata
-> byte range becomes Uninitialized
```

## 21.5 Free-time destroy order

raw allocation 中 still-live initialized regions 按：

```text
init_sequence 的逆序
```

销毁。

不是按地址顺序。

同一 byte range 被 destroy / move-out 后重新初始化，会获得新的 `init_sequence`。

---

# 22. Pointer semantic model

正式 pointer 类型：

```alias
ptr<T>
ptr<T>?
```

`ptr<T>` non-null。

`ptr<T>?` nullable。

ownership relation 不编码在 pointer 类型里。

不存在：

```text
OwnedPtr<T>
BorrowedPtr<T>
```

同一个 `ptr<T>` 静态类型的 Value 可以由 sema 分别判定为 allocation-root owner 或 borrowed pointer view。

## 22.1 Pointer semantic facts

语言语义上的 pointer 至少关联：

```text
runtime allocation provenance
allocation bounds
view bounds
current offset/address
typed view T
nullness
是否携带 allocation-root ownership capability
```

其中：

```text
runtime allocation provenance / bounds
```

可能完全是运行时值。

sema 不把它们伪装成编译期常量。

---

# 23. Pointer ABI：未决设计闸门

当前尚未冻结 `ptr<T>` 的最终机器表示。

候选方向包括但不限于：

```text
fat pointer
thin pointer + runtime side table
capability handle + offset
其它满足语义不变量的表示
```

在该设计关闭前：

* `size(ptr<T>)` 的最终具体机器值不作为长期承诺；
* codegen ABI 不得提前假定 pointer 等于裸 `I64`；
* struct layout / function ABI 的 pointer 部分不得通过临时实现变成语言规范；
* HIR 只表达 pointer semantic operation，不表达具体寄存器布局；
* runtime metadata 的最终存放位置不冻结。

pointer ABI 的后续设计必须同时满足：

```text
provenance
bounds
view bounds
one-past-end
nullable
runtime checks
allocation identity
stored owning pointer capability
borrowed derived pointer
```

---

# 24. `refer`

```alias
refer(place)
```

建立 non-owning pointer view。

语义：

```text
Place
-> BorrowedValue<ptr<T>>
```

`refer`：

* 不产生 allocation ownership；
* 不延长 owner 生命周期；
* 可以指向合法的 uninitialized typed Place；
* 建立的 pointer view 受来源 Place / allocation 的 bounds 约束；
* 不能通过 owning field / container element stored。

---

# 25. `deref` 与 indexing

```alias
deref(ptr)
ptr[index]
```

都产生 Place，而不是立即产生普通 Value。

外层操作再决定：

```text
read
write
borrow
move
refer
initialize
replace
```

nullable pointer 必须经过控制流证明 non-null 后才能 dereference / indexing。

pointer 有效不等于 pointee initialized。

因此一个 pointer 可以合法指向 Uninitialized raw storage；只有读取、borrow object、member access、method call、move object 等需要一个已存在 `T` object 的操作才要求 `Initialized<T>`。

---

# 26. Pointer arithmetic

允许：

```alias
ptr + integer
ptr - integer
```

整数单位是 `T` element stride，不是 byte。

pointer arithmetic：

* 不产生新的 allocation ownership；
* 不因为 offset 回到 0 而获得 ownership；
* 不扩大来源 view bounds；
* 结果是 derived non-owning pointer view。

如果来源是 owning pointer Place，则 owner 仍然留在原 Place，派生结果是 borrow。

派生结果不能通过 stored owning field / container element 保存。

若某个 unanchored `OwnedTemporary` allocation root 无法在派生 view 生命周期内保持 owner，则该 borrow 不得逃逸；编译器必须拒绝悬空派生结果。

---

# 27. Pointer difference / comparison / one-past-end

## 27.1 Difference

同一 runtime allocation 内两个 `ptr<T>` 可以做差。

结果类型：

```alias
i64
```

结果：

```text
(offset(p) - offset(q)) / stride(T)
```

若 sema 无法证明 runtime provenance 相同，但该操作仍可能合法，则必须插入 provenance check；运行时不同 allocation 时 abort。

## 27.2 Equality

```alias
p == q
p != q
```

基于：

```text
provenance + position
```

而不是只比较裸机器地址。

## 27.3 Ordering

```alias
p < q
p <= q
p > q
p >= q
```

只对同一 allocation 内的位置有定义；无法静态证明时需要 runtime provenance check。

## 27.4 One-past-end

pointer 可以处于 view 的 one-past-end 位置。

允许：

```text
comparison
difference
loop sentinel
```

禁止：

```text
deref
read
write
borrow pointee
```

超过 one-past-end 的 pointer position 非法。

---

# 28. Bounds / alignment / layout

pointer arithmetic、index、deref、refer、reinterpret 必须服从：

```text
allocation bounds
view bounds
alignment
```

若静态可证明失败，则 compile error。

若安全性可以由受控 runtime check 保证，则插入 runtime check；失败时 abort。

每个 storable type `T` 都必须有：

```text
size(T)
align(T)
stride(T)
```

满足：

```text
stride(T) >= size(T)
stride(T) % align(T) == 0
stride(T) > 0
```

`unit` 不是 storable value type。

---

# 29. `reinterpret<T>`

```alias
reinterpret<T>(ptr)
```

建立同一 allocation 上新的 typed pointer view。

它：

* 不创建 allocation；
* 不创建 object；
* 不创建 ownership；
* 不 move；
* 不 deep clone；
* 不 shallow clone；
* 不改变 allocation provenance。

`reinterpret` 必须满足目标 `T` 的 alignment 与当前 view / allocation bounds。

## 29.1 Typed view 与 initialized object 分离

建立 `ptr<T>` view 不等于 storage 中已经存在 `Initialized<T>`。

因此仅仅创建 reinterpret view，不因为 view 其它位置存在 `Initialized<S>` 就自动把整个 view 判成非法。

真正形成具体 typed Place 或访问具体 element range 时，才检查该目标 byte range 的 initialization identity。

若目标 range 当前承载不同 `Initialized<S>`：

```text
read as T
write replacement as T
borrow T
move T
member access / method call as T
```

均非法或在动态情况下 runtime abort。

要把同一 storage 从 `Initialized<S>` 变为 `Initialized<T>`，必须先正常结束 S 生命周期，使该 range 回到 Uninitialized，再首次写入 T。

`reinterpret` 不能绕过 ownership、initialization、alignment、bounds 或 destruction。

---

# 30. Nullable

nullable 是通用能力：

```alias
T?
```

不允许嵌套：

```alias
T??
```

基本规则：

```text
T    -> T?   allowed
null -> T?   allowed
T?   -> T    no implicit conversion
null -> T    invalid
```

`T? -> T` 依赖控制流 narrowing。

nullness 与 ownership relation 独立。

owning nullable slot 从 non-null 变为 null：

```text
destroy old owned payload
-> write null
```

borrowed nullable slot 从 borrowed T 变为 null只结束/替换 borrow binding，不取得 ownership。

---

# 31. HIR contracts

进入 codegen 前，sema 必须已经把源码操作解析为明确内存语义。

HIR 至少要能结构化表达：

```text
ReadPlace
DeepClone
ShallowClone
Borrow
Move
InitializePlace
ReplacePlace
ReturnTransfer
ReferPlace
DerefPlace
PointerOffset
PointerDifference
PointerCompare
ReinterpretPointer
Free
Destroy
```

具体节点命名可由实现调整，但不得退化成 codegen 再看 AST 猜意图。

## 31.1 Value category

HIR / typed semantic facts 必须明确区分：

```text
Place
InlineValue
OwnedTemporary
BorrowedValue
Null
```

codegen 不能通过“这个表达式看起来像 constructor / malloc”重新判断 transfer。

## 31.2 Ownership facts

Move / Free / ReturnTransfer 等消费操作必须携带 sema 已验证的 capability 事实。

Move / Replace 必须在 HIR 前完成 overlap 检查。

## 31.3 Borrow facts

HIR 前必须已经完成：

```text
loan kind
loan source Place
loan live region
Place overlap 冲突检查
closure capture loan
borrowed return source
```

codegen 不执行 borrow checker。

## 31.4 Pointer facts

HIR 表达的是：

```text
需要什么 pointer semantic operation
需要哪些 runtime provenance/bounds/init checks
结果是 owner 还是 derived borrow
```

不要求 sema 提供某个具体 runtime allocation id 常量。

---

# 32. 编译期必须阻止

Alias 的静态 ownership / borrow 层必须阻止：

```text
double free
use after move
use after statically known free
borrow outlives owner
free while conflicting live loan exists
move owner while conflicting live loan exists
conflicting read/write borrow
owning slot 接收 BorrowedValue
stored borrow into field/container
ordinary struct field partial move-out
ordinary array element arbitrary move-out hole
ownership cycle
illegal shallow clone
move source/target overlap
independent malloc root owner leaking at scope end
```

Alias 的静态 + runtime memory safety 层共同必须阻止：

```text
deref one-past-end
pointer out of bounds
invalid pointer ordering/difference across allocations
read uninitialized raw object
initialized region overlap
reinterpret type confusion
use of deallocated allocation provenance
misaligned typed access
free borrowed/interior/non-owner pointer
```

不存在通过 `unsafe` 绕过上述规则的入口。

---

# 33. Runtime check 边界

允许 runtime check 的典型情况：

```text
dynamic pointer bounds
dynamic runtime provenance equality
dynamic alignment condition
dynamic raw initialization region overlap / identity
```

不使用 runtime check 放宽：

```text
ownership uniqueness
borrow exclusivity
move-after-use
stored borrow
function ownership effect mismatch
```

这些必须静态解决，否则编译失败。

---

# 34. 当前明确不进入本计划的范围

以下不在本计划内：

## 34.1 Pointer 最终 ABI

单独设计，当前保持 unresolved gate。

## 34.2 一般用户泛型系统

`malloc<T>` / `reinterpret<T>` 属于语言 intrinsic generic syntax，不要求本计划同时实现一般泛型函数、trait bound 或 `where`。

一般泛型应另立设计。

## 34.3 整数隐式 promotion 新规则

ownership / pointer 计划不顺带修改整个整数类型提升系统。

需要改变时另立规范。

## 34.4 FFI

不定义 C ABI / external ABI。

## 34.5 用户自定义 destructor

当前 destruction pipeline 由语言/runtime 自动控制。

不提供用户级：

```text
destructor
drop
deinit
```

## 34.6 Stored borrow / weak graph

v1 明确不提供。

---

# 35. 推荐实现顺序

在 pointer ABI 未冻结前，可以投入开发的前端 / HIR 工作：

```text
1. Place / Value category
2. OwnedTemporary
3. OwnershipCapability 数据流
4. local slot relation
5. clone / shallow / borrow / move semantic resolution
6. Place projection
7. Place overlap 三态分析
8. NLL loan analysis
9. closure capture loan
10. function parameter effects
11. function return effects + borrow source
12. type capability predicates
13. HIR ownership operations
14. raw allocation abstract semantic nodes
15. runtime-check requirement facts
```

在 pointer ABI 关闭前不要冻结：

```text
ptr<T> ValueAbi
pointer register/storage width
pointer function ABI
pointer field physical layout
allocation descriptor physical layout
runtime pointer-check calling convention
```

---

# 36. 核心不变量总结

Alias 的内存模型最终必须保持：

```text
Place != Value

stable Place 普通复制
-> DeepClone

fresh owned result
-> OwnedTemporary
-> transfer

malloc 没有特殊赋值规则

compile-time ownership capability
!= runtime allocation identity

borrow 不拥有对象
borrow 不延长 owner 生命周期
stored borrow 禁止

ownership / borrow exclusivity 静态保证
bounds / provenance / raw-init 必要时可 runtime check

move 消费 ownership capability
clone 创建独立 owner
shallow 只在不会复制 ownership capability 时合法

普通用户对象图只包含 owning edges
raw allocation 可以部分初始化
Initialized<T> 不能被 reinterpret 冒充成 Initialized<U>

sema 决定语义
HIR 固化语义
codegen fail-closed 执行

ptr<T> 的机器表示尚未冻结
任何当前实现都不得反过来把临时表示升级成长期语言设计
```

这套不变量是后续实现与审阅的基线。