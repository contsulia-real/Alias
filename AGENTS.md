# PROJECT KNOWLEDGE BASE

**Generated:** 2026-08-27
**Git:** `main` 分支

## OVERVIEW

Alias——用 Rust 实现的自研**编译型语言**（`.as` 源程序）。唯一执行形态是 Cranelift 生成 COFF、rust-lld 链接独立 `.exe`（纯 kernel32 依赖，无 CRT）。`run` 先完整编译临时 exe 再启动，`build` 输出持久 exe；不存在解释执行或进程内机器码执行路径。管线：lexer → parser → **sema** → codegen → linker → 原生进程。

## STRUCTURE

```
D:\Project\Alias\
├── src/
│   ├── main.rs      # CLI 壳：run|build 子命令（裸单参 = run）→ 退出码 clamp 到 0–255
│   ├── lib.rs       # 编排器 run()/build() + 统一错误类型 AliasError/Span(file:line:col)
│   ├── lexer.rs     # lex(src) -> Vec<Token>；含字符串插值 StrPart 切分
│   ├── parser/      # parse(tokens) -> Program；含递归/表达式链深度守卫
│   ├── ast.rs       # AST 纯数据定义（Expr/Stmt/BindKind/BinOp/Pattern…）
│   ├── sema/        # 静态语义层 check()：作用域链 + 内部类型推断 + 全量检查
│   ├── codegen/     # 唯一 Cranelift Object 后端：abi/runtime 单源 + 原生 runtime + 发射器
│   └── linker.rs    # rust-lld 子进程封装：SDK 发现 + 链接参数（链接唯一拥有者）
├── demos/*.as       # Alias 语言示例程序；count_to_ten.as 被 smoke.rs include_str! 复用为测试
├── tests/smoke.rs   # Phase 1 集成冒烟测试，直接调库接口 run()
├── tests/golden.rs  # Phase 0 黄金记录：二进制精确三元组 (stdout/stderr/exit)
├── tests/sema_laws.rs # Phase 1 sema 法律：负向矩阵断言精确中文消息+行:列
├── tests/native_parity.rs # Phase 4 编译器黄金基线：demos 机械枚举 + 定向用例
├── tests/struct_laws.rs # Phase 2a struct 法律：正负矩阵断言精确中文消息+行:列
├── tests/result_laws.rs # Phase 2b result/match/? 法律：正负矩阵
├── tests/method_laws.rs # Phase 2c 扩展方法法律：正负矩阵断言精确中文消息+行:列
├── tests/array_laws.rs # Phase 2d array<T> 法律：正负矩阵 + 运行时中止子进程用例
├── tests/pattern_laws.rs # Pattern AST / match 第一批法律：wildcard/binding/literal/result ctor
├── tests/operator_pub_laws.rs # pub / % / 位运算 / 移位法律
├── tests/native_pipeline.rs # run 临时产物 vs build 持久产物三元组逐字节一致
├── tests/security_regressions.rs # 内存安全、资源上限、宿主存活与覆盖保护回归
├── tests/destructive_codegen.rs # 固定种子随机 AST、数值边界、深层闭包、并发原生编译
├── docs/spec-notes.md # 规范冻结：display 表、Q①–Q⑥、运行时契约、唯一原生形态
├── MIGRATION.md     # 迁移记录：每个语义变化一行，引用裁决编号
├── NO_CI.md        # 项目级硬规则：CI 永久禁用
├── Cargo.toml       # [dependencies] = cranelift-* 0.135 (唯一第三方依赖, codegen/ 独占)
└── .cargo/config.toml # windows-msvc 目标用 rust-lld linker
```

## WHERE TO LOOK

