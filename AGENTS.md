# Alias 工程知识库

**同步日期：** 2026-08-29  
**基线分支：** `main`  
**当前语言规范：** `docs/spec-notes.md`  
**已冻结的内存 / ownership / pointer 目标设计：** `docs/plan.md`

> 本文件只拥有 **当前工程结构、架构边界、代码维护不变式与验证方式**。  
> 语言语法、类型规则、Pattern、转换、显示等当前用户可观察语义只由 `docs/spec-notes.md` 定义。`docs/plan.md` 只拥有其明确专题中的已冻结目标设计与实现合同；在该计划尚未全部落地期间，不得把目标设计误写成“当前实现已经支持”，也不得让当前历史实现反过来覆盖计划中的正式裁决。

## 1. 项目与唯一执行模型

Alias 是 Rust 实现的静态类型原生编译语言，源文件扩展名为 `.as`。

当前唯一执行管线：

```text
source.as
  → lexer
  → parser AST
  → sema
  → CheckedProgram typed HIR
  → Ty → VTy 单次投影
  → Cranelift Object / COFF
  → rust-lld
  → Windows x64 .exe
  → 独立进程执行
```

不存在解释器、JIT、宿主函数执行后端或进程内执行生成代码的并行路径。`run` 与 `build` 必须共享同一编译/链接主链；差异只在产物生命周期。

当前目标固定为 `x86_64-pc-windows-msvc`。Rust 内部目标字符串由 `src/target.rs` 单一 owner 提供；Cargo target 配置属于独立工具边界，不要求与 Rust 常量机械共享。不得用宿主 ISA 探测替代显式目标。

## 2. 权威来源与变更顺序

开始实现、重构或审计前，按以下顺序确认当前事实与已冻结目标：

1. 当前仓库源码；
2. `docs/spec-notes.md` 当前语言规范；
3. `docs/plan.md` 已冻结的内存、ownership、borrow、raw allocation 与 pointer 目标设计；
4. 本文件中的工程边界；
5. `GENERAL_ENGINEERING_RULES.md`；
6. `NO_CI.md`。

聊天历史、旧 commit、旧 Phase 编号和迁移期间的中间设计都不能替代当前源码、当前规范与当前有效设计文档。

必须区分“当前实现事实”和“已冻结但尚未全部落地的目标合同”。`docs/spec-notes.md` 继续拥有当前用户可观察语义；执行 `docs/plan.md` 范围内的开发时，现有实现若与计划冲突，应重构现有实现，而不是把冲突包装成兼容义务或 fallback。

缺失产品、语言、架构或兼容性裁决时，必须明确标记为未知/待裁决，不能自行补全。已经在 `docs/plan.md` 冻结的内存生命周期、ownership、pointer ABI 等裁决不再属于“未知长期决策”。仍未冻结的其它长期决策（例如失败 build 对旧成功 exe 的保留策略）不得从现状反推为永久设计。

## 3. 当前源码结构与 owner

```text
src/
├── main.rs                 # CLI 参数与进程退出映射
├── lib.rs                  # run/build 编排、编译器工作线程栈；AliasError / Span
├── target.rs               # 编译器内部目标 triple owner
├── limits.rs               # 当前共享输入/表达式限制 owner
├── builtins.rs             # 预定义语言名字与 ownership intrinsic 分类 owner
├── lexer.rs                # token 化、字符串插值拆分
├── ast.rs                  # parser AST，仅语法
├── parser/                 # 语法解析
├── sema/
│   ├── mod.rs              # check(Program) -> CheckedProgram
│   ├── decls.rs / stmts.rs
│   ├── places.rs           # 递归 Place(Local/Field/Index) 解析、终端可写性与赋值目标类型检查
│   ├── exprs.rs + exprs/   # 表达式静态语义；clone/read plan、borrow/move Place resolution
│   ├── types.rs            # Ty 与类型槽检查
│   └── hir.rs + hir/       # typed HIR、lower、expr→Place、function effects、ownership/loan flow、capture、validate、visit
├── codegen/
│   ├── mod.rs              # compile_to_object(CheckedProgram)
│   ├── abi.rs              # Ty→VTy、ValueAbi、PtrLayout、结构体布局、word 编码的 canonical ABI owner
│   ├── layout.rs           # runtime heap object 物理布局 owner
│   ├── emit.rs + emit/     # HIR → Cranelift；clone.rs / shallow.rs 只执行各自 resolved plan
│   │   ├── value.rs        # resolved expression ABI 对应的 Cranelift SSA lane carrier
│   │   └── places.rs       # resolved Place storage address、物理写入与字段 storage 查询 owner
│   ├── funcgen.rs          # 用户函数/闭包生成
│   ├── runtime.rs          # RUNTIME_CONTRACTS 与 runtime 调用校验 owner
│   └── native_runtime.rs + native_runtime/ # 产物内 runtime 实现
└── linker.rs               # COFF → exe 与 Windows SDK/rust-lld 定位 owner
```

