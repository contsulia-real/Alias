# Alias 工程知识库

**同步日期：** 2026-08-28  
**基线分支：** `main`  
**当前语言规范：** `docs/spec-notes.md`

> 本文件只拥有 **当前工程结构、架构边界、代码维护不变式与验证方式**。  
> 语言语法、类型规则、Pattern、转换、显示等用户可观察语义只由 `docs/spec-notes.md` 定义。不得在本文件复制第二份语言规范，也不得保留会与当前状态竞争的历史规范。

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

开始实现、重构或审计前，按以下顺序确认当前事实：

1. 当前仓库源码；
2. `docs/spec-notes.md` 当前语言规范；
3. 本文件中的工程边界；
4. `GENERAL_ENGINEERING_RULES.md`；
5. `NO_CI.md`。

聊天历史、旧 commit、旧 Phase 编号和迁移期间的中间设计都不能替代当前源码与当前规范。

缺失产品、语言、架构或兼容性裁决时，必须明确标记为未知/待裁决，不能自行补全。当前仍未冻结的长期决策（例如最终内存生命周期模型、失败 build 对旧成功 exe 的保留策略）不得从现状反推为永久设计。

## 3. 当前源码结构与 owner

```text
src/
├── main.rs                 # CLI 参数与进程退出映射
├── lib.rs                  # run/build 编排、编译器工作线程栈；AliasError / Span
├── target.rs               # 编译器内部目标 triple owner
├── limits.rs               # 当前共享输入/表达式限制 owner
├── builtins.rs             # 预定义语言名字分类 owner
├── lexer.rs                # token 化、字符串插值拆分
├── ast.rs                  # parser AST，仅语法
├── parser/                 # 语法解析
├── sema/
│   ├── mod.rs              # check(Program) -> CheckedProgram
│   ├── decls.rs / stmts.rs
│   ├── exprs.rs + exprs/   # 表达式静态语义、调用/方法解析、目标类型传播
│   ├── types.rs            # Ty 与类型槽检查
│   └── hir.rs + hir/       # typed HIR、lower、capture、validate、visit
├── codegen/
│   ├── mod.rs              # compile_to_object(CheckedProgram)
│   ├── abi.rs              # Ty→VTy、ValueAbi、结构体布局、word 编码
│   ├── layout.rs           # runtime heap object 物理布局 owner
│   ├── emit.rs + emit/     # HIR → Cranelift 发射
│   ├── funcgen.rs          # 用户函数/闭包生成
│   ├── runtime.rs          # RUNTIME_CONTRACTS 与 runtime 调用校验 owner
│   └── native_runtime.rs + native_runtime/ # 产物内 runtime 实现
└── linker.rs               # COFF → exe 与 Windows SDK/rust-lld 定位 owner
```

禁止创建语义不清的 `utils` / `helpers` / `common` 汇总模块。共享逻辑只能放到拥有该概念的窄 owner 中。

## 4. parser → sema → HIR → codegen 边界

### parser AST

`src/ast.rs` 只表达源码语法，不拥有最终静态类型、BindingId、MethodId、字段索引或最终调用目标。

parser 可以查询 `builtins.rs` 中**明确属于语法分类**的信息，但不得自行复制 builtin 字符串名单，也不得在多个 parser 文件维护平行分类。

### sema

sema 是语言静态语义的 owner。名字解析、目标类型传播、转换关系、调用/方法归属、Pattern coverage、字段/构造器索引等必须在这里完成。

检查阶段使用 AST 节点地址作为短生命周期 fact key。该 identity **只在同一次 check → lower 调用链内有效**；两阶段之间禁止移动、clone 后替换或重建 AST 节点。若未来引入 AST 重写，必须先改用稳定 NodeId，不能继续依赖地址并增加补丁式 fallback。

### CheckedProgram typed HIR

`CheckedProgram` 是后端入口，也是 sema 的完成态：

- 每个可求值 HIR 表达式有最终 `Ty`；
- Binding/Method/字段/构造器索引均已结构化解析；
- 调用使用 `CallTarget` / `MethodTarget`；
- contextual conversion 使用显式 resolved HIR 节点；
- `typeof` 已固化静态类型名，不允许 codegen 再生成语言类型拼写；
- capture 列表在最终 HIR validation 之前完成写回。