| 任务 | 位置 | 备注 |
|------|------|------|
| 加语言特性 | ast.rs → parser.rs → sema.rs → codegen.rs | 按此顺序四处联动，入口链不变 |
| 加内建函数 | `codegen/emit.rs` + `codegen/runtime.rs` + `codegen/native_runtime.rs` | 先在 `RUNTIME_CONTRACTS` 登记签名和可空性；调用点不得手写 runtime 签名 |
| 改值 ABI/布局 | `codegen/abi.rs` | `VTy::abi()`、用户函数签名、存储字转换、结构体字段布局的唯一真相源；其它模块不得复制宽度表 |
| 改错误信息格式 | lib.rs `AliasError::Display` | 运行时错误经 span-ID 中止存根/编译期 AliasError 携带 |
| 加语法 | lexer.rs Tok + parser/ | 词法与语法分离改动 |
| 验证语义 | tests/*_laws.rs + demos/ | 每条用例对应一条语言裁决 |

## CODE MAP

| 符号 | 类型 | 位置 | 角色 |
|------|------|------|------|
| `run` / `build` | fn | lib.rs | 共用唯一编译管线；run 链接临时 exe 后启动，build 输出持久 exe |
| `lex` | fn | lexer.rs | 源码 → Token 流 |
| `parse` | fn | parser/ | Token → Program AST |
| `sema::check` | fn | sema/mod.rs | 静态全量检查 |
| `codegen::compile_to_object` | fn | codegen/mod.rs | 把 Program 编译为含 runtime 与入口的 COFF 字节流 |
| `VTy::abi` / `build_struct_layouts` | fn | codegen/abi.rs | 寄存器、存储、对齐、参数/返回和字段布局单源 |
| `RUNTIME_CONTRACTS` | table | codegen/runtime.rs | runtime 参数、返回值和可空性的唯一机器契约 |
| `emit_expr` / `emit_stmt` | fn | codegen/emit.rs | 表达式/语句发射核心（单元格值模型 + 闭包创建） |
| `methods` / `method_rets` | field | codegen/mod.rs | 方法表：(接收者,方法名)→FuncId 静态分派 + 返回类型投影 |
| `define_span_data` | fn | codegen/native_runtime.rs | 把运行时诊断 span 表固化进原生产物只读数据段 |
| `AliasError`/`Span` | struct | lib.rs | 错误统一携带 file:line:col |

## CONVENTIONS

- **宪法驱动**：已冻结裁决优先于局部实现便利；语义变化必须同步测试/规范。
- **执行模型**：所有合法程序必须先生成 COFF 并经 rust-lld 链接为 exe；`run` 只负责临时产物生命周期和启动，不得增加进程内执行捷径。
- **中文报错**：所有面向用户的错误消息为简体中文。
- **存储/闭包语义**：`alias.cell.new(bytes)` 按声明类型尺寸分配清零存储区；每个绑定一泄漏堆单元格，闭包 env 持捕获单元格指针（引用捕获）。
- **方法语义**：`pub? func <Ret> <RecvType>.<name>` 定义扩展方法；`pub` 只允许顶层；`public` 不再是关键字或兼容别名。self 是隐式 val 绑定；方法名按接收者类型划分命名空间；内建 len/upper/lower/trim 不可覆盖。
- **Pattern 语义**：第一批 Pattern 为 `_`、普通标识符绑定、整数/bool/纯字符串字面量、`ok(name|_)` / `err(name|_)`。普通标识符与 `_` 都是 catch-all，前者绑定整个主语；guard/struct Pattern 暂未加入。
- **运算符语义**：`%` 仅整数；`& | ^ ~ << >>` 仅整数；同型同宽度运算，不允许隐式数值混算；不提供复合赋值。
- **数组语义**：实例 = 泄漏头块 {data_ptr,len,cap}，引用语义（别名共享）；下标只读（arr[i]=x 拒绝），越界/pop 空 → span-ID 中止存根；内建 len/push/pop 不可定义。
- **混型 ABI**：函数类型携带完整参数/返回投影；整数规范在途形为 I64，f32/f64 保持原生浮点寄存器类型，进入 result/array 的 8 字节字槽时才按位装箱。
- **ABI/布局单源**：每种 `VTy` 的规范寄存器类型、实际存储类型/宽度/对齐、参数/返回类型及载荷字编码只在 `codegen/abi.rs` 定义。
- **runtime 契约单源**：所有 `alias.*` 与内部 `rt.*` 符号必须进入 `RUNTIME_CONTRACTS`；缺失、重复、多余或调用参数类型漂移均为编译器错误。
- **运行时错误**：只由编译产物按内嵌 span 表输出中文诊断并 `ExitProcess(1)`；编译器进程不执行语言 runtime。
- **库接口边界**：`run()` 的 `Err` 只承载编译、链接和启动失败；语言运行时中止属于已编译子进程，`run()` 返回其退出码。

## ANTI-PATTERNS (THIS PROJECT)

- **禁止字符串哨兵**：控制流与错误各有类型化通道，不得用特殊字符串/魔法值传递。
- import 只解析不执行——不要尝试解析 import 语义（Phase 5 前）。
- 下标赋值是明确拒绝（只读索引裁决），不是 bug。
- 不为废弃开发状态保留兼容层；被替换的语法应直接退役。

## UNIQUE STYLES

- 语言语法风格：`func i32 main = () -> {...}`、`pub func u32 f = (...) -> ...`、`var i32 x = 1;`。
- 无括号调用 (P2e 泛化): 语句入口裸名吞一个 unary 实参；表达式内 `ident unary` 吞参调用、`expr Ident [unary]` 方法中缀（`a plus b` ≡ `a.plus(b)`）。铁律: `dup 5 + 1` 报错，须 `(dup 5) + 1`; 函数值传参须显式 `f(g)`; 零参调用必须括号 `five()`。
- main 必须零参且只允许返回 i32；其它返回类型在 sema 阶段拒绝。
- 字符串插值用单引号：`'n=$i'`。
- demos/ 即测试夹具：smoke.rs 用 include_str! 直接跑 demo 文件。

## COMMANDS

```bash
cargo run -- demos/count_to_ten.as        # 编译临时 exe 后运行
cargo run -- build demos/hello_native.as  # 编译为独立 exe 并链接
cargo test --all-targets                  # 显式手动全量验证
cargo build                               # 构建
```

## NOTES

- **CI 永久禁用。** 以仓库根目录 `NO_CI.md` 为硬规则；任何工具、代理或后续任务都不得新增、恢复、启用或询问启用 CI/GitHub Actions/其它自动化流水线。验证只能显式手动运行。
- 无 lint 配置、无 Makefile——构建使用 cargo 默认行为。
- 数值类型含 i8/i16/i32/i64、u8/u16/u32/u64、f32/f64；跨类型混算禁止，显式转换走 `to_*` 内建。
- 除零是运行时错误而非 panic（`codegen/emit.rs` 显式守卫 → span-ID 中止存根，INT_MIN÷-1 同守卫）。
- 输入上限：源码 8 MiB、200000 token、语法/类型/插值嵌套 128 层、表达式链 256 项。
