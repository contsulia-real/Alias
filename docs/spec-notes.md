# Alias 规范笔记 — Phase 0 规范冻结

**状态**: 规范性文档 (normative)。本文与 `tests/golden.rs` 共同构成语言行为契约。
**权威链**: `.omo/plans/compiler-migration.md` (用户已裁决 D1–D5 + Q①–Q⑥) → 本文固化 → `tests/golden.rs` 逐字节执行。
**冻结日期**: 2026-08-24。此后任何改动若使黄金记录变红, 必须先修订本文并说明裁决依据。

---

## 一、Value::display 渲染格式表 (规范性)

来源: `src/codegen/emit.rs` 的 display 分派与 `codegen/native_runtime.rs`。字符串插值的洞 (`'n=$i'`) 与
内建 `println`/`print` 的输出一律经过此表, 无第二渲染路径。

| 运行时值 | 静态类型 | 显示字节 | 备注 |
|---|---|---|---|
| 有符号整数 | `i8/i16/i32/i64` | 十进制表示 (如 `48`, `-3`) | `+/-/*/<<`、一元负号、自增减按声明宽度 checked，越界中止 |
| 无符号整数 | `u8/u16/u32/u64` | 无符号十进制表示 | `+/-/*/<<`、自增减按声明宽度 checked，越界中止 |
| 浮点 | `f32/f64` | 规范化十进制科学计数法；零为 `0` | 原生 runtime 统一舍入规则 |
| 布尔 | `Bool(bool)` | `true` / `false` | 小写字面量 |
| 字符串 | `Str(String)` | 原文字节 | **不加引号、不转义**, 与源码字面量形态无关 |
| 函数值 | `Func(Rc<FuncValue>)` | `<func>` | 永不泄露闭包/参数信息 |

`unit` 不在显示表中。它只用于函数签名，表示函数没有返回值；不存在可构造、
绑定、传参、转换、插值或打印的 unit 值，`()` 也不是表达式。

## 二、行为怪癖裁决 Q①–Q⑥ (规范性, 用户已裁决, 实现代理不得重开)

| # | 现状 (位置) | 裁决 | 黄金锚点 |
|---|---|---|---|
| Q① | 历史实现曾允许 `true < false` | **编译错误**: 有序比较仅限同型 i32/i64 与 string; EqEq/NotEq 对 bool 仍合法 | Phase 1 以 `tightened_*` 测试锁定 |
| Q② | 函数参数为隐式 val 绑定 | **保留**运行时语义; 编译期拒绝对参数赋值 | `closure_reference_capture_latest_value` (间接依赖) |
| Q③ | 历史实现允许声明返回非 unit 的函数块体落空 | **编译错误**: 声明返回非 unit 的函数控制流必须可达 return; 仅 unit 可落空 | Phase 1 锁定 |
| Q④ | 历史实现允许 bool/string/unit main | **收紧**: main 必须存在、是零参 func，且唯一合法返回类型为 `i32`; 其它返回类型在 sema 阶段编译错误。进程边界仍把 i32 clamp 到 0–255 | `bool_main_rejected`、`string_main_rejected`、`unit_main_rejected`、`exit_code_clamped_to_255` |
| Q⑤ | 缺 main 使用 `Span::default()` | **修复**: Span 为 default 时 Display 省略位置前缀, 只报「找不到顶层 func main」 | `missing_main_error` |
| Q⑥ | 顶层绑定按序求值、先于 main、可有副作用 | **保留顺序语义**: 生成的入口 wrapper 先按序求值顶层初始化再调用用户 main | `top_level_side_effect_ordering` |

## 三、已冻结的观察事实 (规范性, 黄金记录逐字节背书)

以下事实来自对当前构建的实际探测, 属于可观察契约的一部分:

1. **错误 Display 格式**: `错误 @ {line}:{col} — {msg}` (`src/lib.rs:26-30`)。
   `Span` 当前只保存 `line/col/len`，不保存源文件路径；`run(path,src)` / `build(path,src,out)`
   的 path 尚不进入语言诊断。`Span::default()` 省略位置前缀。
2. **列号 0 起始**: lexer 产出的 col 从 0 计数 (实测 `return 1 / 0` 中 `1` 报
   `2:11`; 行首缩进四格的 `a` 报 `3:4`)。行号 1 起始。