禁止创建语义不清的 `utils` / `helpers` / `common` 汇总模块。共享逻辑只能放到拥有该概念的窄 owner 中。

执行 `docs/plan.md` 不改变“一个规则一个 owner”的要求。若当前 owner 的抽象能力不足（例如 `ValueAbi` 只能表达单个 Cranelift scalar），应升级或按责任拆分 owner，而不是在 emitter、runtime 或新 helper 中建立平行 ABI 体系。

## 4. parser → sema → HIR → codegen 边界

### parser AST

`src/ast.rs` 只表达源码语法，不拥有最终静态类型、BindingId、MethodId、字段索引或最终调用目标。

parser 可以查询 `builtins.rs` 中**明确属于语法分类**的信息，但不得自行复制 builtin 字符串名单，也不得在多个 parser 文件维护平行分类。`clone` / `shallow` / `borrow` / `move` 由 `OwnershipBuiltinName` 统一分类；无括号调用和保留名判断都消费这一 owner。

### sema

sema 是语言静态语义的 owner。名字解析、目标类型传播、转换关系、调用/方法归属、Pattern coverage、字段/构造器索引，以及当前已落地的 value category / initial ownership capability / binding storage relation / Place overlap 和后续 loan、function effect 等静态事实，都必须在这里或其明确的 sema 子 owner 中完成。

显式 `clone` 的 DeepCloneable 判定与递归 `DeepClonePlan` 只由 `sema/exprs/deep_clone.rs` 决定；当前已知 owning slot 的稳定 Place 普通读取只由 `sema/exprs/ordinary_read.rs` 解析 source Place，并复用 canonical DeepClone plan owner；显式 `shallow` 的递归 ShallowCloneable 判定、standalone root legality 与 `ShallowClonePlan` 只由 `sema/exprs/shallow_clone.rs` 决定；显式 `borrow(place)` / `move(place)` 的 source Place resolution 分别只由 `sema/exprs/borrow_value.rs` / `move_value.rs` 决定。其它 checker/validator 可以调用这些 owner，但不得复制类型 capability 矩阵或从 AST 形状恢复 Place/source。generic `call()` / `resolve_call_target()` 对 ownership intrinsic 只允许 fail-closed，不得形成第二条解析路径或重新计算 plan。

检查阶段使用 AST 节点地址作为短生命周期 fact key。该 identity **只在同一次 check → lower 调用链内有效**；两阶段之间禁止移动、clone 后替换或重建 AST 节点。若未来引入 AST 重写，必须先改用稳定 NodeId，不能继续依赖地址并增加补丁式 fallback。

### CheckedProgram typed HIR

`CheckedProgram` 是后端入口，也是 sema 的完成态：

