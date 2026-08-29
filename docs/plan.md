# Alias 内存、所有权与指针模型

## 1. 总体目标

Alias 使用编译期可验证的所有权与生命周期模型，同时保留显式底层内存能力。

核心目标：

* 每个动态对象在任意时刻只有一个真正的生命周期 owner；
* 可以存在任意数量的 non-owning borrow；
* borrow 不延长 owner 生命周期；
* ownership 可以转移；
* 编译器阻止 use-after-free、double free、悬空 borrow 等错误；
* 不要求开发者书写 Rust 风格生命周期参数；
* Alias 永远不提供 `unsafe`；
* 支持显式 `malloc` / `free`；
* 编译器能够自动管理普通语言对象的销毁；
* 底层能力存在，但不把内部生命周期状态暴露成用户 API。

---

# 2. 基础 ownership 模型

一个动态对象的生命周期关系分为：

```text
owning edge
borrow edge
inline value
```

### owning edge

A owns B：

```text
A
└── owns B
```

B 的生命周期依附于这个 ownership，除非 B 的 ownership 被显式转移。

一个对象在任意时刻只能有一个 owner。

### borrow edge

A borrows B：

```text
A
----> B
```

A 可以访问 B，但：

* A 不拥有 B；
* A 不能决定 B 的销毁；
* borrow 不延长 B 的 owner 生命周期。

### inline value

整数、浮点、bool 等普通值直接存在于存储位置中，不需要独立 ownership allocation。

---

# 3. Ownership graph

ownership graph 不允许 ownership cycle。

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

borrow/reference graph 可以形成环：

```text
A borrows B
B borrows A
```

只要生命周期分析能够保证所有 borrow 有效即可。

---

# 4. 四种基本值操作

Alias 的对象语义有四个基本操作：

```alias
a = b
a = shallow(b)
a = borrow(b)
a = move(b)
```

## 4.1 普通 `=`

```alias
a = b
```

当 RHS 最终确定为合法 readable source，且该类型支持 deep clone 时，对具有 ownership 的对象执行 **deep clone**。

普通 `=` 不要求 RHS 自身是 owner。RHS 可以来自 owning place，也可以来自合法、当前可读的 borrow。deep clone 只读取源对象，不取得源 ownership，也不改变原有 owner / borrow 关系。

结果：

```text
b
└── original ownership tree

a
└── completely new ownership tree
```

`a` 和 `b` 是两个独立 owner。

deep clone：

* 沿 owning edges 递归复制；
* borrow/reference edges 不递归复制；
* borrow edges 仍然指向原来的外部对象。

因此：

```alias
val User owner = User(...)
val User view = borrow(owner)
val User copy = view
```

合法，并按：

```text
readable borrowed RHS
→ deep clone
→ copy 获得全新的 ownership tree
```

执行。`owner` 与 `view` 的既有关系不变。

`=` 不在 RHS 尚未得到确定结果时提前生效。

因此对于三元表达式、`match` 等值表达式：

```text
先完整求值 RHS
↓
得到唯一确定结果
↓
再进入 = 的语义
```

### `malloc` 结果的特殊 ownership 语义

如果 RHS 最终确定为 `malloc(...)` 产生、尚未移交给其它 owner 的新 allocation ownership，则 `=` **不执行 deep clone**，而是直接把该 ownership 移交给目标 owning slot。

例如：

```alias
buffer.data = malloc<u8>(1024)
```

语义是：

```text
malloc 创建 allocation
↓
产生新的 allocation ownership
↓
ownership 直接移交给 buffer.data
↓
buffer 成为该 allocation 所在 ownership tree 的 owner
```

不能先 deep clone 再把原 `malloc` allocation 遗弃，否则原 allocation 将没有可用于 `free` 的 owner。

同一规则适用于 RHS 先经过三元、`match` 等表达式后，最终确定结果仍然是该 `malloc` allocation ownership 的情况。

---

## 4.2 `shallow`

```alias
a = shallow(b)
```

执行 **shallow clone**。

产生新的 root owner，但只复制当前层。

重要限制：

如果当前对象直接包含 owning dynamic edges，而 shallow clone 会导致两个 root 同时 owning 同一个 child，则该 shallow clone 非法。

因此 generic shallow clone 只有在不会复制 owning dynamic child ownership 时才合法。

否则编译器拒绝。

`malloc(...)` 产生的新 allocation ownership 不能被 `shallow(...)` 包装。

---

## 4.3 `borrow`

```alias
a = borrow(b)
```

不产生新 owner。

`a` 和 `b` 指向同一个对象。

```text
b = owner
a = non-owning borrow
```

borrow 不延长 owner 生命周期。

`malloc(...)` 产生的新 allocation ownership 不能被 `borrow(...)` 包装。

---

## 4.4 `move`

```alias
a = move(b)
```

ownership 从 `b` 转移到 `a`。

不复制对象。

```text
before:
b owns Object

after:
a owns Object
b = moved
```

`b` 之后不能再作为 live value 使用。

`malloc(...)` 产生的新 allocation ownership 不能被 `move(...)` 包装；该 ownership 应直接进入接收它的 owning slot。

---

# 5. Storage Slot

存储位置有两种 ownership relation：

```text
Owning
Borrowed
```

relation 是 slot 的长期契约，不由每一次 RHS 动态改变。

## Owning slot

可以接收：

```text
=           ✓
shallow()   ✓
move()      ✓
borrow()    ✗
```

普通 `=` 的 RHS 只需要是合法 readable source，不要求 RHS 自身是 owner。owning slot 对普通已有对象建立 deep-cloned owner；如果最终接收的是 `malloc(...)` 产生的新 allocation ownership，则按第 4.1 节直接接管 ownership，不执行 deep clone。

## Borrowed slot

可以接收：

```text
borrow()    ✓
=           ✗
shallow()   ✗
move()      ✗
```