3. **进程边界 clamp**: main 返回值经 `code.clamp(0, 255)` 输出
   (`src/main.rs:22`); 返 300 → 进程退出码 255。
4. **CLI 层退出码**: 无参数 → stderr「用法: alias <source.as>\n」+ 退出码 2;
   文件不可读 → stderr「无法读取 {path}: {os error}\n」+ 退出码 2
   (`src/main.rs:5-19`; 占位符原为 `<script.as>`, M18 术语清理改为 `<source.as>`)。
5. **import 通知**: import 只解析不执行, 向 stderr 打印
   「[alias] 注意: {n} 条 import 已解析但标准库尚未接入 (Phase 5 前)\n」
   (`src/codegen/mod.rs`)。该文本已进黄金记录, Phase 5 接入标准库前不得变动。
6. **count_to_ten 打印 1..10**: demo 循环体先 `increase i` 再 `println i`,
   故 stdout 为 `1\n…\n10\n` — 不是直觉的 0..9。以实际字节为准。
7. **除零是运行时错误**: 「除以零」, span 取除号左侧操作数
   (`src/codegen/emit.rs`), 退出码 FAILURE(1), 非 panic。

## 四、黄金记录索引

`tests/golden.rs` 断言编译产物二进制的精确三元组 (stdout 字节/stderr 字节/退出码):

| 用例名 | 覆盖点 |
|---|---|
| `count_to_ten_demo` | demo 夹具 + import 通知 + 循环顺序 |
| `arithmetic_exit_48` | i32 main 返回值即退出码 |
| `bool_main_rejected` / `string_main_rejected` / `unit_main_rejected` | Q④ main 仅 i32 |
| `string_interpolation_equality` | 插值 `'n=$i'` + 字符串相等 |
| `division_by_zero_error` | 除零消息 + 0 起始列号 |
| `display_func` | display 表 `<func>`；unit 不在值域 |
| `top_level_side_effect_ordering` | Q⑥ 顶层副作用先于 main |
| `closure_reference_capture_latest_value` | 引用捕获读到最新值 |
| `while_false_dead_code` | 死代码落空 |
| `val_reassignment_error` | 「val」消息 + span |
| `missing_type_slot_error` | 「类型槽」消息 + span |
| `missing_main_error` | Q⑤ 现状 `@ 0:0` (待 Phase 1 改写) |
| `exit_code_clamped_to_255` | clamp 行为 |
| `no_args_usage_exit_2` / `missing_file_exit_2` | CLI 层退出码 2 |

## 五、范围边界

- `demos/recursion.as`、`file_wc.as`、`producer_consumer.as`、`helper.as` 是
  **规范文档非测试夹具** — 当前可能无法解析/运行, 禁止纳入黄金记录。
- MethodCall/Field/Index 占位已分别随 Phase 2a/2b/2c/2d 退役为真语义
  (见附录三/四/五/六); import 保持解析暂存 (Phase 5 前)。
- 本文件只冻结行为, 不发明新语义; 未尽事宜以迁移计划为准。

---

# 附录 — Phase 1 Sema 层规范 (2026-08-24 增补)

当前管线为 lex → parse → **sema** → COFF → rust-lld → exe。sema 通过的
程序只允许进入完整原生编译管线；运行时检查由生成代码与原生 runtime 承担。

## 一、Q③ 严格落空规则 (规范性, 2026-08-24 用户终裁)

**规则**: 声明返回类型 ≠ unit 的块体函数, 其块的最后一条语句必须是
`return` 语句; 否则编译错误:
「返回类型为 {ty} 的函数体必须以 return 语句收尾」, span 为函数字面量
(参数表 `(` 起)。箭头体永无落空; unit 函数任意收尾合法。

**循环收尾不豁免** (用户终裁, 推翻早期驱动尾豁免): 循环语句收尾与其它非
return 收尾同等拒绝。count_to_ten 语料已补 `return 0` (MIGRATION.md M3)。
codegen 的块尾落空回退仅对 unit 函数可达。

## 二、func 绑定的类型槽语义 (规范性)