- 每个可求值 HIR 表达式有最终 `Ty`；`hir/value_categories.rs` 固化 `Place` 与 Value 子类，当前明确区分 `InlineValue`、`OwnedTemporary`、`BorrowedValue` 与仍需后续 effect/ownership 事实继续收窄的 `General`；Identity conversion 必须完整继承 inner category；
- `hir/ownership_capabilities.rs` 独立固化当前可证明的 initial capability：`InlineValue → None`、`OwnedTemporary → Available`、`BorrowedValue → None`；Place / General 在当前迁移阶段没有可伪造的 capability fact，codegen 不得把缺失 fact 当 fallback；
- 显式 Binding 由 `hir/storage_relations.rs` 固化当前可证明的 slot relation：InlineValue / OwnedTemporary、已解析 owning-slot `ReadPlace` 与 `Owned` call result 为 `Owning`，显式 BorrowedValue local 及 `Borrowed` call result 为 `Borrowed`；尚未纳入 effect/loan 的读取上下文不得提前猜 relation；
- `hir/place_relation.rs` 是 resolved Place 三态关系 `Disjoint / Overlap / Unknown` 的唯一 owner；不同 Local root、字段 divergence、常量 index divergence 等证明只在这里完成，动态 index 无充分事实时保持 `Unknown`，final gate 同时验证自反与 ancestor overlap 不变量；
- `hir/ownership_flow.rs` 以显式 CFG/worklist 统一验证 dynamic local 的程序点 capability、local borrow 与 closure capture loan；branch/loop join 对 may-be-moved / may-be-exposed fail-closed；reaching-definition + backward liveness 推导每个 loan generation 的 NLL region，并按实际 referent use 固化 ReadLoan/WriteLoan；命名 closure binding 与立即调用临时值都是显式 loan holder，嵌套 closure holder dependency 继续保持被捕获 closure 的内层 loans；loan conflict 与 Move replacement 都消费 canonical Place relation，只有 `Disjoint` 授权独立；分析不得把用户 HIR nesting 映射到宿主递归；
- `hir/capture.rs` 以 child-before-parent 的有限遍历分配并固化 capture loan；捕获 dynamic Place 进入用户调用时由 parameter argument pass 建立 call loan；capture 不是当前 `Borrowed(source)` 支持的 return source，非 fresh-owner capture return 继续 fail-closed；
- `hir/expr_places.rs` 是已解析 HIR expression 恢复 stable `Place` projection 的唯一 owner；parameter argument planning 与 ownership flow 必须共同消费它，不能分别按 expression 形状拼第二套 Field/Index source；
- `hir/parameter_effects.rs` 在完整 HIR 上以有限格 fixed-point 统一固化 parameter `ReadBorrow / WriteBorrow / Owned` 与 return `Inline / Owned / Borrowed(source)` effects；完整 `Ty::Func`、显式 `Param`、用户方法 target、每个 caller `ArgumentPass` / `CallResult` 与 return `ReturnPass` 必须一致。稳定 dynamic owning Place 只可进入 borrow parameter effect，`Owned` 参数必须接收 `OwnedTemporary + Available`；borrowed return source 当前精确到 `Parameter(index) / Self / Global(binding)`，caller 建立 returned loan。call/returned loan 与 local/capture loan 共用 ownership CFG/NLL，final gate 独立复算并拒绝签名、source、source writability、loan kind 或 pass 漂移；
- `hir/ownership_operations.rs` 在 function effect 写回后统一固化 assignment destination-side write：owning Place 使用 `Replace(InlineCopy|OwnershipTransfer)`，直接 borrowed-alias 赋值使用 `RebindBorrowedAlias`。ownership CFG、final gate 与 codegen 必须消费该 operation；effect fixed-point 尚未完成时的 provisional CFG 也只能调用这一 owner 计算草案，不能复制分类矩阵。Binding 初始化与容器写入仍等待各自完整合同；
- `hir/pattern_bindings.rs` 在 function effect 写回后为 match arm binding 固化 `InlineCopy / DeepClone(plan) / OwnershipTransfer`。普通整值 binding 可 transfer fresh owned subject，稳定 Place / BorrowedValue 必须 clone；constructor payload 属于仍 live 的 result storage，只能 inline copy 或 clone，禁止 partial move。ownership CFG 的 provisional 构图只按 canonical binding type 建立 dynamic local，final gate 与 codegen 必须消费并复核冻结 operation；
- 显式 `clone` 固化为 `CallTarget::Builtin(BuiltinCall::DeepClone(DeepClonePlan))`；dynamic ownership-bearing clone 是 `OwnedTemporary + Available`，标量 clone 保持 `InlineValue + None`；
- 当前已知 owning target 从稳定 Place 普通读取时固化为 `Expr::ReadPlace { source: Place, plan: DeepClonePlan }`；覆盖非 func binding 初始化、local/field replacement、struct 字段默认值/构造实参、array 字面量元素/`push` 实参与 result payload；`for` 循环变量另在 `Stmt::For` 固化 element `DeepClonePlan`，防止 loop binding 与仍 live 的容器元素共享 ownership root。用户函数实参和方法 receiver/实参走 resolved parameter pass，函数返回走 resolved return pass，Pattern 走 resolved binding operation；for iterable source 仍等待自身 effect；capture 不走 owning read，而由结构化 capture loan 管理；
- 显式 `shallow` 固化为 `CallTarget::Builtin(BuiltinCall::ShallowClone(ShallowClonePlan))`；`Inline` 只允许作为递归 safe leaf，合法 user-level shallow root 必须是 aggregate，因此一律为 `OwnedTemporary + Available`；
- 显式 `borrow(place)` 固化为专用 `Expr::Borrow { loan_id, source: Place, kind }`；普通 borrow binding 当前只允许 current-function owning local 所根植的 Place，结果为 `BorrowedValue + None` 并进入 local `Borrowed` slot；return 语境额外允许直接 parameter/self/global source，并由 `ReturnPass` / caller `CallResult` 保持 provenance。`hir/borrow_contract.rs` 阻止 stored/top-level/global borrow binding、borrowed alias capture/generation return、显式 BorrowedValue call forwarding 与 reborrow；
- 显式 `move(place)` 固化为专用 `Expr::Move { source: Place }`；当前 dynamic source 接受同一函数内已证明 owning 的完整 local，包含 `Owned` parameter；field/array partial move-out 以及缺少 capture/global transfer source 的 move 均 fail-closed；scalar move 保持 InlineValue 值语义；
- Binding/Method/字段/构造器索引均已结构化解析；
- 当前赋值统一固化为递归 resolved `Place` 与 resolved `AssignmentOperation`；`Local` / `Field(base Place)` / `Index(base Place, index fact)` projection、target `Ty`/span、root BindingId、字段索引/可写合同和 replacement/rebind ownership operation 都必须在 final gate 中闭合验证；直接 terminal Index assignment 仍未开放并在 HIR gate fail-closed；
- 调用使用 `CallTarget` / `MethodTarget`；
- contextual conversion 使用显式 resolved HIR 节点；
- `typeof` 已固化静态类型名，不允许 codegen 再生成语言类型拼写；
- value category、当前可证明的 initial capability / storage relation，以及每项 capture 的 BindingId/LoanId/root Place/final ReadLoan|WriteLoan 都在最终 HIR validation 前写回；
- `docs/plan.md` 范围内的 ownership / borrow / pointer 操作一旦进入当前 HIR，就必须在进入 codegen 前固化为足以直接发射的 resolved HIR / typed facts。

