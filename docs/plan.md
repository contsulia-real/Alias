# Alias 内存、所有权与指针模型

## 0. 文档状态

本文定义 Alias 下一阶段的内存、所有权、借用、销毁、raw allocation 与 pointer 语义及其当前目标 ABI。

本文不是对当前实现的兼容说明。当前实现中与本文冲突、语义错误、仅为历史阶段服务或无法支撑本文不变量的结构都可以重构或删除。

本文先冻结语言语义与编译器合同，再由实现服从这些合同。现有“一切复杂值都是一个 I64 word”之类实现假设不是长期约束。

`ptr<T>` 的 runtime representation 与当前 Windows x64 ABI 已在本文中冻结：

```text
4-word inline fat capability pointer
= provenance + address + view_start + view_end
```

必须严格区分：

```text
目标程序位宽 / machine address width
!=
Alias ptr<T> capability value 的 storage size
```

当前目标 `x86_64-pc-windows-msvc` 是 **64-bit 目标**，机器地址与 machine pointer width 为 64 bits / 8 bytes。

Alias 的一个 `ptr<T>` 语言值由四个 64-bit machine word 组成，因此：

```text
target program              = 64-bit
machine address width        = 64 bits
machine pointer width        = 64 bits / 8 bytes
Alias ptr<T> capability size = 32 bytes / 256 bits

size(ptr<T>)  = 32 bytes
align(ptr<T>) = 8 bytes
```

这里的“32 bytes”绝不表示“32-bit 程序”或“32-bit 地址空间”；它只描述一个 Alias pointer capability aggregate value 的总 storage 大小。

`ptr<T>?` 使用同一表示，不增加额外 tag。

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
* pointer provenance / bounds 是正式语言语义，不退化成裸地址语义；
* sema 决定最终内存语义，HIR 固化结果，codegen 不重新猜测。

---

# 2. 三个必须分离的层次

Alias 的实现必须严格区分：

```text
compile-time ownership capability
runtime storage identity / provenance
runtime raw-initialization metadata
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

ownership capability **不编码进 `ptr<T>` 的机器 bit pattern**。

因此 allocation-root owner pointer 与 borrowed pointer view 可以拥有完全相同的 32-byte pointer representation，但只有前者在静态语义上携带可消费 ownership capability。

## 2.2 Runtime storage identity / provenance

一次真实程序执行中的每个可寻址 storage root 都具有 canonical runtime storage identity / provenance。

storage root 可以来自：

```text
raw malloc allocation
address-taken stack/local storage
global/static storage
heap language object
其它具有稳定地址范围的 storage root
```

同一个 `malloc` 语法节点可以在循环、递归或多次调用中产生任意多个不同 runtime identity。

因此：

```text
compile-time ownership capability
!=
runtime storage identity
```

sema 只能静态决定 ownership flow、pointer operation 与哪些 runtime check 必须存在；不能把某个动态 storage identity 当成编译期常量。

## 2.3 Runtime raw-initialization metadata

raw allocation 允许部分初始化和 hole，因此运行时可能需要维护哪些 byte range 当前承载合法对象。

这属于 raw allocation 的 runtime metadata，不属于用户类型，也不是 ownership capability 本身。

普通已经由语言静态布局管理的 local / struct / array object 不因为可 `refer` 就自动需要 raw initialized-region table。

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

pointer value 仍属于 Value。

### InlineValue

整数、浮点、bool 等不具有独立动态 ownership 的普通值。

### OwnedTemporary

一个已经完整构造、携带唯一 ownership capability、尚未固定进入某个 owning Place 的临时值。

来源包括：

```text
struct / array / result 等新构造结果
函数 owned return
对携带独立动态 ownership 的值执行 clone(...)
合法 user-level shallow(...) 产生的新 aggregate root
move(place)
malloc(...) 成功产生的新 allocation root
其它未来产生新 owner 的表达式
```

对 `InlineValue` 执行 `clone(...)` 仍产生 `InlineValue`，不会仅因使用 clone 语法而制造 ownership capability。

`ShallowCloneable(scalar) = true` 只表示 scalar 可作为递归 shallow-safe 叶；它不意味着 `shallow(scalar)` 是合法 user-level root 操作。合法 `shallow(...)` 必须确实建立一个新的独立 aggregate root，因此其结果才是 `OwnedTemporary`。

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

这里 `clone(...) -> owning` 描述的是 target slot relation；它不表示所有 clone 结果都必须是 `OwnedTemporary`。标量 clone 仍可作为 `InlineValue` 进入 owning slot。

这里 `shallow(...) -> owning` 只适用于合法 user-level shallow root；递归 `ShallowCloneable(scalar)=true` 不创建一个可独立绑定的 shallow scalar owner。

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

Alias v1 不提供通过普通字段/容器长期保存 parent pointer、observer pointer、weak back-reference 等 stored borrow 能力。

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

结果 category 取决于 T 是否携带独立动态 ownership：

```text
InlineValue 类型
-> InlineValue
-> ownership capability = None