`hir::validate_resolved_hir` 是 fail-closed 权威门。它使用显式栈而非宿主递归，避免验证器重新引入深度风险。任何 Unknown、缺失 ID、非法 target 或未完成 fact 都必须在进入 codegen 前失败。

### codegen

codegen 只消费已解析 HIR，不得根据：

- AST 名字；
- builtin 字符串；
- expected type 猜测；
- 函数体形状；
- 诊断文本；
- fallback/default I64；

重新决定静态语义。

若 codegen 需要新增语言层判断，优先判断 HIR 是否缺少 resolved payload，而不是把 sema predicate 复制到后端或放进共享 helper 让两层共同决定。

## 5. ABI、物理布局与 runtime 契约

### 值 ABI

`src/codegen/abi.rs` 是值 ABI owner：

- `Ty → VTy` 只经 `project_ty(&CheckedProgram)` 一次性投影；
- register/storage/param/ret 的物理宽度由 `ValueAbi` 决定；
- 窄整数在表达式寄存器中规范化为 I64，但存储、参数和返回槽仍使用声明宽度；
- `storage_word` / `restore_word` 是一 word 容器与具体值表示之间的边界；
- f32 在 word 容器中保存 I32 bit pattern 后扩展到 I64，不能按数值转换处理；
- `unit` 与 `Unknown` 没有值 ABI，到达需要值 ABI 的位置属于内部不变式失败；
- 结构体布局必须统一处理字段对齐与最终尾部 padding。

`user_signature` 的前两个机器参数是隐藏的 globals 与 closure env。调用方和被调方不得各自复制另一套隐藏参数约定。

### heap object layout

`src/codegen/layout.rs` 是跨 emitter/runtime 的 heap object 物理布局 owner。目前 closure、raw array、array wrapper、iterator、result 与 string block 的 offset/size 都必须引用这里的命名常量。

禁止在 emitter、native runtime、display、IO 等文件重新写裸 `0/8/16/...` 来表达同一对象字段。历史上曾出现因 8-byte 分配与 16-byte 写入不一致导致的真实内存破坏；因此布局重复不是样式问题，而是正确性风险。

### runtime machine contracts

`src/codegen/runtime.rs::RUNTIME_CONTRACTS` 是所有 `alias.*` / `rt.*` 符号的机器签名与 nullable 元数据 owner。

- runtime 调用点必须验证参数数量和 Cranelift 机器类型；
- value-vs-unit 调用必须匹配 contract；
- native runtime 定义集合必须与 contract 表精确一致；
- 不得为了方便从调用点反向推导/复制 runtime signature。

## 6. Cranelift 控制流与 fail-closed 规则

每个 Cranelift 函数在 `define_function` 前必须无条件执行 verifier。禁止改用可能因 flags 跳过的验证路径。

`Frame::terminated` 表示当前 Cranelift insertion point 已由 return/jump/trap 等终止。后续若源码结构要求继续建立不可达块，必须显式创建/seal 新 block，再清除此状态；不能向已终止 block 继续插指令。

for/iterator 发射必须保持 iterator fail-fast 版本检查。游标在进入循环 body 前推进，是为了让 `continue` 仍然前进；把增量放到 body 尾部会让 continue 跳过推进并破坏循环语义。

进程终止 runtime 调用之后仍保留 trap 作为控制流终结保证；不得假设外部函数永不返回来替代 IR terminator。

## 7. 内存与资源生命周期

当前原生 runtime 使用 Windows process heap，分配路径依赖 zero-initialized memory。`HEAP_ZERO_MEMORY` 的意义是对象头、cell/env 等未显式写入的 word 初始为零；不能改成普通 HeapAlloc 后继续假设 null/0 初值。

当前没有 HeapFree/GC/ARC，属于**当前实现事实**，不是冻结的长期内存模型。没有明确 workload/产品裁决前，不得擅自引入 GC、ARC、arena 或“临时兼容释放层”。

临时 object/exe 等编译器侧资源应由窄 RAII owner 管理；失败路径必须先关闭句柄再删除 Windows 文件。不要为不存在的恢复模型增加 checkpoint、shadow artifact 或冗余状态。

## 8. 并发与原子序列号

用于临时文件/run 名字唯一化的原子计数只提供进程内唯一序列，不承担同步或 happens-before 语义，因此使用 `Ordering::Relaxed`。若用途改变为跨线程状态发布，必须重新评估 ordering，不能沿用该注释作为泛化理由。

## 9. 输入健壮性