`hir::validate_resolved_hir` 是 fail-closed 权威门。它使用显式栈/CFG worklist 而非宿主递归，避免验证器重新引入深度风险。任何 Unknown、缺失 ID、非法 target，或对**当前已经落地**的 value category / initial capability / storage relation / Assignment operation / Place relation / Move/loan flow / Borrow kind / capture/parameter/return effect、caller argument/result/return pass、borrow source writability、`ReadPlace`/显式 clone 的 DeepClonePlan / ShallowClonePlan 的缺失、漂移都必须在进入 codegen 前失败；未来 free 等操作一旦进入 HIR，也不得以当前迁移阶段的 General/缺失 fact 作为 fallback 绕过其完整性门禁。

### codegen

codegen 只消费已解析 HIR，不得根据：

- AST 名字；
- builtin 字符串；
- expected type 猜测；
- 函数体形状；
- 诊断文本；
- fallback/default I64；
- pointer bit pattern；
- address 是否等于 descriptor base；

重新决定静态语义、ownership 或 borrow relation。

`codegen/emit/cells.rs` 统一物化 local/capture/global binding cell 的实际 machine address，并根据 resolved `StorageRelation` 区分 owning value cell 与保存 referent address 的 borrowed alias cell；`codegen/emit/places.rs` 再把 resolved Local/Field/Index Place 递归映射到 canonical semantic storage address。Field 投影复用 canonical struct field layout owner，Index 投影复用 checked array element address owner；replacement、borrow 与后续 refer 都必须复用这条地址链，禁止重新拼 capture/global/field/index 地址规则。