`func T f = ...` 中 `T` 一律表示**函数返回类型** (语言约定, 非 init 值类型):
- 初始化器为函数字面量 → 字面量体内所有 return 按 T 校验, 推断返回类型须等于 T;
- 初始化器为其它表达式 → 其须为函数值, 其返回类型须等于 T;
- 类型槽写 `func` (多态) → 接受任意签名函数值, 经其调用点不做元数/实参检查。

val/var 绑定的类型槽表示初始化值的类型 (D3: 声明↔初始化一致性)。
赋值语句**不**做类型一致性检查 — D3 未列赋值, 不私自收紧。

## 三、sema 与运行时的分工 (规范性)

| 检查 | 归属 | 消息来源 |
|---|---|---|
| 未定义绑定/val 重赋值/incdec 四错/二元操作数/Neg/循环条件/元数/调非函数/顶层 return | sema (前移) + 运行时守卫 | 黄金消息逐字节冻结 |
| 声明↔初始化/实参↔形参/return↔声明/未知类型名/泛型形状 | sema 独有 | 新消息, 风格对齐 |
| Q① bool 有序比较 / Q③ 落空 / Q④ main 形状 / Q② 参数赋值 | sema 收紧 | 新消息或沿用原文 |
| MethodCall/Field/Index 占位、除零、import 通知 | 仅运行时 | 黄金记录冻结 |

sema 不下钻 MethodCall/Field/Index 子树 — 运行时在求值 recv 前即报占位错,
不下钻保证 surfaced 错误与现状逐字节一致。


# 附录二 — 原生运行时契约 (规范性, 2026-08-27)

## §五 运行时符号契约

规范的机器可检查版本位于 `src/codegen/runtime.rs::RUNTIME_CONTRACTS`。
每个表项同时声明参数、返回值和每条边的可空性；调用点只给符号与实参，
由该表生成 Cranelift 签名。`native_runtime.rs` 实际定义集合必须与契约表精确相等。
`nullable` 只允许在契约明确标注的边上出现：空串输入指针、零长度 stdout
指针、无捕获 closure env 及可承载引用的数组载荷字。

| 符号 | 签名 | 语义 |
|------|------|------|
| alias.cell.new | (bytes:i64)→i64 | 泄漏并清零 bytes 字节存储区；绑定/结构体值由调用端按声明宽度写入 |
| alias.env.new | (i32)→i64 | 泄漏 n×8 字节捕获槽区 |
| alias.globals.new | (bytes:i64)→i64 | 泄漏并清零按混型布局计算的顶层槽区 |
| alias.closure.new | (i64,i64)→i64 | 泄漏 {code,env} 16 字节闭包对象 |
| alias.str.new | (ptr,i32)→i64 | 复制字节 → 泄漏块 {data_ptr,len} |
| alias.str.concat | (i64,i64)→i64 | 拼接新块 |
| alias.str.cmp | (i64,i64)→i32 | 字典序字节比较 -1/0/1 |
| alias.str.len / upper / lower / trim | (i64)→i32 / i64 / i64 / i64 | 字节长度、ASCII 大小写映射与四字符集 trim |
| alias.arr.new / len / push / pop | (i32,i32)→i64 / (i64)→i32 / (i64,i64) / (i64)→i64? | raw 数组头的分配、增长和弹出；当前调用端固定传 8 字节载荷槽，pop 字可承载 null 引用 |
| alias.display.int/i64/u64/f32/f64/bool/func/str | 各型→i64 | Value::display 规则 (§display 表)；不存在 unit display 符号 |
| alias.display.struct/array/result | ()→i64 / ()→i64 / (i32)→i64 | 复合值固定显示或按 result tag 显示 |
| alias.println.str / print.str | (i64) | 写块 + 可选换行 |
| alias.println/print.i32/bool | (i32) | 经 display 复用 |
| alias.abort_div/oob/pop/conv/overflow | (i32) | span-ID 查表；编译产物输出诊断后 exit 1 |
| rt.heap.alloc / rt.write.dec / rt.write.stdout | 内部契约 | 零初始化分配、十进制写和 stdout 写 |

字符串表示: 泄漏 16 字节块 {data_ptr: u64, len: u64}; data_ptr 为 null
当且仅当 len=0。字节一律复制进块 — 统一所有权。

## §五.1 值 ABI 与布局单源