携带独立动态 ownership 的 DeepCloneable 类型
-> OwnedTemporary
-> ownership capability = Available
```

deep clone：

* 对 InlineValue 执行普通值复制，不制造动态 owner；
* 对携带 ownership subtree 的值递归复制 ownership subtree；
* 不取得 source ownership；
* 不改变 source 既有 loan；
* 只有 dynamic ownership-bearing clone 结果才拥有新的独立 ownership capability。

只有 `DeepCloneable(T)` 为真时才合法。

## 7.2 `shallow(x)`

`shallow(x)` 产生新的独立 aggregate root，但只允许出现在复制该 root 的语义结构不会复制任何既有唯一 ownership capability 的类型上。

合法 user-level `shallow(x)` 的结果固定为：

```text
OwnedTemporary
ownership capability = Available
```

必须区分：

```text
ShallowCloneable(T)
!=
user-level shallow(T-value) 一定存在
```

`ShallowCloneable(T)` 是递归安全性谓词。scalar 可以作为 shallow-safe 叶，因此：

```text
ShallowCloneable(scalar) = true
```

但 scalar 自身没有需要建立的新 aggregate ownership root，所以：

```alias
shallow(1)      // 非法
shallow(true)   // 非法
```

user-level shallow 根必须是一个本身具有独立 aggregate ownership root 的类型，并且其全部递归子结构都满足 `ShallowCloneable`。当前核心模型中典型可用根是满足谓词的 `struct` / `result`；string、array、iterator、function/closure、ptr 均不允许作为 shallow root。

实现不能把“当前物理表示可复制”误当成“shallow 安全”。若某个 shallow-safe aggregate 当前后端仍用 heap pointer 表示，codegen 也必须建立新的独立 aggregate root，而不能简单复制旧 pointer bit pattern 后让两个 owner 指向同一 root。

如果 shallow 会复制任何唯一 ownership capability，则非法。

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
dynamic ownership-bearing clone result
合法 shallow aggregate root result
move result
malloc allocation root
```

都统一为 `OwnedTemporary`，进入 owning target 时直接 transfer。

InlineValue 的 clone 仍是 InlineValue，并继续采用普通值复制；它不进入上述 owner-transfer 集合。scalar 的递归 ShallowCloneable 也不产生 standalone shallow owner。

不存在 `malloc` 专属 transfer 规则。

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

因此需要动态 ownership capability 的 owned argument 必须来自：

```text
OwnedTemporary
move(place)
对动态 ownership-bearing 值执行 clone(place)
合法 shallow aggregate root
其它明确产生新 owner 的表达式
```

InlineValue 参数按其值语义传递，不因为参数 effect 的概念分类制造动态 ownership capability。

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

若：

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
| integer / float / bool | 值复制 | 递归 safe leaf；不提供 standalone `shallow(scalar)` | 等价普通值传递 | 可借用 Place | no-op |
| `string` | 独立复制内容 | 不允许 | 允许 | 允许 | 销毁内容 storage |
| `struct` | 仅当所有 owning fields 可 deep clone | 仅当 `ShallowCloneable`；可作为 aggregate root | 允许 | 允许 | owning fields 逆声明顺序销毁 |
| `array<T>` | 仅当 `DeepCloneable(T)` | 不允许 | 允许 | 允许 | elements 逆索引销毁后释放 backing |
| `result<T,E>` | 仅当 payload 类型均可 deep clone | 仅当 payload 可 shallow；可作为 aggregate root | 允许 | 允许 | 销毁 active payload |
| `iterator<T>` | 不允许 | 不允许 | 允许 | 允许 | 销毁自身状态，不拥有 array elements |
| closure / executable function value | 不允许 | 不允许 | 允许 | 允许 | 结束自身 capture obligations / storage |
| `T?` | 继承 payload 能力 | 继承 payload 递归能力；standalone root 仍须满足 root 规则 | 允许 | 允许 | non-null 时销毁 payload |
| `ptr<T>` | **不开放普通 DeepClone** | 不允许 | owning relation 时允许 | 允许 | root owner 由 `free` / parent destruction 结束；borrowed view 不销毁 storage |

## 14.1 `DeepCloneable(T)`

DeepClone capability 必须递归计算。

至少：

```text
DeepCloneable(scalar) = true
DeepCloneable(string) = true
DeepCloneable(ptr<T>) = false
DeepCloneable(array<T>) = DeepCloneable(T)
DeepCloneable(struct S) = every owning field is DeepCloneable
DeepCloneable(result<T,E>) = T and E are DeepCloneable
DeepCloneable(T?) = DeepCloneable(T)
```

raw pointer allocation 的任意 deep clone 牵涉 holes、heterogeneous initialized regions、pointer provenance、interior references 与对象图复制策略；在未来单独设计前不开放。

因此：

```alias
val ptr<T> b = a
```

如果 `a` 是稳定 owning pointer Place 且该语境需要普通 DeepClone，则编译失败。用户必须显式 move 或建立 borrow。