Assignment 发射必须直接消费 resolved operation。后端不得再把 `BorrowedValue + Local`、slot relation 或机器地址组合成一条平行的 replacement-vs-rebind 判定路径；当前尚未落地的 destruction 只意味着 replacement 暂未释放旧资源，不授权丢失已经固化的 transfer responsibility。

`codegen/emit/clone.rs` 同时执行显式 clone 与 owning-slot `ReadPlace` 的 resolved plan；`shallow.rs` 执行 resolved shallow plan。两者可以验证 plan 与既有物理布局的内部不变量，但不能自行判断静态类型是否允许对应操作。尤其 shallow-safe aggregate 当前即使以 heap pointer 表示，也必须建立新的独立 aggregate root；禁止简单 bit-copy pointer 后让两个语义 owner 指向同一 root。

若 codegen 需要新增语言层判断，优先判断 HIR 是否缺少 resolved payload，而不是把 sema predicate 复制到后端或放进共享 helper 让两层共同决定。

## 5. ABI、物理布局与 runtime 契约

### 值 ABI

`src/codegen/abi.rs` 是当前值 ABI owner；执行 `docs/plan.md` 时，值 ABI 的 canonical owner 仍必须保持唯一，但当前抽象允许重构或按清晰责任拆分。

当前实现事实：

- `Ty → VTy` 只经 `project_ty(&CheckedProgram)` 一次性投影；
- `ValueAbi` 已显式区分 scalar 与 multi-lane expression、按 `(machine type, byte offset)` 描述的 scalar/aggregate storage lanes、`Direct / IndirectByValue` parameter 和 `Direct / ExplicitSRet` return；当前已接入的语言类型仍全部投影为 scalar 形态，aggregate caller/callee lowering 尚未开放并 fail-closed；
- `PtrLayout` 已冻结当前 Windows x64 capability 的 `provenance / address / view_start / view_end` 四个 I64 lane、`0/8/16/24` offset 与 `size=32 / align=8 / stride=32`；编译入口会把它与目标 ISA 的 I64 machine pointer 核对。`Compiler::machine_ptr_ty` 只表示单个原生地址，不能当作 Alias pointer value ABI；pointer expression/type projection 仍未开放；
- `emit/value.rs::ExprValue` 是实际 Cranelift expression result 的唯一 lane carrier；它按 canonical storage lane offset 统一执行 local/global/temporary cell、resolved Place 与 struct field 的完整 load/store，窄 scalar 仍只在该边界规范化。scalar-only operator/call/container 路径必须显式提取唯一 lane，遇到 aggregate fail-closed。三元与 match 的 CFG merge 已按 resolved `ValueAbi::expression_types()` 传递全部 lane，不再借 universal I64 word 合并表达式；pointer VTy 与具体 pointer expression、typed array/result backing 以及 aggregate caller/callee lowering 尚未接入；
- 窄整数在表达式寄存器中规范化为 I64，但存储、参数和返回槽仍使用声明宽度；
- `Borrowed` return 使用独立的 I64 referent-address lane；caller/callee signature 由同一 `Ty::Func → VTy::Func` 投影决定，不能按声明标量宽度截断地址；
- `storage_word` / `restore_word` 当前承担一-word 容器与具体值表示之间的边界；
- `ValueLayout { size, align, stride }` 是当前所有 value storage layout 查询的唯一入口；local/global/temporary cell、resolved Place 与 struct field layout/load/store 共同消费它及同一 `StorageLane` offset 合同，container backing 仍待后续 typed-stride 重构；
- f32 在当前 word 容器中保存 I32 bit pattern 后扩展到 I64，不能按数值转换处理；
- `unit` 与 `Unknown` 没有值 ABI，到达需要值 ABI 的位置属于内部不变式失败；
- 结构体布局必须统一处理字段对齐与最终尾部 padding。