输入上限的当前值由 `src/limits.rs` 和相关 lexer/parser owner 实现，语言可观察要求见 `docs/spec-notes.md`。

实现要求：

- 对用户输入的深度/规模必须有显式上限；
- HIR capture/validation 等遍历避免对不可信嵌套使用宿主递归；
- 公开 build/run 管线在显式配置的编译器工作线程栈上执行仍含有的有界递归下降，不能依赖调用者线程栈承载合法输入；
- 用户输入超限必须产生 `AliasError`，不能 panic；
- internal invariant panic 只用于 sema 成功后理论上不可达的编译器内部状态。

## 10. 模块依赖与可见性

- 子模块显式 import 实际 owner；生产代码禁止依赖 `use super::*` 形成隐式 dependency barrel；
- 不因拆文件而把父模块私有状态批量升级成 `pub(crate)`；仅暴露真实跨模块接口；
- 无消费者字段、缓存、签名表或未来占位状态应删除，而不是为了“可能以后用”保留；
- 不通过 accidental transitive import、wildcard re-export 或巨型 facade 获得依赖；
- 相似遍历不等于同一职责。HIR visit/validate/capture 具有不同状态与失败语义，不为机械 DRY 建立万能 Visitor。

## 11. 注释标准

不追求注释百分比。以下 correctness-sensitive 位置必须有本地 `why / what breaks` 说明：

- 编译器 phase 顺序与 AST/HIR identity 假设；
- Cranelift block sealing / terminated 状态；
- ABI register/storage/word 变换；
- heap object 物理布局；
- runtime nullable/fail-closed 行为；
- memory ordering；
- 手写数值格式化等非直观算法。

能用命名常量、窄类型、结构化 target 或更清晰控制流消除 magic value 时，优先改代码结构，再补必要注释。禁止把历史 Phase、旧迁移编号和已删除实现留在当前生产代码注释中。

## 12. 测试责任

测试按**行为 contract**分层，不按数量评价质量。

- `*_laws.rs`：各语言子系统的静态/动态法律；
- `golden.rs`：需要冻结 stdout/stderr/exit 或诊断字节的代表性黄金行为；
- `smoke.rs`：少量跨层主链烟雾检查，不复制完整法律；
- `demo_corpus.rs`：机械枚举所有 `demos/*.as` 并冻结每个 demo 的三元组；
- `native_pipeline.rs`：真实 object/link/独立进程边界；
- `destructive_codegen.rs`：重复运行/内存破坏等破坏性回归；
- `security_regressions.rs`：输入边界与安全回归。

同一个行为 contract 不应在 smoke/golden/demo corpus 中再建多个近似副本。真实 native 生命周期、destructive/security 边界不能为了减少测试数被机械删除。

测试文件和注释只使用当前职责名称；`parity`、迁移 P0/P1/P3 等历史脚手架不得保留在当前工程中。

## 13. 文档责任边界

- `docs/spec-notes.md`：当前语言规范，唯一用户可观察语义 owner；
- `AGENTS.md`：当前工程结构、owner、架构边界、维护规则；
- topic docs：只拥有其明确专题，纯历史说明应删除，不能与当前规范并存；
- `GENERAL_ENGINEERING_RULES.md`：跨项目工程规则；
- `NO_CI.md`：Alias CI 永久禁用硬规则。

当前语义变化必须更新 `docs/spec-notes.md`；工程 owner/边界变化才更新本文件。不要为了“同步”把完整语言规则复制回来。

## 14. 开发与验证硬规则

Alias 是正式、长期维护项目，不以 MVP/Demo/PoC 标准降级实现。

预发布开发历史不是兼容义务：新设计正式替换旧设计后，删除旧 AST、旧分支、fallback、桥接层、兼容别名和历史测试脚手架。只保留一个当前正确形态。

CI 永久禁用。不得新增、恢复或建议 GitHub Actions 或其它 CI。`.cargo/config.toml` 对整个 Alias crate 强制启用 `-D warnings`；所有 target 必须保持零 warning，不得用全局 `allow` 降级门禁。需要验证时只使用显式手动命令：

```bash
cargo check
cargo build
cargo test --all-targets
cargo clippy --all-targets
```

执行结构性重构后必须继续搜索同类问题到 fixed point：重复 owner、裸布局 offset、历史阶段注释、wildcard dependency、无消费者状态、后端静态语义判断等不能只修首个样本。