任何包含 owning `ptr<T>` 的 aggregate 也会据此失去普通 DeepClone 能力，除非未来为 raw allocation 定义正式 clone 语义。

## 14.2 `ShallowCloneable(T)`

shallow capability 必须递归计算。

原则：

> 对一个 aggregate root 建立 shallow copy 时，不得复制任何既有唯一 ownership capability。

递归安全性：

```text
ShallowCloneable(scalar) = true
ShallowCloneable(string) = false
ShallowCloneable(ptr<T>) = false
ShallowCloneable(array<T>) = false
ShallowCloneable(struct S) = every field is ShallowCloneable
ShallowCloneable(result<T,E>) = T and E are ShallowCloneable
```

这里 `ShallowCloneable(scalar)=true` 的唯一含义是 scalar 可以作为 aggregate 内的安全递归叶子；它**不**开放 standalone `shallow(scalar)`。user-level shallow 还必须满足 root 类型本身具有可新建的独立 aggregate ownership root。

因此：

```text
recursive shallow-safe leaf
!=
legal user-level shallow root
```

合法 user-level shallow root 产生新的 `OwnedTemporary + Available`。若当前物理 ABI 用 pointer 代表该 aggregate，backend 必须建立新的 root storage，而不是 bit-copy pointer。

若 inline child 内部包含 dynamic owner，则 parent 也不可 shallow。

---

# 15. Struct / array / result owning slots

## 15.1 Struct field

普通 struct field 永远是 owning slot。

它可以接收：

```text
InlineValue
OwnedTemporary
clone(...)（若类型允许）
shallow(...)（若类型允许且是合法 root）
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

`malloc<T>(count)` 创建一段独立 storage provenance 的 raw allocation。

`malloc<T>()` 等价于 `malloc<T>(1)`。

`malloc` 是语言 intrinsic；本文不要求先实现一般用户泛型函数。

成功：

```text
OwnedTemporary<allocation-root ptr<T>>
```

失败：

```text
null
```

`count == 0` 视为 allocation failure 并返回 null。

`count * stride(T)` 必须按数学精确结果检查，不能先在 Alias 整数宽度内回绕。

为了保证合法 `ptr<T>` difference 可以由 `i64` 表示，初始 element count 不得超过 `i64::MAX`；违反时同样视为 allocation failure。

新 allocation 初始全部为：

```text
Uninitialized raw storage
```

`malloc` 不自动构造 `T`。

成功时初始 pointer view：

```text
provenance = new StorageDescriptor
address    = allocation base
view_start = allocation base
view_end   = allocation base + count * stride(T)
```

---

# 18. Allocation root ownership

allocation ownership 属于整个 raw allocation，不属于某个 element 或当前 pointer offset。

独立 allocation root owner：

* 必须最终显式 `free`；或
* ownership transfer 进普通 owning parent。

若一个可达路径让独立 allocation root owner 在未 `free`、未 transfer 的情况下结束生命周期，则编译失败。

一旦 allocation root ownership transfer 进 struct field、container payload 或其它 owning parent，该 allocation 变成 parent ownership subtree 的 child，由 parent destruction / replacement 管理。

borrowed interior pointer 永远不能 `free`。

pointer arithmetic、`refer`、`reinterpret` 都不能因为最终 address 回到 allocation base 而重新制造 allocation-root ownership capability。

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
2. deallocate raw allocation storage
3. destroy raw initialization metadata
4. end / release the StorageDescriptor
5. consume ownership capability
```

不需要在 pointer bit pattern 中维护 `is_owner` flag；`Free` HIR 只有在 sema 已证明 root ownership capability 后才能生成。

---

# 20. StorageDescriptor 与 provenance

每个可寻址 storage root 在其可被 pointer 观察的生命周期内都有一个 canonical `StorageDescriptor` identity。

pointer 的 `provenance` lane 保存该 descriptor 的稳定地址/handle；在当前 Windows x64 ABI 中它是一个 **64-bit machine pointer value（8 bytes）**。这再次与整个 Alias `ptr<T>` capability aggregate 的 32-byte storage 大小相区分。

概念职责至少包括：

```text
StorageDescriptor {
    base
    extent
    storage identity / provenance
    storage kind
    optional raw-initialization metadata
}
```

具体 descriptor 内部 byte layout **不是用户语言 ABI**，可以由 runtime/codegen 在保持本文语义合同的前提下调整。

关键不变量：

* 一个 live storage root 只有一个 canonical descriptor identity；
* subplace / subview 不创建新的 provenance identity；
* `refer(object.field)` 继续使用 object/root 的同一 descriptor，只收窄 pointer view；
* descriptor identity 在所有合法 derived pointer 生命周期内稳定；
* descriptor 生命周期不得短于任何合法 pointer loan；
* pointer equality / difference / ordering 使用 descriptor identity 判断 provenance，而不是仅看机器地址。

## 20.1 非 raw storage