以上当前语言类型仍使用 scalar / universal one-word 的结构是**当前实现事实，不是长期设计合同**。`docs/plan.md` 已冻结 aggregate-capable pointer ABI 与 typed aggregate/container layout；实施时必须沿现有 aggregate-capable `ValueAbi` 继续接入 pointer 与 container，不能把 `ptr<T>` 压成 I64 handle、把 array/result 继续强制塞回 universal 8-byte payload，或建立第二套临时 ABI。

当前已实现用户函数机器前缀是 `[globals, closure_env, ...]`。`docs/plan.md` 已冻结需要 sret 时的目标内部 ABI 前缀 `[sret?, globals, closure_env, ...]`。迁移必须由统一 signature/ABI owner 一次性决定 caller/callee 两侧，不能在调用点和函数生成器各复制一套隐藏参数规则，也不能为了兼容开发期旧 ABI 保留双路径。

### heap object layout

`src/codegen/layout.rs` 是跨 emitter/runtime 的 heap object 物理布局 owner。目前 closure、raw array、array wrapper、iterator、result 与 string block 的 offset/size 都必须引用这里的命名常量。

禁止在 emitter、native runtime、display、IO 等文件重新写裸 `0/8/16/...` 来表达同一对象字段。历史上曾出现因 8-byte 分配与 16-byte 写入不一致导致的真实内存破坏；因此布局重复不是样式问题，而是正确性风险。

执行 `docs/plan.md` 时，现有固定-word array/result 等布局若与 typed `size/align/stride` 合同冲突，应重构其 canonical layout owner；不得通过在新 pointer 路径里复制裸 offset 来绕过旧布局限制。`StorageDescriptor`、pointer capability layout、raw initialization metadata 等新增物理合同也必须各有明确窄 owner，不能散落 magic offsets。

### runtime machine contracts

`src/codegen/runtime.rs::RUNTIME_CONTRACTS` 是所有 `alias.*` / `rt.*` 符号的机器签名与 nullable 元数据 owner。

- runtime 调用点必须验证参数数量和 Cranelift 机器类型；
- value-vs-unit 调用必须匹配 contract；
- native runtime 定义集合必须与 contract 表精确一致；
- 不得为了方便从调用点反向推导/复制 runtime signature；
- 新增 pointer/provenance/raw-init runtime helper 时仍必须先在 canonical contract owner 中定义机器合同，再由 emitter 与 native runtime 消费。

## 6. Cranelift 控制流与 fail-closed 规则

每个 Cranelift 函数在 `define_function` 前必须无条件执行 verifier。禁止改用可能因 flags 跳过的验证路径。

`Frame::terminated` 表示当前 Cranelift insertion point 已由 return/jump/trap 等终止。后续若源码结构要求继续建立不可达块，必须显式创建/seal 新 block，再清除此状态；不能向已终止 block 继续插指令。

for/iterator 发射必须保持 iterator fail-fast 版本检查。游标在进入循环 body 前推进，是为了让 `continue` 仍然前进；把增量放到 body 尾部会让 continue 跳过推进并破坏循环语义。

进程终止 runtime 调用之后仍保留 trap 作为控制流终结保证；不得假设外部函数永不返回来替代 IR terminator。

## 7. 内存与资源生命周期

当前原生 runtime 使用 Windows process heap，分配路径依赖 zero-initialized memory。`HEAP_ZERO_MEMORY` 的意义是当前对象头、cell/env 等未显式写入的 word 初始为零；在相关旧布局仍存在期间，不能改成普通 HeapAlloc 后继续假设 null/0 初值。