原因是 borrowed slot 不能突然获得 ownership。

---

# 6. 局部绑定

`val` / `var` 只描述名字能否重新绑定：

```text
val = binding 不可重新绑定
var = binding 可以重新绑定
```

它们与 ownership 完全正交。

编译器内部局部 binding 至少有：

```text
mutability:
    val | var

relation:
    owning | borrowed
```

例如：

```alias
val User a = ...
```

如果初始化结果建立 ownership，则：

```text
a:
    mutability = val
    relation = owning
```

而：

```alias
val User b = borrow(a)
```

则：

```text
b:
    mutability = val
    relation = borrowed
```

Alias 不需要暴露：

```text
owned User
borrowed User
```

这种用户类型。

## `func` binding 边界

`func` 与 `val` / `var` 是平级的 binding 声明形式，不是 Alias Type：

```alias
func i32 calculate = (i32 value) -> return value
```

因此 `func` 不能出现在 `val` / `var` 类型槽、参数类型、字段类型、返回类型、容器 element 类型或泛型类型实参中，也不存在：

```alias
val func f = ...
var func f = ...
```

`func` binding 只能在声明时由对应函数字面量直接建立，不支持把已有函数或函数值再次绑定成另一个 `func`：

```alias
func i32 original = () -> return 1
func i32 alias = original
```

上述二次绑定非法。

编译器内部仍然维护函数的完整 semantic signature，包括参数类型、返回类型与 parameter ownership effects，用于直接调用、立即调用以及 function-valued 三元 / `match` / merge 的静态检查。该 semantic signature 是编译器事实，不会把 `func` 重新变成用户类型。

---

# 7. 局部 relation

局部 slot 的 relation 在声明时建立，并在该 slot 的后续生命周期中保持不变。

典型初始化结果：

```text
普通 owning 结果
→ owning

shallow(...)
→ owning

move(...)
→ owning

borrow(...)
→ borrowed

malloc(...) ownership
→ owning
```

relation 一旦确定，`var` 后续重新绑定也必须保持相同 relation。

例如 borrowed `var` 只能重新获得 borrowed relation，不能变成 owner。

`null` 本身就是 `null`，不是 `ptr<T>`，也不携带 owning / borrowed relation。

对于仅由 `null` 初始化的 nullable local：

```alias
var T? x = null
```

声明时：

```text
relation = unresolved
nullness = null
```

之后第一次出现 non-null relation 时固定该 local 的 relation：

```text
第一次得到 owning T
→ relation = owning

第一次得到 borrowed T
→ relation = borrowed
```

一旦确定，后续保持不变。

如果不同可达控制流路径要求同一个 local 最终同时成为 owning 与 borrowed，则编译失败。

struct field 不使用这一 unresolved 规则，因为普通 struct field 已经固定为 owning slot。

### Pointer relation 与 ownership identity 传播

`ptr<T>` 类型本身仍然不编码 ownership；pointer expression 的 relation 与 allocation ownership identity 由 sema 单独维护。

```text
refer(place)
→ borrowed

p + n
p - n
→ borrowed
```

pointer arithmetic 产生的派生 pointer 不获得 allocation ownership，即使最终 offset 为 0。

`reinterpret<T>(p)` 不改变 allocation 的 owner，也绝不创建新的 ownership。它不能通过复制一个 `owning` relation 标志制造第二个 owner。

对于已经存在于 owning Place 中的 allocation owner，从该 Place 建立出的额外 pointer view 都是 non-owning view。

对于尚未落入 owning Place 的 fresh allocation ownership，sema 维护唯一的 ownership identity/token。该 token 可以随 RHS 的确定性表达式继续传播到最终 `=`，但任何中间表达式都不能复制出第二份 ownership。最终 `=` 仍按已经确定的 fresh allocation ownership transfer 规则把这唯一 ownership 移交给目标 owning slot。

`deref(ptr)` 与 `ptr[index]` 产生的 Place 不能获得超过来源 pointer relation / ownership identity 所允许的权限。

因此 borrowed pointer 不能通过 `deref` / indexing 绕过 ownership，例如不能通过：

```alias
move(deref(refer(x)))
```

取得 `x` 的 ownership。

---

# 8. `val` 不等于对象只读

`val` 只禁止名字重新绑定。

对象内部是否可以修改，取决于：

* 字段自己的可变规则；
* 当前访问是否获得合法的写权限；
* borrow 冲突分析。

因此：

```text
binding mutability
object mutability
borrow exclusivity
```

是三个不同概念。

---

# 9. Borrow 模型

用户使用：

```alias
borrow(...)
```

Alias 不暴露 Rust 风格：

```text
&T
&mut T
```

编译器根据实际使用情况内部区分：

```text
read borrow
exclusive write borrow
```

规则：

* 多个 read borrow 可以共存；
* exclusive write borrow 必须独占；
* exclusive write borrow 存在时不能有冲突的其它 borrow；
* owner 不能在 live borrow 存在期间被 move、free 或结束生命周期。

borrow 使用非词法生命周期。

也就是说 borrow 生命周期结束于最后一次实际使用，而不是整个 block 结束。

### Closure capture

closure 捕获外层 binding 时直接使用同一套 borrow 规则，不建立另一套 capture ownership 模型。

例如：

```alias
var i32 x = 0
func unit show = () -> println x
```

`show` 对 `x` 是 read borrow。

而：

```alias
var i32 x = 0
func unit change = () -> increase x
```

`change` 对 `x` 是 exclusive write borrow。

closure capture 同样服从 NLL 与普通 borrow 冲突规则。

---

# 10. Place

Alias 的成员访问、容器元素访问和 pointer dereference，本质上首先得到的是：

```text
Place
```

即：

> 一个确定的存储位置。

而不是立即 shallow、borrow 或 move。

典型 place：

```text
局部变量 slot
struct field
array element
pointer dereference
pointer indexing
```