对于 address-taken local / stack storage，descriptor 可以由编译器在 frame 内建立稳定地址的内部记录；不要求 heap 分配 descriptor。

如果一个原本只存在于 SSA 的 Place 被 `refer`，codegen 必须把它 materialize 到具有稳定地址的 storage，并为该 storage root 建立 canonical descriptor。

对于 global/static storage，可以使用静态 descriptor。

对于 heap language object，可以使用与该 object 生命周期绑定的 descriptor。

## 20.2 Raw allocation descriptor

`malloc` 创建的 raw allocation 使用 `StorageDescriptor` 作为其 canonical provenance root，并关联 raw initialized-region metadata。

在静态 borrow checker 已经保证不存在 live pointer loan 的前提下，`free` 可以随 allocation 一并结束 descriptor 生命周期；不要求永久泄漏 descriptor 来捕捉本应被静态禁止的 use-after-free。

---

# 21. Raw allocation initialization metadata

因为 raw allocation 支持：

```text
partial initialization
holes
heterogeneous typed objects
move-out -> hole
destroy -> hole
reinterpret 后建立新的 typed object
```

raw allocation descriptor 必须能够关联 initialized-region metadata。

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

## 21.1 Region invariant

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

## 21.2 Dynamic metadata

若具体 offset / region 只能在运行时知道，编译器可以发射受控 runtime metadata check。

check 失败必须 runtime abort，不能产生未定义行为。

---

# 22. Raw object lifecycle

## 22.1 First write

对合法 uninitialized typed range 首次写入：

```text
check bounds/alignment/no-overlap
-> construct value
-> insert Initialized<T> region
```

## 22.2 Replacement

若目标完整对应一个现有 `Initialized<T>`：

```text
1. 完整准备 RHS
2. destroy old T
3. commit new T
4. region 保持 Initialized<T>
```

不能用 replacement 把 `Initialized<S>` 直接冒充为 `Initialized<T>`。

## 22.3 Move-out

从 raw initialized object 完整 move-out：

```text
Initialized<T>
-> remove region metadata
-> byte range becomes Uninitialized
-> moved object 作为 OwnedTemporary 继续存在
```

## 22.4 Destroy

显式/隐式 destroy raw object：

```text
Initialized<T>
-> destroy T
-> remove region metadata
-> byte range becomes Uninitialized
```

## 22.5 Free-time destroy order

raw allocation 中 still-live initialized regions 按：

```text
init_sequence 的逆序
```

销毁，而不是按地址顺序。

同一 byte range 被 destroy / move-out 后重新初始化，会获得新的 `init_sequence`。

---

# 23. Pointer 类型与冻结 ABI

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

## 23.1 语义字段

pointer value 的正式语义由四个 runtime lane 表达：

```text
Ptr<T> {
    provenance
    address
    view_start
    view_end
}
```

`T` 是静态类型，不进入 runtime pointer value。

ownership capability 是 sema/HIR 静态事实，也不进入 pointer runtime value。

## 23.2 当前 Windows x64 物理布局与位宽

当前目标 `x86_64-pc-windows-msvc` 是 64-bit 目标。这里有两个不同的宽度概念：

```text
machine pointer / address width = 64 bits / 8 bytes
Alias ptr<T> value size         = 256 bits / 32 bytes
```

`ptr<T>` 不是一个 256-bit 机器地址；它是由四个 64-bit lane 组成的 aggregate capability value，其中只有 `address` lane 承担当前机器地址位置的角色，`provenance` lane 也是一个 64-bit machine pointer/handle。

物理布局：

```text
offset  0: provenance   8 bytes / 64 bits
offset  8: address      8 bytes / 64 bits
offset 16: view_start   8 bytes / 64 bits
offset 24: view_end     8 bytes / 64 bits

size  = 32 bytes / 256 bits
align = 8 bytes
```

概念上是：

```text
provenance : *StorageDescriptor   // 64-bit machine pointer
address    : u64                  // 64-bit machine address value
view_start : u64
view_end   : u64
```

其它目标若未来存在，可以定义与其 **machine pointer width** 匹配的等价 4-word capability ABI；当前规范只冻结 Windows x64 的四个 64-bit word / 32-byte 形式。

任何文档、代码或诊断中提到“32-byte pointer capability”时，都不得把它简称或解释成“32-bit pointer”。

## 23.3 Pointer invariant

任何 non-null live pointer 必须满足：

```text
provenance != 0
view_start <= address <= view_end
StorageDescriptor.base <= view_start
view_end <= StorageDescriptor.base + StorageDescriptor.extent
```

其中：

```text
address == view_end
```

表示 one-past-end。

对于当前 typed view，合法 pointer position 必须位于该 view 自己的 element lattice 上：

```text
address = view_start + k * stride(T)
```

合法 pointer operation 不能扩大 `[view_start, view_end]`。

## 23.4 为什么不是 thin pointer / handle

Alias pointer 的 per-value view bounds 不能只由机器 address 唯一恢复；同一 address 可以存在不同合法 view。