当前没有 HeapFree/GC/ARC，属于**当前实现事实**，不是目标模型。Alias 的目标 ownership、borrow、destruction、raw allocation、`malloc/free` 与 pointer 生命周期已经由 `docs/plan.md` 冻结。实现该计划时应直接把当前生命周期实现重构到该合同，不得擅自引入 GC、ARC、arena、“临时兼容释放层”或与计划竞争的第二套所有权机制。

当前 zero-init、heap block、closure env 等实现细节若在计划执行中被正式替换，只保留新设计实际需要的约束；不要为了开发期旧对象布局制造兼容层。相反，只要某条当前路径仍依赖 zero-init，就必须在其 canonical owner 被完整替换前继续满足该不变量。

临时 object/exe 等编译器侧资源应由窄 RAII owner 管理；失败路径必须先关闭句柄再删除 Windows 文件。不要为不存在的恢复模型增加 checkpoint、shadow artifact 或冗余状态。

## 8. 并发与原子序列号

用于临时文件/run 名字唯一化的原子计数只提供进程内唯一序列，不承担同步或 happens-before 语义，因此使用 `Ordering::Relaxed`。若用途改变为跨线程状态发布，必须重新评估 ordering，不能沿用该注释作为泛化理由。

## 9. 输入健壮性

输入上限的当前值由 `src/limits.rs` 和相关 lexer/parser owner 实现，语言可观察要求见 `docs/spec-notes.md`。

实现要求：

- 对用户输入的深度/规模必须有显式上限；
- HIR value-category / initial-capability / storage-relation / Place-relation / capture / validation 等遍历避免对不可信嵌套使用宿主递归；
- 公开 build/run 管线在显式配置的编译器工作线程栈上执行仍含有的有界递归下降，不能依赖调用者线程栈承载合法输入；
- 用户输入超限必须产生 `AliasError`，不能 panic；
- internal invariant panic 只用于 sema 成功后理论上不可达的编译器内部状态。

`docs/plan.md` 新增的数据流、Place overlap、loan、effect、raw-init validation 等分析若处理用户可控嵌套/规模，同样必须服从已有输入健壮性政策；不能因为它们是“新分析”就重新引入无界宿主递归或用户输入触发 panic。

## 10. 模块依赖与可见性

- 子模块显式 import 实际 owner；生产代码禁止依赖 `use super::*` 形成隐式 dependency barrel；
- 不因拆文件而把父模块私有状态批量升级成 `pub(crate)`；仅暴露真实跨模块接口；
- 无消费者字段、缓存、签名表或未来占位状态应删除，而不是为了“可能以后用”保留；
- 不通过 accidental transitive import、wildcard re-export 或巨型 facade 获得依赖；
- 相似遍历不等于同一职责。HIR visit/validate/capture/value-category/initial-capability/storage-relation/Place-relation，以及后续 loan/effect 分析若具有不同状态与失败语义，不为机械 DRY 建立万能 Visitor。

## 11. 注释标准

不追求注释百分比。以下 correctness-sensitive 位置必须有本地 `why / what breaks` 说明：

- 编译器 phase 顺序与 AST/HIR identity 假设；
- Cranelift block sealing / terminated 状态；
- ABI register/storage/aggregate/sret 变换；
- pointer capability / StorageDescriptor / typed aggregate 物理布局；
- ownership transfer、clone/shallow root、borrow loan 与 destruction responsibility；
- runtime nullable/fail-closed 行为；
- memory ordering；
- 手写数值格式化等非直观算法。

能用命名常量、窄类型、结构化 target 或更清晰控制流消除 magic value 时，优先改代码结构，再补必要注释。禁止把历史 Phase、旧迁移编号和已删除实现留在当前生产代码注释中。

## 12. 测试责任

测试按**行为 contract**分层，不按数量评价质量。