之后外层操作决定如何处理这个 place：

```text
read
write
borrow
move
shallow
deep clone
```

---

# 11. Struct 字段

普通 struct 字段始终是 owning slot。

不存在 borrowed field。

字段 relation 属于字段声明契约，而不是由每次赋值决定。

因此 owning field 可以接收：

```text
普通赋值     → deep clone
shallow      → shallow clone
move         → ownership transfer into field
malloc 结果   → allocation ownership 直接移交进 field
borrow       → 非法
```

例如：

```alias
buffer.data = move(ptr_data)
```

可以把 ownership 移入 field。

但普通 struct field 不允许单独 move-out，因为这会在仍然 live 的 struct 中制造 partial-move hole。

例如：

```alias
val ptr<u8>? data = move(buffer.data)
```

非法。

同理，已经由 struct owning field 持有的 allocation 不能通过：

```alias
free(buffer.data)
```

单独释放。

一旦 allocation ownership 已移交给 `buffer` 的 ownership tree，该 allocation 由 parent ownership 的 replacement / destruction 管理。

---

# 12. Array 元素

普通：

```text
array<T>
```

元素是 owning slot。

普通 array 必须维持：

```text
[0, len)
全部是有效元素
```

因此不能随意把中间元素 `move` 出去后留下 hole。

ownership extraction 必须通过能够维护容器不变量的结构操作，例如 pop/remove 等。

具体 API 不属于本内存规范。

---

# 13. Place replacement

对于一个已经 initialized 的 owning place：

```text
target = rhs
```

必须先让 RHS 得到确定结果，再开始 replacement。

replacement 顺序：

```text
1. 完整求值 RHS，并准备新的 owner/value
2. 新结果成功建立
3. 销毁旧 target
4. 把新结果提交到 target
```

不能：

```text
先销毁旧 target
→ 再创建新值
```

否则新值构造失败会破坏 target。

这也保证普通 deep-clone self-assignment 等情况安全。

### Move replacement 的 storage 不重叠规则

```alias
x = move(x)
```

是编译错误。

更一般地：

```alias
target = move(source)
```

要求 source 与 target 的 ownership storage 可证明不重叠。以下情况均非法：

```text
source == target
source 是 target 的 ancestor
target 是 source 的 ancestor
```

如果 sema 无法证明二者不重叠，则必须 fail-closed 拒绝，不能把重叠裁决留给 codegen。

当确定的 RHS 是 `malloc(...)` 产生的新 allocation ownership 时，第 4 步是 ownership transfer，而不是 deep clone。

---

# 14. 函数参数

普通函数调用不会因为普通 `=` 的 deep clone 规则而自动深克隆整个对象图。

函数参数的 ownership / borrow 契约属于编译器的静态语义信息，不要求用户书写 Rust 风格 ownership annotation。

编译器根据函数对参数的实际使用确定相应语义，例如：

```text
read borrow
exclusive write borrow
owned
```

调用必须满足该静态契约。

显式：

```alias
move(x)
```

表示把 ownership 交给需要 owned 参数的调用。

显式：

```alias
shallow(x)
```

表示传入一个新的 shallow-cloned owner；仍然必须满足 shallow clone 本身的合法性规则。

方法的隐式 `self` 使用同一套参数 ownership / borrow 规则，不建立第二套规则。

函数的 parameter ownership effects 属于完整 semantic function signature。对于 function-valued 三元、`match` 或其它控制流 merge，所有候选函数的完整 semantic signature 必须完全一致，包括每个参数的 ownership effect。

例如 read-borrow 参数与 owned 参数不属于同一 semantic function signature，不能合流：

```alias
cond ? read_only : consumes
```

当前不执行 effect merge、effect subtyping 或自动适配。由于 `func` 不是 Type，这类函数值只能继续进入合法的可执行表达式语境，不能通过 `val` / `var` 或新的 `func` binding 二次保存。

---

# 15. 函数返回

函数内部 local owner 在返回普通已确定值时，可以自动把 ownership 转移给 caller。

不要求用户写：

```text
return move(local)
```

只要编译器知道 local owner 的生命周期在函数返回时结束即可。

返回 borrow 只有在编译器能够证明被引用 owner 比返回值活得更久时才允许。

否则编译失败。

不要求开发者书写生命周期参数；需要的 borrow 来源关系属于编译器静态语义信息。

可执行函数本身不能作为函数返回值。

`return` 返回的必须是函数声明返回类型下已经确定的值，不能把可执行函数或 closure 作为返回值返回。

---

# 16. 销毁模型

销毁的是 ownership，不是变量名字。

当普通 owner 生命周期结束时，触发 destruction pipeline。

可能的触发来源：

```text
作用域结束
owner 被 replacement
parent destruction
其它 ownership consumption
```

显式 `malloc` 创建、且仍然保持为独立 allocation root owner 的 allocation 是特殊情况：

* 不能仅依靠作用域结束被隐式 `free`；
* 如果没有把 ownership 移交给其它 owner，则必须显式 `free`；
* 如果一条可达控制流路径让该独立 allocation root owner 在未 `free`、未 transfer 的情况下结束生命周期，编译失败。

如果 `malloc` allocation ownership 已移交进普通 ownership tree，则不再是独立 allocation root owner，由新的 parent owner 管理其后续生命周期。

---

# 17. 销毁顺序

ownership tree 按：

```text
children first
parent last
```

销毁。

例如：

```text
A
├── owns B
│   └── owns D
└── owns C
```

销毁顺序：

```text
D
B
C
A
```

同级 struct owning fields 按声明逆序销毁。

array owning elements 按索引逆序销毁。

如果 child 是已经移交给 parent ownership tree 的 allocation，则它在 parent destruction / replacement 过程中按正常 ownership child 处理。

---

# 18. Destroy 与 Deallocate 分离

必须区分：

```text
destroy
```

和：

```text
deallocate
```

destroy：