因此不采用：

```text
8-byte raw address + global side table
```

也不采用：

```text
8-byte capability-table handle
```

后者会把普通 pointer arithmetic / subview 变成 capability entry 生命周期管理问题。

4-word inline fat pointer 允许 pointer arithmetic 直接在 SSA lanes 中完成，不为每次派生 pointer 分配 runtime object。

---

# 24. Nullable pointer ABI

`ptr<T>?` 与 `ptr<T>` 使用相同 32-byte capability storage。这里的 32 bytes 同样只是 aggregate value 大小；当前机器地址宽度仍是 64 bits。

canonical null：

```text
provenance = 0
address    = 0
view_start = 0
view_end   = 0
```

判断 pointer nullness 以：

```text
provenance == 0
```

为 canonical 判定。

所有合法 non-null pointer 必须 `provenance != 0`。

因此：

```text
size(ptr<T>)  = 32
size(ptr<T>?) = 32
align         = 8
```

nullable narrowing 不改变 pointer bit representation，只改变静态 nullness facts。

---

# 25. `refer`

```alias
refer(place)
```

建立 non-owning pointer view。

语义：

```text
Place
-> BorrowedValue<ptr<T>>
```

设 source Place 的地址为 `A`，其 canonical storage root descriptor 为 `D`，则单个 T Place 的基本 view：

```text
provenance = D
address    = A
view_start = A
view_end   = A + stride(T)
```

`refer`：

* 不产生 allocation ownership；
* 不延长 owner 生命周期；
* 可以指向合法的 uninitialized typed Place；
* 不创建新的 storage provenance identity；
* 建立的 pointer view 受来源 Place / root storage bounds 约束；
* 不能通过 owning field / container element stored。

对 subplace 建立 pointer 时仍复用 root descriptor；view bounds 负责表达 subplace capability 边界。

---

# 26. `deref` 与 indexing

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

对 `deref(p)` 至少要求：

```text
p.address < p.view_end
p.address 满足 align(T)
[p.address, p.address + size(T)) 位于 view / storage bounds 内
```

raw allocation 上还必须满足相应 Initialized<T> identity 规则。

---

# 27. Pointer arithmetic

允许：

```alias
ptr + integer
ptr - integer
```

整数单位是 `T` element stride，不是 byte。

设：

```text
delta = integer * stride(T)
new_address = address +/- delta
```

乘法与地址计算必须 checked，不能发生整数回绕。

合法结果必须满足：

```text
view_start <= new_address <= view_end
```

结果：

```text
result.provenance = source.provenance
result.address    = new_address
result.view_start = source.view_start
result.view_end   = source.view_end
```

因此 pointer arithmetic：

* 不产生新的 storage provenance；
* 不产生新的 allocation ownership；
* 不因为 offset 回到 0 而获得 ownership；
* 不扩大来源 view bounds；
* 保持原 typed element lattice；
* 结果是 derived non-owning pointer view。

如果来源是 owning pointer Place，则 owner 仍然留在原 Place，派生结果是 borrow。

派生结果不能通过 stored owning field / container element 保存。

若某个 unanchored `OwnedTemporary` allocation root 无法在派生 view 生命周期内保持 owner，则该 borrow 不得逃逸；编译器必须拒绝悬空派生结果。

静态能证明 arithmetic 越界时编译失败；只能动态确定时插入 runtime bounds check，失败时 abort。

---

# 28. Pointer difference / comparison / one-past-end

## 28.1 Equality

对类型兼容的 pointer：

```alias
p == q
p != q
```

pointer identity 基于：

```text
provenance + address
```

因此 runtime 比较：

```text
p.provenance == q.provenance
&&
p.address == q.address
```

`view_start / view_end` 不参与 equality。

这意味着同 provenance、同 address、但 capability bounds 不同的两个 pointer 仍然地址相等；它们是否能 dereference 由各自 view bounds 决定。

两个机器地址数值偶然相同、但 provenance 不同的 pointer 不相等。

## 28.2 Difference

两个类型兼容的 `ptr<T>` 做差：

```alias
p - q
```

结果静态类型固定为：

```alias
i64
```

必须满足：

```text
p.provenance == q.provenance
```

若 sema 无法静态证明，则插入 runtime provenance check；不同 storage provenance 时 abort。

随后要求：

```text
(p.address - q.address) % stride(T) == 0
```

否则二者不位于同一 T element lattice distance 上，runtime abort。

最终结果：

```text
(p.address - q.address) / stride(T)
```

必须可由 `i64` 表示。

## 28.3 Ordering

```alias
p < q
p <= q
p > q
p >= q
```

只对同一 storage provenance 内的位置有定义。

无法静态证明 provenance 相同时插入 runtime check；不同 provenance 时 abort。

通过 provenance check 后按 `address` 顺序比较。

## 28.4 One-past-end

pointer 可以处于：

```text
address == view_end
```

允许：

```text
comparison
difference
loop sentinel
reinterpret 成零长度后继 view
```