`src/codegen/abi.rs` 是语言值物理表示的唯一规范实现。每个值类型的 `VTy`
通过 `ValueAbi` 同时给出：规范在途寄存器类型、实际存储类型与字节数、对齐、
用户函数参数类型、返回类型以及 8 字节 result/array 载荷字编码。`unit` 返回标记
生成零返回值 ABI，不分配返回槽，也不制造哑返回字。整数表达式
在途规范为 I64，但 i8/i16/i32/u8/u16/u32 的参数、返回和内存槽保持声明宽度；
f32/f64 始终使用对应浮点寄存器，进入载荷字时才按位装箱。

结构体布局由同层两阶段计算：先登记全部结构体名字，再按声明序对齐字段，
最后按最大字段对齐补尾随填充。单元格、全局槽和字段不得在调用点
另写宽度或偏移表；用户函数和间接闭包调用统一由 `user_signature` 加入
`globals/env` 两个隐藏 I64 参数并生成显式参数/返回 ABI。

## §六 唯一原生编译形态

- CLI: `alias run <source.as>` 先生成并链接临时 exe 后启动；`alias build <source.as>`
  输出与源同目录同名 exe（成功静默）；裸 `alias <source.as>` = run。build 只接受
  `.as` 扩展名，避免输入路径与输出 `.exe` 指向同一文件。
- 禁止进程内执行：编译器不加载或调用生成的机器码，也不提供宿主 runtime；
  两个命令共享同一 `compile_to_object → link_exe` 管线。
- 库接口 `run(path,src)` 的 `Err` 只表示词法、语法、语义、代码生成、链接或
  进程启动失败；编译产物自身的运行时中止由子进程输出诊断，返回其退出码 1。
- 产物依赖: 仅 kernel32.lib (GetStdHandle/WriteFile/ExitProcess/
  HeapAlloc/GetProcessHeap/RtlMoveMemory)。**无 CRT**:
  入口为导出 alias_start (显式 ExitProcess 传退出码),
  十进制转换与字节比较由 shim IR 实现。
- 已知限制: cranelift-object 不写 .pdata/.xdata — SEH 展开穿越
  Alias 帧暂不支持; console 程序无碍。

## §六.1 编译输入健壮性边界

- 源码最大 8 MiB，token 最大 200000。
- `()`、`[]`、`{}` 的组合语法嵌套、泛型类型嵌套和字符串插值嵌套最大 128 层。
- 加减、乘除、后缀、无括号调用与连续一元负号最大 256 项/层。
- 超限、u64 整数字面量越界、负整数字面量超出 i64、非有限浮点字面量都返回带 span 的中文编译错误，不得 panic 或栈溢出。

---

# 附录三 — Phase 2a struct 规范 (规范性, 2026-08-25 用户批准)

## §七 文法

```
struct_def  := "struct" IDENT "{" field* "}"
field       := ("val" | "var") type_expr IDENT ("=" expr)?      行界分隔
call_arg    := IDENT "=" expr | expr                            单 '=' 即标签
stmt        := ... | recv "." IDENT "=" expr                    字段赋值语句
```

- 构造与函数调用共用一处语法空间: `N(k = v)` 与 `f(k = v)` 同形解析
  为带标签实参 — 被调方合法性由 sema 按名字解析结果裁决 (M24)。
- 字段默认值 `= expr` 按声明期词汇环境校验类型; 构造期求值。

## §八 语义 (冻结)

| 主题 | 裁决 |
|------|------|
| 值模型 | 实例 = `alias.cell.new(bytes)` 分配的泄漏槽区；字段按自身 size/align 排布并含填充，变量持实例指针 |
| 引用语义 | 赋值/传参/闭包捕获共享同一实例 — 经任一别名写字段, 其余别名立即可见 |
| 可变性 | 字段级: var 字段可写与绑定自身 val/var 无关; val 字段写 → 「'{f}' 是 val 字段, 不可赋值」; 绑定重指仍受绑定 val/var 管辖 |
| 构造 | 全命名实参; 缺省字段取声明默认值; 必填字段缺失/重复/未知/类型不符各有独立诊断; 实参按声明序求值 |
| 打印 | 结构体值经 println/print/插值 → 固定 `<struct>` (display 表新增行, 与 `<func>` 对称) |
| 命名空间 | 结构体名与 func/绑定单一命名空间: 重名即编译错误; 名字被局部绑定遮蔽时构造分派让位于普通调用 |
| 类型槽 | 已登记结构体名合法 (`val stat s = ...` / `func stat mk = ...`); 未登记 → 「未知类型名」; 声明前不可见 (与绑定同序) |
| 边界 | 无泛型 (result<T,E> 等仍按 Phase 5+ 拒绝); 无方法调用; struct 仅顶层可定义 |