* 处理 owned children；
* 执行对象生命周期清理。

deallocate：

* 释放物理 storage。

因此：

```text
stack object
```

可能需要 destroy，但不需要 heap deallocate。

不能把：

```text
destroy == free physical memory
```

写死。

---

# 19. Partial initialization 与 typed initialized region

raw allocation 允许部分初始化。

由于同一段未初始化 raw storage 可以通过 `reinterpret<T>` 建立不同 typed view，因此初始化状态不能永久按 `malloc<S>` 时的 `S` element slot 作为对象类型身份。

allocation 的初始化事实概念上记录为：

```text
Uninitialized byte range

或

[start, end): Initialized<T>
```

即：raw storage 中哪些 byte range 当前承载了什么已初始化对象。

例如一段 16-byte raw storage 可以处于：

```text
[0, 8)   Initialized<u64>
[8, 16)  Uninitialized
```

这是合法状态。

不同 initialized object region 不允许重叠。

初始化状态属于编译器/runtime 内部语义。

Alias 用户不能：

```text
is_initialized(...)
```

不能直接查询、修改 initialization metadata。

---

# 20. 未初始化 place

未初始化 place 可以：

```text
定位                ✓
通过 pointer 指向    ✓
首次写入             ✓
```

不能：

```text
读取                ✗
访问成员            ✗
调用方法            ✗
borrow 其中的 T     ✗
move T              ✗
destroy T           ✗
```

因为那里还没有合法的 `T` 对象。

---

# 21. 首次写入

Alias 不提供单独的：

```text
init(...)
initialize(...)
construct_at(...)
```

首次写入一个 uninitialized typed place 就是初始化。

即：

```text
Uninitialized range + first write T
→ 建立 Initialized<T> region
```

再次写入同一个已经 initialized 的 owning place：

```text
Initialized<T> + write
→ replacement
```

初始化不是新的 value operation，只是 storage/object lifecycle 状态转换。

---

# 22. 从 raw allocation move-out

raw allocation 允许出现 hole。

如果一个 `Initialized<T>` raw object 的 ownership 被完整 move 出：

```text
Initialized<T>
→ move-out
→ 对应 byte range 回到 Uninitialized
```

该 range 可以之后再次初始化。

如果直接销毁其中对象：

```text
Initialized<T>
→ destroy
→ 对应 byte range 回到 Uninitialized
```

move-out 和 destroy 的结果状态相同，但对象生命周期不同：

```text
move-out
→ 对象继续存在于新 owner

destroy
→ 对象生命周期结束
```

---

# 23. Pointer validity 与 pointee initialization 分离

pointer 可以合法指向未初始化 storage。

因此：

```text
pointer valid
```

不等于：

```text
pointee initialized
```

一个 pointer view 可以经历：

```text
目标 range initialized
↓
move-out / destroy
目标 range uninitialized
↓
再次首次写入
目标 range initialized
```

pointer 本身始终可以保持有效。

真正让 pointer 失效的是其 allocation 生命周期结束。

---

# 24. Pointer 类型与 nullable

正式 pointer 类型：

```alias
ptr<T>
```

nullable pointer：

```alias
ptr<T>?
```

`ptr<T>` 是 non-null pointer。

`ptr<T>?` 可以为 `null`。

ownership 不编码在 pointer 类型里。

不存在：

```text
OwnedPtr<T>
BorrowedPtr<T>
```

这种类型。

nullable 是通用能力，不是 pointer 专属。

nullable 只允许在 non-nullable `T` 上建立一层。Alias 不支持嵌套 nullable：

```alias
T??
```

非法；nullable modifier 不能再次应用到已经是 `T?` 的类型。

`null` 本身就是 `null`，不是 `ptr<T>`。

`null` 可以进入 nullable 语境；不能因为 `null` 本身而把它解释成某个具体 pointer 类型或 pointer ownership relation。

基本 nullable 转换规则：

```text
T    → T?   允许
null → T?   允许
T?   → T    不允许隐式转换
null → T    非法
```

`T? → T` 只能依赖控制流 narrowing。

在编译器能够证明 `T?` 当前 non-null 的控制流区域中，可以把它作为对应 non-null `T` 使用，不要求额外 `unwrap()` 仪式。

nullable 的：

```text
nullness
ownership relation
```

是两个独立状态。

对于 owning nullable slot，从 non-null owned payload replacement 为 `null` 时：

```text
销毁旧 owned payload
↓
slot = null
```

borrowed nullable slot 可以处于 `null` 或 borrowed `T` 状态；写入 `null` 不会让它获得 ownership。

---

# 25. Allocation

一个 allocation 是：

> 一段拥有独立 provenance 和生命周期的连续 storage。

概念上至少包含：

```text
base
extent
alignment
provenance
owner
typed initialization regions
```

allocation ownership 属于整个 allocation，不属于某个 element。

---

# 26. 一般泛型函数与单态化

Alias 一般泛型函数声明语法：

```alias
func 返回类型 函数名<类型参数...> = (参数...) -> 函数体
```

泛型函数调用：

```alias
函数名<具体类型...>(参数...)
```

泛型函数最终单态化。

类型参数 `T` 在某个具体单态化实例中能够执行哪些操作，取决于该实例最终具体类型本身支持哪些操作。

也就是说，泛型函数不会因为 `T` 是抽象类型参数就提前假定一套额外能力；具体实例是否合法由最终单态化后的具体类型与函数体操作共同决定。

当前不引入：

```text
trait bounds
where clauses
```

---

# 27. `malloc`

`malloc` 是泛型函数。

`malloc` 的类型参数必须显式提供。

示例：

```alias
var ptr<User>? ptr_user = malloc<User>()
```

以及批量分配：

```alias
var ptr<User>? ptr_user = malloc<User>(count)
```

`count` 使用 Alias 已有的无符号整数体系：

```text
u8
u16
u32
u64
```