禁止：

```text
deref
read
write
borrow pointee
```

超过 `view_end` 的 pointer position 非法。

---

# 29. Bounds / alignment / type layout

pointer arithmetic、index、deref、refer、reinterpret 必须服从：

```text
storage root bounds
pointer view bounds
alignment
typed element lattice
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

当前 Windows x64：

```text
machine pointer width = 64 bits / 8 bytes
size(ptr<T>)          = 32 bytes / 256 bits
align(ptr<T>)         = 8 bytes
stride(ptr<T>)        = 32 bytes
```

`ptr<T>?` 相同。

---

# 30. `reinterpret<T>`

```alias
reinterpret<T>(ptr)
```

建立同一 storage provenance 上新的 typed pointer view。

它：

* 不创建 storage root；
* 不创建 object；
* 不创建 ownership；
* 不 move；
* 不 deep clone；
* 不 shallow clone；
* 不改变 provenance。

## 30.1 Capability narrowing

`reinterpret<T>` 不扩大 source view，而是从 source 当前 address 开始建立新的 typed lattice。

设：

```text
current   = source.address
available = source.view_end - current
count     = floor(available / stride(T))
```

成功结果：

```text
result.provenance = source.provenance
result.address    = current
result.view_start = current
result.view_end   = current + count * stride(T)
```

因此 reinterpret 后：

```text
result.address = result.view_start
```

新 view 的 element lattice origin 明确，不需要额外第五个 pointer lane。

source pointer 本身不被修改。

尾部不足一个完整 `T` stride 的 bytes 不进入新的 typed pointer view。

`current` 必须满足 `align(T)`；若静态不能确定，可以 runtime check，失败时 abort。

## 30.2 Typed view 与 initialized object 分离

建立 `ptr<T>` view 不等于 storage 中已经存在 `Initialized<T>`。

因此仅仅创建 reinterpret view，不因为原 view 其它位置存在 `Initialized<S>` 就自动把整个新 view 判成非法。

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

`reinterpret` 不能绕过 ownership、initialization、alignment、bounds、provenance 或 destruction。

---

# 31. Pointer function ABI

`ptr<T>` 是 32-byte / 256-bit aggregate capability value，不允许继续套用“一个语言值 = 一个 Cranelift scalar Type”的旧抽象。它仍运行在 64-bit 目标上；32-byte aggregate size 与 target bitness 无关。

## 31.1 Expression representation

函数内部 / 表达式 lowering 中，pointer 可以保持为四个 SSA lane：

```text
provenance : I64
address    : I64
view_start : I64
view_end   : I64
```

普通 pointer arithmetic / comparison 不要求把 pointer 临时落入 heap object。

## 31.2 Storage representation

写入 local cell、struct field、array element、result payload 等真实 storage 时使用连续：

```text
32 bytes, align 8
```

是否能物理复制这 32 bytes 由 HIR 的已解析 value operation 决定；codegen 不得因为 pointer layout 可 memcpy 就绕过 ownership / DeepClone / ShallowClone 规则。

## 31.3 Parameter passing

Alias 内部函数 ABI 对 pointer parameter 使用：

```text
IndirectByValue(32, align 8)
```

caller：

```text
1. 建立 caller-owned 32-byte argument temporary
2. 写入四个 pointer lanes
3. 传入该 temporary 的地址
```

callee：

```text
1. 从 incoming address 读取四个 lanes
2. 进入本函数 SSA representation
```

这里传入 temporary 地址所使用的机器参数本身仍是当前 x64 的 64-bit machine pointer。

这是语言层 by-value parameter；间接传递只是一种机器 ABI lowering，不把 callee 参数变成 stored borrow，也不允许 callee 通过修改 argument temporary 改写 caller 的语义 Place。

所有 semantic pointer parameter，包括方法接收者在其最终机器签名需要 pointer value 时，都服从同一规则。

## 31.4 Pointer return

pointer return 使用显式 caller-allocated return area：

```text
ExplicitSRet(32, align 8)
```

caller：

```text
allocate 32-byte return area
pass hidden return-area address
call
load four lanes
```

callee：

```text
write four lanes into return area
return normally
```

隐藏 return-area 地址本身同样是当前 x64 的 64-bit machine pointer。

不依赖四个独立机器 return register，也不把 pointer 压回 I64 handle。

对于存在显式 sret 的用户函数，当前 Alias internal hidden parameter order 冻结为：

```text
[sret?, globals, closure_env, explicit machine params...]
```

无 sret 时仍为：

```text
[globals, closure_env, explicit machine params...]
```

该规则是 Alias 内部 ABI，不是 C ABI / 外部 ABI。

## 31.5 Nullable pointer function ABI

`ptr<T>?` 与 `ptr<T>` 完全使用相同 parameter / return ABI；null 只由 canonical zero representation 表达，不增加 discriminant 参数或额外 return tag。

---

# 32. Aggregate / container ABI implications

冻结 32-byte `ptr<T>` capability aggregate 后，当前任何“所有任意值都可以压入 8-byte universal word”的实现假设必须退出长期设计。

这里的 32-byte aggregate 不改变目标程序仍为 64-bit 的事实。

## 32.1 Array

`array<T>` backing element storage 必须按：

```text
stride(T)
```

前进，而不是固定 8 bytes。

因此：

```text
array<ptr<T>> element stride = 32
```

array runtime 可以保留自己的 header / wrapper，但元素物理布局必须由唯一 type-layout owner 决定。

## 32.2 Result

`result<T,E>` 的 payload storage 必须能够容纳实际 active payload。

概念布局至少按：

```text
payload_size  = max(size(T), size(E))
payload_align = max(align(T), align(E))
```

加上 canonical discriminant 与必要 padding 计算。

不能继续假定 payload 永远是单个 8-byte word。

## 32.3 Struct

struct field layout 必须直接消费字段类型的真实：

```text
size
align
```

因此 `ptr<T>` field 占 32 bytes / align 8。

## 32.4 Generic internal storage

任何 closure env、cell、container payload 或其它内部结构，只要语义上能够 inline 保存任意用户 `T`，就不能假定 `T` 必然是一个 I64 word。

实现可以显式选择 typed inline storage 或额外 indirection，但该选择必须由统一 ABI/layout owner 管理，不能分散 magic offsets / fallback。

---

# 33. Nullable 通用语义

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

borrowed nullable slot 从 borrowed T 变为 null 只结束/替换 borrow binding，不取得 ownership。

pointer nullable 的具体无额外 tag ABI 见第 24 节。

---

# 34. HIR contracts

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

## 34.1 Value category

HIR / typed semantic facts 必须明确区分：

```text
Place
InlineValue
OwnedTemporary
BorrowedValue
Null
```

codegen 不能通过“这个表达式看起来像 constructor / malloc”重新判断 transfer。

对显式 `DeepClone`，HIR 必须冻结 clone capability/plan；标量 clone 固化为 InlineValue，dynamic ownership-bearing clone 固化为 OwnedTemporary，后端不得按物理表示重新分类。

对显式 `ShallowClone`，HIR 必须冻结递归 `ShallowClonePlan` 与 root legality。合法 user-level shallow 一律固化为 `OwnedTemporary + Available`；`Inline` 只允许作为递归 plan 叶，不能被后端提升成 standalone shallow root。

## 34.2 Ownership facts

Move / Free / ReturnTransfer 等消费操作必须携带 sema 已验证的 capability 事实。

Move / Replace 必须在 HIR 前完成 overlap 检查。

pointer 的 32-byte physical representation 不包含 ownership bit；后端不能根据 `address == descriptor.base` 等条件推断 owner。

## 34.3 Borrow facts

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

## 34.4 Pointer facts

HIR 表达：

```text
pointer semantic operation
result relation / derived borrow
需要的 runtime provenance/bounds/alignment/init checks
```

运行时 pointer value 由冻结的四 lane ABI 实现。

sema 不要求提供某个具体 runtime StorageDescriptor 地址常量；动态 provenance 仍然可以完全是 runtime value。

---

# 35. 编译期与运行时必须阻止

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
illegal deep clone of ptr-containing ownership resource
move source/target overlap
independent malloc root owner leaking at scope end
```