锁定: tests/struct_laws.rs (22 用例) + demos/structs.as 原生产物黄金记录。

---

# 附录四 — Phase 2b result/match/?/转义 规范 (规范性, 2026-08-25 用户批准；Pattern 于 2026-08-27 扩展)

## §九 文法

```
type_expr   := ... | "result" "<" type_expr "," type_expr ">"
ctor_call   := ("ok" | "err") "(" expr ")"
match_expr  := "match" expr "{" arm* "}"
arm         := pattern "->" arm_body [","]?
pattern     := "_" | IDENT | INT | BOOL | STRING
             | ("ok"|"err") "(" (IDENT|"_") ")"
arm_body    := "{" 块 "}" | "return" expr | expr
postfix     := ... | postfix "?"                            ? 与字段/调用同缀
escape      := \n | \t | \r | \\ | \' | \" | \0 | \$        字面量与插值 Lit 部
```

- 臂间逗号可选: 换行与逗号皆可分隔; 尾逗号容忍。
- 调用实参尾逗号容忍。
- 字符串 Pattern 必须是纯字面量，不能含插值。
- 本批不支持 guard、struct Pattern、嵌套 constructor payload Pattern 或用户自定义 Pattern 构造器。

## §十 语义 (冻结)

| 主题 | 裁决 |
|------|------|
| result 类型 | 内建泛型枚举, 恰两参 `result<T,E>` |
| 构造器 | ok/err 为类型构造器非名字分派函数; 恰一位置实参; 单侧推断 ok(e):result<typeof e, Unknown> / err(e):result<Unknown, typeof e>, 另一侧由声明上下文统一; 被绑定遮蔽即普通调用 |
| match 表达式 | 主语可为一般静态类型。`_` 与普通 IDENT 均为 catch-all；IDENT 以不可变 val 绑定整个主语。整数/bool/string 字面量 Pattern 必须与主语类型兼容。`ok(name|_)` / `err(name|_)` 仅适用于 result，name 绑定对应 payload。各臂非 never 值必须同型。 |
| 穷尽性 | bool 可由 true+false 或 catch-all 穷尽；result 可由 ok+err 或 catch-all 穷尽；整数/string 等开放域必须有 `_` 或普通绑定兜底。重复字面量、重复 ok/err、以及完整覆盖后的后续 arm 都是编译错误。 |
| never 流 | Ret 臂 (`-> return e`) 与 return 收尾块臂贡献 never 流, 直接跳函数返回路径; **全 never 臂 match 等价 return 收尾** |
| ? 传播糖 | 脱糖等价 `match e { ok(v) -> v, err(e) -> return err(e) }`; 仅当所在函数声明返回 result<_, E'> 且 E == E' 合法 |
| Q③ 协同 | 声明返回非 unit 的函数以全 never match 收尾合法; 其余落空规则不变 |
| 推断不外泄 | func 绑定声明了类型槽时, 函数签名 ret 取声明词汇而非体推断值 |

## §十一 运行时表示 (冻结)

- **result 实例 = 泄漏 2×8 字节块 {tag: I64, payload: I64}**;
  tag 0=ok / 1=err, payload 为规范字。构造镜像 struct 槽区模式。
- `match` 降级为按 Pattern 顺序测试并进入对应 arm；sema 保证最后剩余路径必被覆盖。result 构造器 Pattern 测 tag；字符串 Pattern 走现有字节比较；字面量整数/bool 走原生比较。
- `?` 的 err 路径原样返回主语 result 块。
- **打印**: result 值经 println/print/插值 → 运行时 tag 定 `<ok>`/`<err>`。
- 原生 runtime 契约符号: alias.display.result(I32)→I64。

锁定: `tests/result_laws.rs` + `tests/pattern_laws.rs` + demos/result_match.as 原生管线一致性。

---

# 附录五 — Phase 2c 扩展方法规范 (规范性, 2026-08-25 用户批准；pub 于 2026-08-27 替换旧关键字)