不为 `malloc` 新增 `usize`。

`malloc<T>()` 严格等价于：

```alias
malloc<T>(1)
```

批量 allocation 的 byte extent：

```text
extent_bytes = count × stride(T)
```

其中 `count × stride(T)` 按数学意义上的精确结果检查，不先按 Alias 整数类型发生回绕。

```text
count == 0
→ null

count × stride(T) 超过目标平台可分配 / 可寻址 extent
→ null
```

上述情况都属于 `malloc` allocation failure。

---

# 28. `malloc` 返回与 ownership 语义

`malloc` 返回：

```alias
ptr<T>?
```

成功：

```text
non-null allocation ownership
```

失败：

```text
null
```

这沿用类似 C `malloc` 的失败模型，但通过 Alias nullable 类型系统表达。

不另外提供：

```text
try_malloc
allocation result wrapper
```

`malloc(...)` 成功产生的是新的 allocation ownership。

如果该 ownership 直接作为 `=` 的最终 RHS 进入 owning slot，则 ownership 直接移交给该 slot，不执行 deep clone。

不能对尚未移交的 `malloc(...)` allocation ownership使用：

```alias
shallow(malloc<T>(...))
borrow(malloc<T>(...))
move(malloc<T>(...))
```

---

# 29. `malloc` 的 storage 状态

`malloc` 只负责 allocation。

不会自动构造 `T`。

新 allocation 初始全部是：

```text
Uninitialized raw storage
```

之后通过正常 typed place 首次写入逐步建立 `Initialized<T>` 等 typed object region。

---

# 30. Allocation failure

显式：

```alias
malloc<T>(...)
```

分配失败返回：

```text
null
```

普通语言语义自动触发的内存分配，例如 deep clone 等，如果发生 OOM，则：

```text
runtime abort
```

不把普通赋值或普通语言对象操作改造成额外的 recoverable allocation API。

---

# 31. `free`

`free` 的返回类型是：

```alias
unit
```

`free` **没有返回值**。

`unit` 在 Alias 中不是值类型，只表示函数没有返回值。

---

# 32. `free(null)`

```alias
free(null)
```

合法。

效果：

```text
不析构
不释放
不报错
```

如果一个 nullable allocation root owner 当前运行时值为 null，`free` 同样什么都不做。

但静态 ownership 仍被消费。

因此一个 owner 不能因为可能为 null 就被多次 free。

---

# 33. `free` 的 ownership 规则

只有当前仍然作为独立 allocation root owner 存在的 allocation ownership 可以显式 `free`。

可以：

```text
allocation root owner
nullable allocation root owner
```

不能：

```text
borrowed pointer
interior non-owner pointer
moved owner
已经 consumed 的 owner
已经移交给普通 ownership parent 的 field/child
```

因此：

```alias
free(buffer.data)
```

在 `buffer.data` 已经通过 ownership transfer 成为 `buffer` ownership tree 的 child 时非法。

`free` 一个 non-null 独立 allocation root：

```text
销毁所有仍 initialized 的对象
↓
deallocate storage
↓
owner consumed
```

未初始化 region 不执行对象析构。

---

# 34. Pointer provenance

pointer 不只是一个机器整数地址。

语言语义上 pointer 必须保留：

```text
allocation identity
allocation bounds
view bounds
offset
type view
lifetime/provenance relation
```

其中 `allocation bounds` 描述整个 allocation 的合法 storage 范围，`view bounds` 描述当前 pointer view 被允许访问的子范围。pointer 操作必须同时服从两者。

```text
malloc<T>(n)
→ 初始 T view extent = n

refer(single T place)
→ 初始 T view extent = 1
```

view bounds 不因为 pointer arithmetic 自动扩大。

机器地址只是最终物理表示的一部分。

两个地址数值碰巧相同，不代表 pointer 在语言语义上具有相同 provenance。

---

# 35. Interior pointer

`ptr<T>` 可以指向 allocation 内部合法位置。

内部位置可以理解成：

```text
allocation provenance + offset + typed view
```

物理地址：

```text
base + offset
```

不需要另外发明 interior pointer 类型。

---

# 36. Allocation ownership 与 pointer offset 分离

allocation owner 永远拥有整个 allocation。

pointer 偏移不会把 ownership 转移给内部元素。

因此：

```text
independent root owner
→ 可以 free

interior pointer
→ 不能 free
```

即使一个 interior pointer 通过算术重新回到 offset 0，也不会因此自动获得 ownership。

ownership provenance 和 address offset 是两个独立维度。

---

# 37. `refer`

Place → pointer 的正式用户语法：

```alias
refer(place)
```

语义：

```text
Place
↓ refer
non-owning ptr<T>
```

`refer`：

* 不产生 allocation ownership；
* 不决定所指对象销毁；
* 可以指向一个合法的未初始化 place；
* 不把 pointer ownership 编码进 `ptr<T>` 类型。

取得某个 place 的 pointer 不等于获得该 place 的 ownership。

---

# 38. `deref`

Pointer → Place 的正式用户语法：

```alias
deref(ptr)
```

Alias 不使用 `*ptr` 作为 dereference 语法。

`deref` 的语义：

```text
ptr<T>
↓ deref
对应的 T storage Place
```

它本身不是：

```text
borrow
shallow
move
deep clone
```

外层操作才决定对这个 Place 做什么。

nullable pointer 必须先处于已证明 non-null 的控制流状态，才能作为对应 non-null pointer dereference。

---

# 39. Pointer indexing

正式语法：

```alias
ptr[index]
```

语义上等价于定位到对应的 element Place。

不自动 shallow、borrow、move 或 deep clone。

---

# 40. Pointer arithmetic

允许：

```alias
ptr + integer
ptr - integer
```

整数单位是 `T` 元素，而不是 byte。

即：

```text
ptr + 1
```

表示移动一个 `T` element stride。

---

# 41. Pointer difference