Alias 的静态 + runtime memory safety 层共同必须阻止：

```text
deref one-past-end
pointer out of view bounds
pointer view escaping storage root bounds
invalid pointer ordering/difference across provenance
non-integral T element distance in pointer subtraction
read uninitialized raw object
initialized region overlap
reinterpret type confusion
misaligned typed access
free borrowed/interior/non-owner pointer
```

不存在通过 `unsafe` 绕过上述规则的入口。

---

# 36. Runtime check 边界

允许 runtime check 的典型情况：

```text
dynamic pointer bounds
dynamic runtime provenance equality
dynamic alignment condition
dynamic raw initialization region overlap / identity
pointer-difference stride divisibility
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

# 37. 当前明确不进入本计划的范围

## 37.1 一般用户泛型系统

`malloc<T>` / `reinterpret<T>` 属于语言 intrinsic generic syntax，不要求本计划同时实现一般泛型函数、trait bound 或 `where`。

一般泛型应另立设计。

## 37.2 整数隐式 promotion 新规则

ownership / pointer 计划不顺带修改整个整数类型提升系统。

需要改变时另立规范。

## 37.3 FFI

不定义 C ABI / external ABI。

本文的 32-byte pointer capability parameter / sret 规则属于 Alias internal ABI；当前目标本身仍是 64-bit Windows x64。

## 37.4 用户自定义 destructor

当前 destruction pipeline 由语言/runtime 自动控制。

不提供用户级：

```text
destructor
drop
deinit
```

## 37.5 Stored borrow / weak graph

v1 明确不提供。

## 37.6 Raw allocation DeepClone

当前 `ptr<T>` 不开放普通 DeepClone。未来如果需要复制 raw allocation ownership graph，必须另立语义设计，不能把 32-byte pointer bit copy 当成 clone。

---

# 38. 推荐实现顺序

## 38.1 前端 / sema / HIR

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
12. DeepCloneable / ShallowCloneable predicates
13. HIR ownership operations
14. raw allocation abstract semantic nodes
15. runtime-check requirement facts
16. ptr<T> / ptr<T>? type + static relation rules
```