## §十二 文法

```
method_def  := ("pub")? "func" type_expr IDENT "." IDENT "=" func_lit
self_expr   := "self"          关键字 — 方法体内为隐式 val 绑定
postfix     := ... | postfix "." IDENT "(" args ")"
```

- 方法只能在顶层定义; 体必须是函数字面量。
- `pub` 只允许顶层绑定。旧 `public` 不再是关键字，也不存在兼容别名或迁移诊断。

## §十三 语义 (冻结)

| 主题 | 裁决 |
|------|------|
| 接收者域 | 已知完整 Alias 值类型均可作为扩展接收者；unit 不是值类型；完整 `TypeExpr` 决定静态分派身份 |
| self | 隐式 val 绑定: 不在参数表、类型 = 完整接收者、不可重绑定/不可 increase; 方法体外出现 → 「未定义的绑定 'self'」 |
| 命名空间 | 方法表二级结构: 接收者类型 → (方法名 → 签名); 与裸函数/绑定/字段名互不干扰 |
| 登记 | 签名先入表后查体 — 方法体可递归自调用; 同型同名重复 → 「类型 X 上已定义方法 'm'」 |
| 内建 | string 上的 len/upper/lower/trim 编译器提供; 数值 plus/minus/times/div 与符号运算共享语义; 内建不可覆盖 |
| pub | 可见性标志存储于方法/绑定元数据；单编译单元内恒可调，未来 import/module 语义使用该标志 |
| 调用点 | 静态分派: 接收者推断类型查表; 命名实参拒绝; 元数不含 self; 实参类型逐位校验; 返回类型流入推断 |
| Q③/Q④ 协同 | 方法体落空规则与函数一致; 方法不参与 main 候选 |

## §十四 运行时表示 (冻结)

- **方法 = 普通内部函数**；有返回值方法约定 `fn(globals, env, self, args...) -> word`，unit 方法使用零返回值 ABI；调用点直调，env 传哑字 0。
- **内建字符串方法统一 runtime 符号**: alias.str.len(I64)→I32 / alias.str.upper·lower·trim(I64)→I64。
- **空串结果 data_ptr 恒 null** (§五契约); 非空结果字节一律复制进新块。

锁定: tests/method_laws.rs + demos/methods.as 原生管线一致性。

---

# 附录六 — Phase 2d array<T> 规范 (规范性, 2026-08-25 用户批准)

## §十五 文法

```
type_expr   := ... | "array" "<" type_expr ">"          恰一参, T 递归
array_lit   := "[" [ expr ("," expr)* [","] ] "]"       尾逗号容忍 (M27 先例)
postfix     := ... | postfix "[" expr "]"               下标读
lvalue      := ... (不含下标)                            arr[i] = x 拒绝
```

- 空字面量 `[]` 合法 — 元素类型为 Unknown, 由声明上下文统一;
  裸空字面量推断 array<未知>。
- `array<>` 零参在语法层拒绝;
  `array<i32, string>` 多参在 sema 报「array 需要 1 个类型参数, 实际 N 个」。

## §十六 语义 (冻结)

| 主题 | 裁决 |
|------|------|
| 值模型 | 语言值 = 泄漏 16 字节 wrapper {raw_header: I64, version: I64}；raw_header 指向 24 字节 {data_ptr,len,cap} + cap×8 元素载荷缓冲。空数组 data_ptr 恒 null；变量持 wrapper 指针 |
| 引用语义 | 赋值/传参/闭包捕获共享同一 wrapper — 经任一别名 push/pop，raw header 变化立即可见且共享 version 加一 |
| 字面量 | 元素按书写序求值逐个入缓冲; 元素类型须一致 |
| 下标读 | 主语与下标按序求值 → I32 域越界守卫 → data_ptr 偏移加载 |
| 下标赋值 | 本阶段拒绝 — 解析层「下标赋值尚未支持」 |
| 内建方法 | len()→i32 / push(v) 无返回值 / pop()→元素字; push 实参须等于元素类型; pop 空数组 → 运行时中止 |
| 打印 | 数组值经 println/print/插值 → 固定 `<array>` |

## §十七 运行时表示与符号契约 (冻结)