同一 allocation 内两个 `ptr<T>` 可以做差。

结果静态类型固定为：

```alias
i64
```

结果表示有符号 element distance：

```text
(offset(p) - offset(q)) / stride(T)
```

不是 byte distance。

不同 allocation 之间 pointer subtraction 非法。

为了保证所有合法 pointer difference 都能由 `i64` 表示，单个 allocation 的 element count 不得超过 `i64::MAX`。

如果 `malloc<T>(count)` 请求成功后会违反这一限制，则视为 allocation failure，返回 `null`。

---

# 42. Pointer comparison

相等比较：

```alias
p == q
p != q
```

可以正常使用。

pointer identity 基于：

```text
provenance + offset
```

顺序比较：

```alias
p < q
p <= q
p > q
p >= q
```

只允许同一 allocation 内比较 offset 顺序。

---

# 43. One-past-end

pointer 可以存在于：

```text
allocation 最后一个 element 之后一个位置
```

即 one-past-end。

它可以：

```text
比较
做差
作为遍历终点
```

但不能：

```text
deref
读取
写入
```

超过 one-past-end 的 pointer position 非法。

---

# 44. Bounds

pointer arithmetic、index、dereference 和 `reinterpret` 必须同时受到：

```text
allocation bounds
view bounds
```

约束。

pointer arithmetic 不得把合法 view 扩张到来源 view bounds 之外；`index` / `deref` 只能访问当前 view bounds 内的合法 element；`reinterpret` 建立的目标 typed view 也不得越过来源 pointer 的 view bounds 或 allocation bounds。

如果编译器静态能够证明越界：

```text
compile error
```

动态情况下由编译器插入必要检查。

Alias 不允许合法程序通过普通 pointer 操作制造未定义越界内存访问。

---

# 45. Alignment 与 layout

每个可存储类型 `T` 在编译器内部都有：

```text
size(T)
align(T)
stride(T)
```

满足：

```text
stride(T) >= size(T)
stride(T) 是 align(T) 的整数倍
```

连续 element：

```text
element i address
= base + i × stride(T)
```

allocation base 必须满足相应 alignment 要求。

---

# 46. Struct layout

普通 struct：

* 字段物理顺序保持声明顺序；
* 每个字段按自身 alignment 对齐；
* 必要时插入 padding；
* struct alignment 为字段最大 alignment；
* 尾部 padding 保证连续 struct element 正确对齐。

具体 byte offset 属于目标 ABI，不默认承诺跨平台固定。

---

# 47. 可存储类型

任何实际存储类型必须具有确定：

```text
size
alignment
stride
```

`unit` 不是值类型，因此不能被 allocation。

`func` 是 binding 声明形式，不是 Type；可执行函数或函数字面量也不能作为可存储类型被 allocation，不能作为字段、容器 element 或泛型类型实参。

不为这一规则引入用户可见 `Sized` trait。

---

# 48. 零大小类型

任何实际可存储类型的 stride 必须大于 0。

即便未来允许空 struct，也应保证不同 array/allocation element 具有不同的 storage identity。

---

# 49. Dynamic object 的直接大小

`size(T)` 只计算 T 的直接 storage representation。

例如 string、array 等拥有其它 allocation 时：

```text
size(string)
```

不是字符串内容总字节数。

```text
size(array<T>)
```

也不是 backing elements 总大小。

ownership subtree 不递归计算进 `size(T)`。

---

# 50. Raw allocation 与 array 的区别

raw allocation：

```text
允许部分初始化
允许 holes
允许 initialized object → move-out → uninitialized range
允许通过合法 typed view 在 uninitialized range 上建立新对象
```

普通 `array<T>`：

```text
[0, len)
必须全部 initialized
不允许用户随意制造 hole
```

array 可以在底层建立于 raw allocation 之上，但对用户暴露更强的不变量。

---

# 51. Pointer typed view

allocation 本质是一段具有 provenance 的 storage。

`ptr<T>` 是对某个位置的 typed view。

pointer typed view 的建立或变化：

* 不创建新 allocation；
* 不改变 provenance；
* 不改变 ownership；
* 不自动创建新对象。

---

# 52. 已初始化对象的类型身份

如果某段 storage 当前承载：

```text
Initialized<T>
```

不能仅靠 pointer typed view 变化把它直接冒充：

```text
Initialized<U>
```

对象类型变化必须经过正常生命周期转换。

不能通过 `reinterpret` 绕过：

```text
ownership
initialization
layout
destruction
```

如果 `reinterpret<T>` 的目标 byte range 当前存在 `Initialized<S>`，且 `S != T`，则 `reinterpret<T>(...)` 本身非法。

不能先建立异型 pointer view，再把“是否使用该 view”留到之后判断。要把同一 storage 改为另一对象类型，必须先结束原 `Initialized<S>` 的正常生命周期，使对应 range 回到 `Uninitialized`。

---

# 53. `reinterpret<T>`

正式用户语法：

```alias
reinterpret<T>(ptr)
```

语义：

```text
ptr<S>
↓ reinterpret<T>
ptr<T>
```

pointer view 使用 byte range 与当前位置表示：

```text
[view_start, view_end)
current
```

`reinterpret<T>` 在保持以下内容不变的前提下，为同一 raw storage 建立新的 typed view：

```text
allocation provenance
ownership
view_start
view_end
current
```

目标 type view 变为 `T`。从 `current` 开始，当前 `ptr<T>` 可访问的完整 element 数为：

```text
floor((view_end - current) / stride(T))
```

尾部不足一个完整 `T` 的 bytes 可以保留在原 byte view range 中，但不能通过当前 `ptr<T>` dereference 或 indexing。整个 byte range 不要求能够被 `stride(T)` 整除。

`reinterpret` 本身：

* 不创建新 allocation；
* 不创建对象；
* 不 move；
* 不 borrow；
* 不 shallow clone；
* 不 deep clone。