第 5 项可以按独立、完整的纵切逐个落地，但每个操作一旦公开，就必须同时具备 semantic resolution、resolved HIR、fail-closed validation 与真实 codegen 行为；不能只加入语法壳。`ShallowCloneable` 的递归 leaf predicate 与 user-level shallow-root legality 必须由同一 sema owner 统一裁决，不能让 parser/backend 另建类型白名单。

## 38.2 ABI / codegen 基础重构

```text
17. 把 ValueAbi 从“单 Cranelift scalar”升级为可表达 aggregate / multi-lane value
18. 增加统一 PtrLayout owner：4 x I64, 32 bytes, align 8
19. pointer expression 使用 4 SSA lanes
20. pointer parameter IndirectByValue lowering
21. pointer return ExplicitSRet lowering
22. hidden ABI prefix 支持 [sret?, globals, env, ...]
23. struct field layout 支持 32-byte pointer
24. array backing 改为 stride(T)，移除 universal 8-byte element 假设
25. result payload 改为真实 typed payload layout
```

第 18 项中的 `4 x I64` 表示四个 64-bit lane；它定义的是 32-byte capability aggregate，而不是 32-bit machine pointer。

## 38.3 Runtime provenance / raw memory

```text
26. StorageDescriptor canonical identity
27. address-taken local / global / heap object descriptor lifecycle
28. malloc raw StorageDescriptor + metadata
29. refer / deref / indexing emitter
30. pointer arithmetic / bounds checks
31. pointer equality / ordering / difference provenance checks
32. reinterpret typed-view narrowing
33. initialized-region metadata runtime
34. free / reverse-init destruction
```

## 38.4 验证

必须覆盖：

```text
64-bit target / machine-address-width contract
pointer capability ABI byte layout
nullable canonical zero
pointer parameter / sret round-trip
array<ptr<T>> stride
result<ptr<T>, E> payload layout
struct containing ptr<T>
subview equality with different bounds
same address / different provenance inequality
one-past-end
pointer arithmetic overflow / bounds
pointer subtraction provenance + stride divisibility
reinterpret lattice reset
stack/local refer descriptor lifetime
raw partial initialization / move-out / re-init / free order
ownership rules never由 pointer bits fallback 推断
```

仓库继续遵守 NO_CI 规则；验证只通过仓库规定的显式本地/代理命令执行，不新增 CI。

---

# 39. 核心不变量总结

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
!= runtime storage identity / provenance

borrow 不拥有对象
borrow 不延长 owner 生命周期
stored borrow 禁止

ownership / borrow exclusivity 静态保证
bounds / provenance / raw-init 必要时可 runtime check

move 消费 ownership capability
dynamic ownership-bearing clone 创建独立 owner
InlineValue clone 保持 InlineValue 且不产生 ownership capability
ShallowCloneable(scalar) 只表示递归 safe leaf，不开放 shallow(scalar)
合法 shallow aggregate root 创建新的 OwnedTemporary + Available
shallow backend 不得通过复制旧 aggregate pointer 制造两个 owner
ptr<T> 当前不可 DeepClone / ShallowClone

普通用户对象图只包含 owning edges
raw allocation 可以部分初始化
Initialized<T> 不能被 reinterpret 冒充成 Initialized<U>

每个可寻址 storage root 有 canonical StorageDescriptor
subview 不创建新的 provenance identity

current target
= x86_64-pc-windows-msvc
= 64-bit program / 64-bit machine address width

Windows x64 ptr<T>
= 4 x I64
= provenance + address + view_start + view_end
= 32 bytes / 256-bit aggregate / align 8

32-byte ptr<T> capability size
!= 32-bit program
!= 32-bit machine pointer

ptr<T>? 使用相同 32-byte representation
canonical null = all zero

pointer equality
= provenance + address

pointer arithmetic
= 更新 address
+ 保持 provenance / view bounds
+ checked stride / bounds

reinterpret
= 保持 provenance
+ 从 current address 建立新的 typed lattice
+ 只收窄 view

pointer parameter
= IndirectByValue(32, 8)

pointer return
= ExplicitSRet(32, 8)

ownership relation 不编码进 pointer bits
codegen 永远不能从 address / descriptor 位置猜 owner

array element layout = stride(T)
result payload = typed payload storage
任意值不再默认等于一个 I64 word

sema 决定语义
HIR 固化语义
codegen fail-closed 执行

任何旧实现都不得反过来把历史一字宽表示升级成长期语言设计
```

这套不变量是后续实现与审阅的正式基线。