- **iterator 表示**: `iterator<T>` 是泄漏 24 字节对象
  `{array_wrapper: I64, cursor: I64, expected_version: I64}`。每次取元素前比较
  wrapper.version；任一别名 push/pop 后旧 iterator 以
  「遍历期间集合结构已修改」中止。
- **中止机制**: 越界读与 pop 空数组走 span-ID 中止存根；iterator 失效由产物直接写 stderr 后退出 1。
- **统一 runtime 符号** (§五 契约扩充):

| 符号 | 签名 | 语义 |
|------|------|------|
| alias.arr.new | (cap:i32, elem_size:i32)→i64 | 只分配 raw 头块与 cap×elem_size 缓冲并返回 raw_header；16 字节共享 wrapper 由调用端另行分配，当前元素槽固定 8 字节 |
| alias.arr.len | (i64)→i32 | 头块 len 字段 |
| alias.arr.push | (i64,i64) | 增长并尾插 |
| alias.arr.pop | (i64)→i64 | len-=1 返回 data[len] |
| alias.display.array | ()→i64 | 固定 "<array>" 块 |
| alias.abort_oob / alias.abort_pop | (i32) | span-ID 中止存根 |
| alias.abort_overflow | (i32) | 整数算术或左移结果超出声明宽度时输出「整数溢出」并中止 |

锁定: tests/array_laws.rs + demos/arrays.as 原生管线一致性。

# 附录八 — 无括号文法泛化 (P2e, 规范性)

无括号调用绑定紧于后续二元运算；`dup 5 + 1` 编译错误，写作 `(dup 5) + 1` 或 `dup (5 + 1)`。
函数值传参须显式括号 `f(g)`；零参调用必须 `five()`。

`println f 0` / `print f 0` 允许外层输出内建的唯一实参本身是一个普通单参无括号调用，等价 `println(f(0))`；这不放宽 `dup 5 + 1`。

## 2026-08-27 控制流与运算规范收口

本节为当前规范，若与前文 Phase 1 的旧描述冲突，以本节为准。

- `func` 绑定的 RHS 必须直接是函数字面量；不能用既有函数值初始化另一个 `func` 绑定。
- 顶层命名函数检查函数体前先登记自身完整签名，因此允许直接递归；这不开放后续声明的前向引用。
- 非 `unit` 函数不存在隐式返回；所有可达落空路径都必须由显式 `return <value>` 终止。校验基于控制流。
- 单行函数体可省略花括号，但仍是单条语句：`func i32 f = () -> return 1` 合法，`func i32 f = () -> 1` 非法。`unit` 函数允许自然落空。
- `while <bool> { ... }` 是条件循环；`for <Type> <name> in <Expr> { ... }` 是迭代循环。旧 `for condition { ... }` 与 C 风格 `for(init; cond; step)` 均非法。
- `for` 当前消费 `array<T>` 或 `iterator<T>`；循环变量为隐式不可重新绑定的 `val`。`break` / `continue` 作用于最近一层循环。
- `array<T>.iterator()` 返回真实 `iterator<T>`。数组及其别名的结构性 `push/pop` 会推进共享版本号；旧 iterator 后续消费时 fail-fast 报“遍历期间集合结构已修改”。
- 泛型类型上下文会把 lexer 合并的 `>>` 拆成两个右尖括号，因此 `array<array<i32>>` 无需插入空格；表达式上下文的 `>>` 仍是右移运算符。
- `&&` / `||` 为运行时短路；`?:` 只求值被选中的值分支。
- 当前运算符优先级（高→低）：后缀/调用/方法 > 一元 `- ! ~` > `* / %` > `+ -` > `<< >>` > 有序比较 > `== !=` > `&` > `^` > `|` > 无括号方法/调用边界 > `&&` > `||` > `?:`。
- `%` 仅适用于同型整数；`& | ^ ~ << >>` 仅适用于整数。整数宽度与有/无符号身份必须一致，不做隐式混算。`>>` 对有符号整数为算术右移，对无符号整数为逻辑右移。
- 整数 `+/-/*`、左移 `<<`、一元负号和 `increase/decrease` 采用声明宽度 checked 语义；溢出、无符号下溢、左移丢失有效位或移位数不小于类型宽度，均在运行时按表达式 Span 报「整数溢出」并退出 1。`INT_MIN / -1` 走同一溢出诊断；除数为零仍报「除以零」。`& | ^ ~ >>` 保持固定位宽位模式语义。
- 不提供 `+= -= *= /= %= &= |= ^= <<= >>=` 等复合赋值。
- 数值类型提供内建 `plus/minus/times/div`，分别与 `+/-/*//` 共享静态规则和原生 lowering；`bool.not()` 与 `!` 共享取反语义。非数值类型仍可定义自己的同名扩展方法。
- `increase name` / `decrease name` 是独立语句，不是返回 unit 的表达式。目标必须是可变数值绑定；整数执行 checked 加减 1，f32/f64 按同型加减 1.0。任何赋值、return、实参、插值等值位置均编译期拒绝。
- 普通赋值与结构体字段赋值均执行静态目标类型一致性检查。
- `pub` 是唯一公开可见性关键字，只允许顶层绑定；旧 `public` 不再是关键字或兼容语法。
- `match` 使用统一 Pattern AST；第一批为 `_`、普通标识符绑定、整数/bool/纯字符串字面量、`ok(name|_)` / `err(name|_)`。guard 与 struct Pattern 暂不加入。