目标 typed view 必须满足：

```text
size(T)
align(T)
view bounds
allocation bounds
```

如果静态能够证明不满足，编译错误。

如果只能动态确定，则允许编译器插入必要安全检查；检查失败时 runtime abort。

---

# 54. 未初始化 storage 的重新 typed view

如果目标 storage byte range 当前没有 initialized object，则可以在满足：

```text
size
alignment
bounds
```

条件时通过：

```alias
reinterpret<T>(ptr)
```

建立新的 `ptr<T>` typed view。

建立 view 本身不会创建对象。

只有后续通过该 typed view 对合法 uninitialized range 首次写入时，才建立：

```text
Initialized<T>
```

例如：

```text
16-byte allocation 初始：
[0,16) Uninitialized

建立 ptr<u64> view 并首次写入一个 u64 后：
[0,8)  Initialized<u64>
[8,16) Uninitialized
```

不能把它解释成若干 `Initialized<u8>`，因为真正被建立的是 `u64` 对象。

如果要把同一 storage range 重新作为另一类型对象使用，必须先结束原 initialized object 的正常生命周期，使对应 range 回到 Uninitialized，再进行新的首次写入。

---

# 55. Pointer 与整数

`ptr<T>` 不是普通整数。

pointer 可以观察其机器地址值。

地址值可以由当前目标平台能够完整承载地址的现有无符号整数类型表示。

当前 Windows x64 对应 `u64`。

不新增 `usize`。

地址整数：

* 不携带 provenance；
* 不携带 ownership；
* 只是整数值。

普通整数不能直接恢复成可安全 dereference 的 Alias pointer。

---

# 56. 安全整数隐式提升

Alias 允许安全的整数隐式提升。

定义：

> 当源整数类型的全部值域都是目标整数类型值域的子集时，允许从源整数类型到目标整数类型的隐式提升。

形式化表示：

```text
Values(Source) ⊆ Values(Target)
→ implicit integer promotion allowed
```

这种隐式提升必须保持原数值，不允许通过隐式提升发生：

```text
截断
回绕
符号重解释
```

相同类型之间是 identity，不称为 promotion。

当前整数类型之间允许的非 identity promotion：

```text
i8  → i16, i32, i64
i16 → i32, i64
i32 → i64

u8  → u16, u32, u64, i16, i32, i64
u16 → u32, u64, i32, i64
u32 → u64, i64
```

例如以下都不允许：

```text
i8  → u8
u8  → i8
i32 → u64
```

### 有明确 target type 的上下文

只要上下文已经确定目标整数类型，就允许按上述规则进行安全 promotion。

包括：

```text
binding initialization
assignment
function argument
function return
field / constructor input
array element target
result payload target
```

例如：

```alias
val i64 x = i32_value
```

允许 `i32 → i64`。

如果某个已确定函数参数类型为 `i64`，则传入 `i32` argument 时允许 `i32 → i64`。

### 二元表达式

二元整数表达式不搜索第三种公共整数类型。

二元表达式必须先独立完成自身定型与运算，外层 target type 只能在二元表达式得到最终结果后提升该结果，不能反向提前提升 operands。

例如：

```alias
val i64 z = i32_a + i32_b
```

按以下顺序处理：

```text
i32 + i32
→ binary result = i32
→ 按 i32 宽度执行 checked 运算
→ 外层 target = i64
→ 最终 i32 result 安全提升到 i64
```

不能因为外层 target 是 `i64`，就先把两个 i32 operand 提升到 i64。

只有当一侧能够安全提升到另一侧已经存在的整数类型时，才允许自动 promotion。

例如：

```text
i32 + i64
→ i64

u8 + i16
→ i16
```

同理：

```text
i32 + i64
→ i32 operand 安全提升到已经存在的 i64 operand type
→ i64 + i64
→ i64
```

但：

```text
i32 + u32
```

即使 `i64` 能容纳双方，也不会自动合成为 `i64 + i64`，因此是 type error。

### Integer literal

integer literal 的 contextual typing 与整数 promotion 分开处理。

例如：

```alias
val i8 x = 1
```

其中 `1` 直接按目标上下文确定为可表示的 `i8` literal，不解释成“先得到另一整数类型，再向 `i8` 做隐式窄化”。

### 泛型

泛型 concrete type 必须先由显式类型实参与单态化确定，再应用整数 promotion。

例如：

```alias
f<i64>(i32_value)
```

先确定 `f<i64>` 的 concrete parameter type，再允许 `i32 → i64`。

整数 promotion 不参与寻找或推导泛型类型参数。

---

# 57. 编译器静态状态

ownership、初始化、nullable、borrow 等状态属于编译器分析事实，不应该全部进入用户类型。

典型内部状态：

```text
relation:
    unresolved
    owning
    borrowed

liveness:
    uninitialized
    live
    moved
    consumed

nullness:
    null
    non-null
    unknown

borrow state:
    read
    exclusive write

ownership identity:
    none
    unique token

pointer view state:
    allocation byte bounds
    view_start
    view_end
    current
    type view
    provenance

function semantic signature:
    parameter types
    return type
    parameter ownership effects
```

其中 fresh allocation ownership 的 unique token 不可复制；ownership transfer 只移动该 token。额外 pointer view 不能因为地址、offset 或 typed view 与 owner 相同而获得第二份 token。

raw allocation 还需要维护 typed initialized region 信息。

这些状态由 sema / 数据流分析维护。

---

# 58. HIR 原则

进入 codegen 之前，sema 应该已经明确最终内存语义，例如：

```text
DeepClone
ShallowClone
Borrow
Move
InitializePlace
ReplacePlace
ReferPlace
DerefPlace
PointerOffset
PointerDifference
ReinterpretPointer
Free
```

对于 `=`：

```text
RHS 必须先得到确定结果
↓
sema 确定最终是 DeepClone、malloc ownership transfer 或其它已定义结果
↓
codegen 只执行已解析语义
```