- `*_laws.rs`：各语言子系统的静态/动态法律；显式 deep/shallow copy、owning-slot 普通读取、move、local borrow/NLL、closure capture loan、function parameter effects 与 function return effects 的 native/静态行为分别由 `clone_laws.rs` / `shallow_laws.rs` / `ordinary_read_laws.rs` / `move_laws.rs` / `borrow_laws.rs` / `capture_laws.rs` / `parameter_effect_laws.rs` / `return_effect_laws.rs` 覆盖；
- `golden.rs`：需要冻结 stdout/stderr/exit 或诊断字节的代表性黄金行为；
- `smoke.rs`：少量跨层主链烟雾检查，不复制完整法律；
- `demo_corpus.rs`：机械枚举所有 `demos/*.as` 并冻结每个 demo 的三元组；
- `native_pipeline.rs`：真实 object/link/独立进程边界；
- `destructive_codegen.rs`：重复运行/内存破坏等破坏性回归；
- `security_regressions.rs`：输入边界与安全回归。

同一个行为 contract 不应在 smoke/golden/demo corpus 中再建多个近似副本。真实 native 生命周期、destructive/security 边界不能为了减少测试数被机械删除。

`docs/plan.md` 的 ownership / borrow / pointer ABI 属于 correctness contract。验证必须覆盖静态拒绝规则与真实 native object/link/独立进程路径，尤其 aggregate ABI、sret、typed layout、provenance/bounds 与 destruction；不能只靠 HIR 快照或 isolated helper test 宣称完成。

测试文件和注释只使用当前职责名称；`parity`、迁移 P0/P1/P3 等历史脚手架不得保留在当前工程中。

## 13. 文档责任边界

- `docs/spec-notes.md`：当前语言规范，唯一当前用户可观察语义 owner；
- `docs/plan.md`：已冻结的内存、ownership、borrow、destruction、raw allocation 与 pointer 目标设计/实现合同；它可以在实现完成前领先于当前语言实现，但不能被当作“当前已经支持”的证明；
- `AGENTS.md`：当前工程结构、owner、架构边界、维护规则，以及当前实现与已冻结目标之间必须遵守的迁移边界；
- 其它 topic docs：只拥有其明确专题，纯历史说明应删除，不能与当前规范或活跃设计合同竞争；
- `GENERAL_ENGINEERING_RULES.md`：跨项目工程规则；
- `NO_CI.md`：Alias CI 永久禁用硬规则。

实现 `docs/plan.md` 时，用户可观察语义一旦实际改变，必须在同一任务中同步 `docs/spec-notes.md`；工程 owner/边界变化同步本文件。不要为了“同步”把完整语言规则复制进 `AGENTS.md`，也不要让 `spec-notes` 提前声称尚未落地的行为已经存在。

## 14. 开发与验证硬规则

Alias 是正式、长期维护项目，不以 MVP/Demo/PoC 标准降级实现。

预发布开发历史不是兼容义务：新设计正式替换旧设计后，删除旧 AST、旧分支、fallback、桥接层、兼容别名和历史测试脚手架。只保留一个当前正确形态。

执行 `docs/plan.md` 时同样适用：旧 single-scalar `ValueAbi`、universal word、旧隐藏参数约定、旧 shared-reference 生命周期等一旦被新设计正式替换，就应删除对应旧路径；不得保留“legacy pointer ABI”“compat word path”“temporary ownership fallback”等并行体系。

CI 永久禁用。不得新增、恢复或建议 GitHub Actions 或其它 CI。`.cargo/config.toml` 对整个 Alias crate 强制启用 `-D warnings`；所有 target 必须保持零 warning，不得用全局 `allow` 降级门禁。需要验证时只使用显式手动命令：

```bash
cargo check
cargo build
cargo test --all-targets
cargo clippy --all-targets
```

执行结构性重构后必须继续搜索同类问题到 fixed point：重复 owner、裸布局 offset、历史阶段注释、wildcard dependency、无消费者状态、后端静态语义判断、残留 universal-word 假设、caller/callee ABI 双源等不能只修首个样本。