## 2026-08-27 转换、整数范围与当前函数自引用

- 整数字面量的无符号词法范围为 `0..=18446744073709551615`。无上下文正整数字面量依次默认推断为 i32、i64、u64；负整数字面量只允许落在 i64 范围。进入已知整数目标槽时，先按目标类型检查字面量范围，不允许截断。
- `(T) value` 明确指定目标类型。转换关系包括数值类型互转，以及所有具有 display 规则的具体值到 string；unit 无返回值表达式不属于转换源或目标。后者与 println/插值复用同一渲染契约。整数目标转换在运行时检查值域，越界报「转换越界」；浮点到整数同时拒绝 NaN 与目标范围外值。
- `from(value)` 与无括号形式 `from value` 必须由声明、赋值、return、实参、字段、字符串插值或复合表达式的目标槽给出目标类型；没有目标上下文时编译错误。插值孔的目标为 string，因此 `'${from u}'` 会把数值按 display 规则转换为文本。转换关系不存在时编译错误。
- 普通赋值从绑定类型、字段赋值从字段声明类型向整个 RHS 传播目标类型；sema 为此先解析目标类型，但原生代码仍保持既定的 RHS 运行时求值顺序。
- `try_from(value)` 使用相同的目标类型规则。转换关系存在时与 `from` 一样执行并检查值域；关系不存在时不制造新类型，静默保留源表达式的类型，再由外层目标槽执行普通一致性检查。因此 `val i32 a = try_from(b)` 在 `b:string` 时与 `val i32 a = b` 产生同一类型错误。
- `to_i8`…`to_f64` 旧内建已删除，不保留别名或兼容诊断。

## 2026-08-28 unit 无返回值语义

- `unit` 只允许单独出现在 `func unit name = ...` 或 `func unit Type.method = ...` 的返回类型槽中。参数、val/var、结构体字段、for 变量、方法接收者、数组/result/iterator 类型参数和 cast 目标均拒绝 unit。
- unit 函数允许自然落空或使用裸 `return`；`return <expr>` 非法。非 unit 函数仍必须显式返回值。
- 调用 unit 函数和无返回值内建只能作为独立表达式语句。绑定、赋值、return、实参、数组元素、result 载荷、match 主语、转换、插值、print/println 与 typeof 等值位置统一报「无返回值表达式不能用于值位置」。
- `()` 不再构造值，遇到值语法位置时编译错误「() 不是值；unit 只表示函数不返回值」。函数参数表的 `()` 仍表示零参数，与 unit 无关。
- 原生 ABI 对 unit 返回函数不声明返回值、不建立返回块参数；`alias.display.unit` 与静态 `()` 数据已删除。
- `this` 在每个 func 体内绑定当前函数自身，类型为当前函数完整签名。函数改名不影响递归；进入嵌套 func 后重新绑定到内层函数；func 体外使用编译错误。
- `typeof(expr)` 与 `typeof expr` 是静态类型查询特殊形式，结果为 string 类型名。实参照常完成名字解析与类型检查，但生成代码不求值实参；`typeof(1 / 0)` 返回 `i32`，不会执行除法。数组、iterator、result 使用完整泛型类型名。