`DeepClone` 必须明确其 source 是已经验证可读的 Place / Value；source 可以是 owner，也可以是合法 borrow，codegen 不再判断 source relation。

`Move` / `ReplacePlace` 必须携带 sema 已确认的 ownership storage 不重叠事实。self-move、ancestor/descendant 重叠或无法证明不重叠的情况都必须在 HIR 前失败。

function-valued 三元、`match` 或其它 merge 进入 HIR 前，sema 必须已经验证所有候选的完整 semantic function signature 精确一致，包括 parameter ownership effects；HIR/codegen 不执行 effect merge 或适配。

pointer 相关 HIR 还必须携带 sema 已确定的 provenance、allocation bounds、byte-range view bounds、current、type view、可访问完整 element extent 以及必要的 ownership identity 信息。codegen 不能因为某个 pointer 最终位于 allocation offset 0，就自行推断它拥有 allocation ownership。

整数 promotion 必须以 resolved conversion 进入 HIR。二元表达式先按自身已确定类型完成运算，外层 target promotion 只包裹最终结果；codegen 不得把外层目标反向传播给已经定型的 operands。

codegen 不允许根据源码 AST 形态重新猜语义。

这与 Alias 已经采用的：

```text
sema 决定最终语义
codegen fail-closed
```

架构保持一致。

---

# 59. Place 与 Value 必须区分

编译器内部不能把所有表达式都看成“产生 value”。

必须明确：

```text
Place
Value
```

例如：

```alias
deref(ptr)
```

产生 Place。

```alias
ptr[index]
```

产生 Place。

```alias
object.field
```

产生 Place。

```alias
refer(place)
```

从 Place 建立 pointer view。

而：

```text
read place
deep clone place
shallow place
borrow place
move from place
write place
```

才产生具体 value/lifecycle 行为。

---

# 60. 编译期必须阻止

Alias 的安全模型必须阻止：

```text
double free
use after free
use after move
borrow outlives owner
free while live borrow exists
move owner while live borrow exists
读取未初始化 object/range
deref one-past-end
pointer 越界
borrowed pointer free
interior pointer free
free 已移交给 parent 的 allocation child
ownership cycle
非法 shallow clone
owning slot 接收 borrow
borrowed slot 获得 ownership
普通 struct field move-out
普通 array element 随意 move-out 留 hole
不同 initialized object region 非法重叠
通过 reinterpret 把 Initialized<T> 冒充成 Initialized<U>
独立 malloc root owner 未 free、未 transfer 就结束生命周期
```

这些不是 `unsafe` 下允许发生的行为。

Alias 没有 `unsafe`。

---

# 61. 编译器不能静态完全确定的情况

如果安全性可以通过受控 runtime metadata/check 保证，则允许编译器插入内部检查。

例如：

```text
动态 pointer bounds
动态部分初始化位置
reinterpret 的动态 bounds/alignment 条件
```

这些内部检查不暴露：

```text
is_initialized
bounds_metadata
allocation_id
```

之类用户 API。

如果连通过受控 runtime check 都无法保证语言语义安全，则编译拒绝。

---

# 62. 当前明确不处理的范围

以下内容暂时不进入这一版：

### 外部语言 / FFI

包括：

```text
C ABI
```

甚至可能以后都不设计。

### 用户自定义析构

当前 destruction pipeline 由语言/runtime 自动执行。

暂不提供用户级：

```text
destructor
drop
deinit
```

相关语法。

### 泛型约束系统

当前一般泛型函数最终通过单态化按具体类型能力检查。

暂不引入：

```text
trait bounds
where clauses
```

---

# 63. 用户层核心能力总结

用户层核心操作：

```alias
a = b
a = shallow(b)
a = borrow(b)
a = move(b)
```

pointer / allocation 能力：

```alias
ptr<T>
ptr<T>?
malloc<T>(...)
free(...)
refer(place)
deref(ptr)
reinterpret<T>(ptr)
ptr[index]
ptr + n
ptr - n
ptr_a - ptr_b
```

其中：

```text
=                普通已有 owner → deep clone
= malloc result  malloc allocation ownership → 直接移交
shallow           shallow clone
borrow            non-owning access
move              ownership transfer

malloc            explicit allocation
free              显式结束独立 allocation root ownership

refer             Place → non-owning pointer
deref              pointer → Place
reinterpret<T>    同一 raw storage 上建立新的 ptr<T> typed view
ptr[index]        locate element Place
```

Alias 不使用晦涩的 `*` / `&` pointer 语法承担 refer/deref 语义。

ownership、borrow region、allocation provenance、typed initialization tracking、destruction、rollback、bounds metadata 等复杂状态由编译器负责。

---

# 64. 整体生命周期

普通 ownership 生命周期可以概括为：

```text
construct / obtain value
        ↓
ownership established
        ↓
read / write / borrow / shallow / move / deep clone
        ↓
ownership eventually consumed or parent ends
        ↓
destroy initialized ownership subtree
        ↓
deallocate physical storage if required
```

显式 `malloc` allocation root：

```text
malloc
  ↓
独立 allocation root owner
  ↓
显式 free
或 ownership transfer 给其它 owner
```

如果 ownership 已 transfer 给普通 ownership parent：

```text
parent
└── owns allocation
```

之后由 parent 的 replacement / destruction 管理。

整个过程中：

```text
一个对象只有一个真正 owner
borrow 不拥有对象
move 转移 owner
shallow 在合法时创建新的 shallow owner
deep clone 创建独立 ownership tree
malloc 创建新的 allocation ownership
free 只显式消费独立 allocation root owner
refer 不获得 ownership
deref 只定位 Place
reinterpret 不改变 provenance / owner，也不创建新 ownership
```

这是 Alias 当前整套内存、ownership、pointer 与相关泛型/整数提升计划的核心。
