# Alias 规范笔记 — Phase 0 规范冻结

**状态**: 规范性文档 (normative)。本文与 `tests/golden.rs` 共同构成解释器的行为契约。
**权威链**: `.omo/plans/compiler-migration.md` (用户已裁决 D1–D5 + Q①–Q⑥) → 本文固化 → `tests/golden.rs` 逐字节执行。
**冻结日期**: 2026-08-24。此后任何改动若使黄金记录变红, 必须先修订本文并说明裁决依据。

---

## 一、Value::display 渲染格式表 (规范性)

来源: `src/interp.rs:23-31` (`impl Value::display`)。字符串插值的洞 (`'n=$i'`) 与
内建 `println`/`print` 的输出一律经过此表, 无第二渲染路径。

| 运行时值 | 变体 (`interp.rs:14-20`) | 显示字节 | 备注 |
|---|---|---|---|
| 整数 | `Int(i64)` | 十进制表示 (如 `48`, `-3`) | 内部 i64 承载, 对外语义称 i32 |
| 布尔 | `Bool(bool)` | `true` / `false` | 小写字面量 |
| 字符串 | `Str(String)` | 原文字节 | **不加引号、不转义**, 与源码字面量形态无关 |
| 函数值 | `Func(Rc<FuncValue>)` | `<func>` | 永不泄露闭包/参数信息 |
| 单元 | `Unit` | `()` | |

**已知陷阱 (冻结)**: 语句 `println ()` 无法打印单元 — parser 后缀链把 `(` 吞作
零参调用 (`src/parser.rs:430-433`), 报「println 恰好接受 1 个参数」。显示 unit
须经绑定中转 (`val unit u = ()` 后 `println u`), 见黄金记录 `display_func_and_unit`。

## 二、行为怪癖裁决 Q①–Q⑥ (规范性, 用户已裁决, 实现代理不得重开)

| # | 现状 (位置) | 裁决 | 黄金锚点 |
|---|---|---|---|
| Q① | `true < false` 静默求值为 `false` (`src/interp.rs:317`, bool 分支 `_ => false`) | **改编译错误**: 有序比较仅限同型 i32/i64 与 string; EqEq/NotEq 对 bool 仍合法 | Phase 1 以 `tightened_*` 测试锁定; 现状不入黄金记录 |
| Q② | 函数参数隐式为 val 绑定 (`src/interp.rs:354`, `mutable: false`) | **保留**运行时语义; 编译期拒绝对参数赋值 | `closure_reference_capture_latest_value` (间接依赖) |
| Q③ | 声明返回非 unit 的函数块体落空时静默得 `Unit` (`src/interp.rs:363`, `Ok(_) => Ok(Value::Unit)`) | **编译错误**: 声明返回非 unit 的函数控制流必须可达 return; 仅 unit 可落空 | Phase 1 锁定 |
| Q④ | main 返回 string/unit 时静默退 0 (`src/interp.rs:122`, `Ok(_) => Ok(0)`) | **sema 校验 main**: 存在/func/零参/返回∈`{i32,bool,string,unit}`; 退出映射: i32→原样 (clamp 在进程边界)、bool true→0 false→1、unit→0、string→0 (**不打印**)。其余类型编译错误 | `bool_main_true_exit_0`、`bool_main_false_exit_1`、`exit_code_clamped_to_255` |
| Q⑤ | 缺 main 报 `@ 0:0` (`src/interp.rs:116`, `Span::default()`) | **修复**: Span 为 default 时 Display 省略位置前缀, 只报「找不到顶层 func main」 | 现状由 `missing_main_error` 冻结; 修复落地时该行随 MIGRATION 条目同步改写 |
| Q⑥ | 顶层绑定按序求值、先于 main、可有副作用 (`src/interp.rs:96-113`) | **保留顺序语义**: 生成的入口 wrapper 先按序求值顶层初始化再调用用户 main | `top_level_side_effect_ordering` |

## 三、已冻结的观察事实 (规范性, 黄金记录逐字节背书)

以下事实来自对当前构建的实际探测, 属于可观察契约的一部分:

1. **错误 Display 格式**: `错误 @ {line}:{col} — {msg}` (`src/lib.rs:26-30`)。
   全部运行时/编译期错误经此单一路径。
2. **列号 0 起始**: lexer 产出的 col 从 0 计数 (实测 `return 1 / 0` 中 `1` 报
   `2:11`; 行首缩进四格的 `a` 报 `3:4`)。行号 1 起始。
3. **进程边界 clamp**: main 返回值经 `code.clamp(0, 255)` 输出
   (`src/main.rs:22`); 返 300 → 进程退出码 255。
4. **CLI 层退出码**: 无参数 → stderr「用法: alias <source.as>\n」+ 退出码 2;
   文件不可读 → stderr「无法读取 {path}: {os error}\n」+ 退出码 2
   (`src/main.rs:5-19`; 占位符原为 `<script.as>`, M18 术语清理改为 `<source.as>`)。
5. **import 通知**: import 只解析不执行, 向 stderr 打印
   「[alias] 注意: {n} 条 import 已解析但标准库尚未接入 (Phase 5 前)\n」
   (`src/interp.rs:89-94`)。该文本已进黄金记录, Phase 5 接入标准库前不得变动。
6. **count_to_ten 打印 1..10**: demo 循环体先 `increase i` 再 `println i`,
   故 stdout 为 `1\n…\n10\n` — 不是直觉的 0..9。以实际字节为准。
7. **除零是运行时错误**: 「除以零」, span 取除号左侧操作数
   (`src/interp.rs:301-305`), 退出码 FAILURE(1), 非 panic。

## 四、黄金记录索引

`tests/golden.rs` 断言编译产物二进制的精确三元组 (stdout 字节/stderr 字节/退出码):

| 用例名 | 覆盖点 |
|---|---|
| `count_to_ten_demo` | demo 夹具 + import 通知 + 循环顺序 |
| `arithmetic_exit_48` | i32 main 返回值即退出码 |
| `bool_main_true_exit_0` / `bool_main_false_exit_1` | Q④ bool 映射 |
| `string_interpolation_equality` | 插值 `'n=$i'` + 字符串相等 |
| `division_by_zero_error` | 除零消息 + 0 起始列号 |
| `display_func_and_unit` | display 表 `<func>` / `()` |
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

管线自此为 lex → parse → **sema** → execute (`src/lib.rs run`)。sema 通过的
程序, 解释器仍是行为权威; 运行时检查原样保留 (Phase 4 才删)。

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
| 未定义绑定/val 重赋值/incdec 四错/二元操作数/Neg/循环条件/元数/调非函数/顶层 return | sema (前移) + 运行时保留 | interp.rs 原文逐字节 |
| 声明↔初始化/实参↔形参/return↔声明/未知类型名/泛型形状 | sema 独有 | 新消息, 风格对齐 |
| Q① bool 有序比较 / Q③ 落空 / Q④ main 形状 / Q② 参数赋值 | sema 收紧 | 新消息或沿用原文 |
| MethodCall/Field/Index 占位、除零、import 通知 | 仅运行时 | 黄金记录冻结 |

sema 不下钻 MethodCall/Field/Index 子树 — 运行时在求值 recv 前即报占位错,
不下钻保证 surfaced 错误与现状逐字节一致。


# 附录二 — Phase 5 AOT 运行时契约 (规范性, 2026-08-25)

## §五 运行时符号契约 (JIT 宿主函数与 AOT shim 逐字节对齐)

| 符号 | 签名 | 语义 |
|------|------|------|
| alias.cell.new | (bytes:i64)→i64 | 泄漏并清零 bytes 字节存储区；绑定/结构体值由调用端按声明宽度写入 |
| alias.env.new / alias.globals.new | (i32)→i64 | 泄漏 n×8 字节槽区 |
| alias.closure.new | (i64,i64)→i64 | 泄漏 {code,env} 16 字节闭包对象 |
| alias.str.new | (ptr,i32)→i64 | 复制字节 → 泄漏块 {data_ptr,len} |
| alias.str.concat | (i64,i64)→i64 | 拼接新块 |
| alias.str.cmp | (i64,i64)→i32 | 字典序字节比较 -1/0/1 |
| alias.display.int/bool/unit/func/str | 各型→i64 | Value::display 规则 (§display 表) |
| alias.println.str / print.str | (i64) | 写块 + 可选换行 |
| alias.println/print.i32/bool | (i32) | 经 display 复用 |
| alias.abort_div | (i32) | span-ID 查表 → stderr「错误 @ L:C — 除以零」→ exit 1 |

字符串表示: 泄漏 16 字节块 {data_ptr: u64, len: u64}; data_ptr 为 null
当且仅当 len=0。字节一律复制进块 — 统一所有权。

## §六 AOT 形态

- CLI: `alias run <source.as>` (JIT) / `alias build <source.as>` (AOT exe,
  与源同目录同名 .exe, 成功静默); 裸 `alias <source.as>` = run。
- 产物依赖: 仅 kernel32.lib (GetStdHandle/WriteFile/ExitProcess/
  HeapAlloc/GetProcessHeap/RtlMoveMemory)。**无 CRT**:
  入口为导出 alias_start (显式 ExitProcess 传退出码),
  十进制转换与字节比较由 shim IR 实现。
- 已知限制: cranelift-object 不写 .pdata/.xdata — SEH 展开穿越
  Alias 帧暂不支持; console 程序无碍。

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
| 值模型 | 实例 = 泄漏 n×8 字节槽区 (复用 alias.env.new 符号), 字段按声明序偏移 idx*8 存 64 位规范字; 变量持实例指针 |
| 引用语义 | 赋值/传参/闭包捕获共享同一实例 — 经任一别名写字段, 其余别名立即可见 |
| 可变性 | 字段级: var 字段可写与绑定自身 val/var 无关; val 字段写 → 「'{f}' 是 val 字段, 不可赋值」; 绑定重指仍受绑定 val/var 管辖 |
| 构造 | 全命名实参; 缺省字段取声明默认值; 必填字段缺失/重复/未知/类型不符各有独立诊断; 实参按声明序求值 |
| 打印 | 结构体值经 println/print/插值 → 固定 `<struct>` (display 表新增行, 与 `<func>` 对称) |
| 命名空间 | 结构体名与 func/绑定单一命名空间: 重名即编译错误; 名字被局部绑定遮蔽时构造分派让位于普通调用 |
| 类型槽 | 已登记结构体名合法 (`val stat s = ...` / `func stat mk = ...`); 未登记 → 「未知类型名」; 声明前不可见 (与绑定同序) |
| 边界 | 无泛型 (result<T,E> 等仍按 Phase 5+ 拒绝); 无方法调用; struct 仅顶层可定义 |

锁定: tests/struct_laws.rs (22 用例) + demos/structs.as 双形态 parity。

---

# 附录四 — Phase 2b result/match/?/转义 规范 (规范性, 2026-08-25 用户批准)

## §九 文法

```
type_expr   := ... | "result" "<" type_expr "," type_expr ">"
ctor_call   := ("ok" | "err") "(" expr ")"
match_expr  := "match" expr "{" arm* "}"
arm         := ("ok"|"err") "(" IDENT ")" "->" arm_body [","]?
arm_body    := "{" 块 "}" | "return" expr | expr
postfix     := ... | postfix "?"                            ? 与字段/调用同缀
escape      := \n | \t | \r | \\ | \' | \" | \0 | \$        字面量与插值 Lit 部
```

- 臂间逗号可选: 换行与逗号皆可分隔; 尾逗号容忍 (file_wc.as 冻结形状)。
- 调用实参尾逗号容忍 (M27 — 构造实参跨行书写的必然配套)。
- 臂构造器名只接受 ok/err, 其余语法层拒绝 (语言无用户自定义枚举,
  P7 字面量模式 match 属另一未立案特性)。

## §十 语义 (冻结)

| 主题 | 裁决 |
|------|------|
| result 类型 | 内建泛型枚举, 恰两参 `result<T,E>`; 本阶段唯一合法泛型, 其余泛型仍报 Phase 5+ |
| 构造器 | ok/err 为类型构造器非名字分派函数; 恰一位置实参; 单侧推断 ok(e):result<typeof e, Unknown> / err(e):result<Unknown, typeof e>, 另一侧由声明上下文统一; 被绑定遮蔽即普通调用 (与 struct 分派镜像) |
| match 表达式 | 主语须为 result<T,E> → ok 臂绑定 : T / err 臂绑定 : E; 穷尽性 = 恰一 ok + 恒一 err (缺臂「match 必须同时覆盖 ok 与 err」, 重复臂独立诊断); 臂绑定 val 语义、作用域 = 臂体; 值 = 非 never 臂公共类型 (不一致即编译错误), 全 never → 类型不可用 |
| never 流 | Ret 臂 (`-> return e`) 与 return 收尾块臂贡献 never 流, 直接跳函数返回路径; **全 never 臂 match 等价 return 收尾** (Q③ 终结性扩展, M28) |
| ? 传播糖 | 脱糖 = `match e { ok(v) -> v, err(e) -> return err(e) }` (P6); 仅当所在函数声明返回 result<_, E'> 且 E == E' 合法 — 函数外/非 result 返回函数/异型错误各有独立诊断; 块体与循环体内均经既有返回通道传播 |
| Q③ 协同 | 声明返回非 unit 的函数以全 never match 收尾合法 (M28); 其余落空规则不变 |
| 推断不外泄 | func 绑定声明了类型槽时, 函数签名 ret 取声明词汇而非体推断值 (M29) — 否则构造器单侧推断经签名泄漏 |

## §十一 运行时表示 (冻结)

- **result 实例 = 泄漏 2×8 字节块 {tag: I64, payload: I64}**;
  tag 0=ok / 1=err, payload 为规范字。构造镜像 struct 槽区模式
  (alias.env.new(2) + 双 store); match 降级 = 载 tag → brif 分臂 →
  join 块参数汇合; ? 的 err 路径原样返回主语块 (tag 已为 1, 与
  `return err(payload)` 可观察等价)。
- **打印**: result 值经 println/print/插值 → 运行时 tag 定 `<ok>`/`<err>`
  (display 表新增行, 与 `<func>`/`<struct>` 对称; 载荷永不泄露)。
- JIT 宿主与 AOT shim 同契约符号: alias.display.result(I32)→I64。

锁定: tests/result_laws.rs (22 用例) + demos/result_match.as 双形态 parity
(native_parity 黄金基线 + aot_parity 语料)。

---

# 附录五 — Phase 2c 扩展方法规范 (规范性, 2026-08-25 用户批准)

## §十二 文法

```
method_def  := ("public")? "func" type_expr IDENT "." IDENT "=" func_lit
self_expr   := "self"          关键字 — 方法体内为隐式 val 绑定
postfix     := ... | postfix "." IDENT "(" args ")"    (真语义, 占位退役)
```

- 接收者类型 = 名字路径首段; 方法名 = 末段。多点路径与非 func 绑定
  维持带点名字的普通绑定形态 (parser 不预判合法性)。
- 方法只能在顶层定义; 体必须是函数字面量。

## §十三 语义 (冻结)

| 主题 | 裁决 |
|------|------|
| 接收者域 | string / bool / i32 / 已登记结构体; unit·func → 「类型 X 不能作为方法接收者」; 未知名 → 「未知类型名」; 声明前不可见 (与绑定同序) |
| self | 隐式 val 绑定: 不在参数表、类型 = 接收者、不可重绑定/不可 increase (Q② 文案); 方法体外出现 → 「未定义的绑定 'self'」; 插值洞 $self 与 ${self} 同通道 |
| 命名空间 | 方法表二级结构: 接收者类型 → (方法名 → 签名) — string.append 与 stat.append 共存; 与裸函数/绑定/字段名互不干扰; 方法不可按裸名调用 (未定义绑定) |
| 登记 | 签名先入表后查体 — 方法体可递归自调用; 同型同名重复 → 「类型 X 上已定义方法 'm'」 |
| 内建 | string 上的 len/upper/lower/trim 编译器提供, 用户覆盖 → 「内建方法不可覆盖: T.m」; len=字节长, upper/lower 仅 ASCII 字母平移, trim 剥离首尾空格/\t/\r/\n |
| public | 解析并存储于方法表; 单编译单元内恒可调 (检查机制就位) — import 阶段 (Phase 5+) 翻转为跨单元强制 |
| 调用点 | 静态分派: 接收者推断类型查表; 类型不可知时级联抑制 (先例: match 主语); 查无此名 → 「类型 X 上没有方法 'm'」; 命名实参拒绝; 元数不含 self; 实参类型逐位校验; 返回类型流入推断 — 链式调用由此逐级流动 |
| Q③/Q④ 协同 | 方法体落空规则与函数一致 (返回类型槽即期望返回类型); 方法不参与 main 候选 |

## §十四 运行时表示 (冻结)

- **方法 = 普通内部函数**, 符号 `m<接收者>名字` (Local 链接), 统一约定
  `fn(globals, env, self, args...) -> word`; 调用点直调, env 传哑字 0
  (方法无捕获, 自由名经 globals 参数可达); self 以单元格承载 —
  结构体引用语义穿透方法边界。
- **内建四件套双后端同符号**: alias.str.len(I64)→I32 /
  alias.str.upper·lower·trim(I64)→I64。JIT 宿主函数 + AOT IR shim:
  upper/lower = 逐字节范围 icmp + select 平移写新缓冲;
  trim = 双边界扫描 + RtlMoveMemory 子块复制; 无 CRT 调用。
- **空串结果 data_ptr 恒 null** (§五契约); 非空结果字节一律复制进新块。

锁定: tests/method_laws.rs (26 用例) + demos/methods.as 双形态 parity
(native_parity 黄金基线 + aot_parity 语料)。

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
- `array<>` 零参在语法层拒绝 (类型参数至少一个);
  `array<i32, string>` 多参在 sema 报「array 需要 1 个类型参数, 实际 N 个」。

## §十六 语义 (冻结)

| 主题 | 裁决 |
|------|------|
| 值模型 | 实例 = 泄漏 24 字节头块 {data_ptr: I64, len: I64, cap: I64} + cap×8 元素缓冲; 空数组 data_ptr 恒 null (镜像空串契约); 变量持头块指针 |
| 引用语义 | 赋值/传参/闭包捕获共享同一实例 — 经任一别名 push/pop, 其余别名立即可见 (镜像 struct 契约); val 绑定上的 push 合法 (变异在实例不在绑定) |
| 字面量 | 元素按书写序求值逐个入缓冲 (lhs-to-rhs 黄金冻结约定), 头块先分配、len 后回填; 元素类型须一致 — 违规元素报「数组元素类型不一致: X 与 Y」 |
| 下标读 | 主语与下标按序求值 → I32 域越界守卫 (i < 0 或 i >= len) → data_ptr 偏移加载; 主语非 array → 「下标访问需要 array 类型, 实际 X」(span 在主语); 下标非 i32 → 「下标需要 i32, 实际 X」(span 在下标) |
| 下标赋值 | 本阶段拒绝 — 解析层「下标赋值尚未支持」; 经下标的字段赋值 (`cs[i].f = v`) 不属下标赋值, 按 FieldAssign 正常通道 (写穿透实例指针) |
| 内建方法 | len()→i32 / push(v)→unit / pop()→元素字; 编译器提供, 接收者文法不含 '<' 故用户不可定义亦不可覆盖; push 实参须等于元素类型; pop 空数组 → 运行时中止 |
| 打印 | 数组值经 println/print/插值 → 固定 `<array>` (display 表新增行, 与 `<struct>` 对称; 元素永不泄露) |

## §十七 运行时表示与符号契约 (冻结)

- **中止机制**: 越界读与 pop 空数组走 span-ID 中止存根 (与除零同机制):
  发射期把守卫点行:列登记入 span 表, 运行时按 ID 回查打印
  「错误 @ L:C — {下标越界|pop 空数组}」→ exit 1。span 记账点:
  下标取 `[` token, pop 取方法调用 `.` token (parser 既有 span 语义)。
- **双后端同符号** (§五 契约扩充):

| 符号 | 签名 | 语义 |
|------|------|------|
| alias.arr.new | (i32)→i64 | 泄漏头块 + cap×8 缓冲 (cap=0 → data_ptr null), len=0 |
| alias.arr.len | (i64)→i32 | 头块 len 字段 |
| alias.arr.push | (i64,i64) | 满 len==cap → 新缓冲 2x (cap=0 取 1) + RtlMoveMemory 复制旧元素, 头块原地换 data_ptr/cap; 尾插 + len+=1 |
| alias.arr.pop | (i64)→i64 | len-=1 返回 data[len] (空守卫在发射层, shim 按契约假定非空) |
| alias.display.array | ()→i64 | 固定 "<array>" 块 |
| alias.abort_oob / alias.abort_pop | (i32) | span-ID 中止存根 (消息后缀不同) |

- AOT shim 无 CRT 调用: 增长复制走 RtlMoveMemory, 分配走 rt.heap.alloc
  (HeapAlloc); JIT 宿主以 Rust 等价实现同一布局与增长策略。

锁定: tests/array_laws.rs (21 用例) + demos/arrays.as 双形态 parity
(native_parity 黄金基线 + aot_parity 语料)。

# 附录八 — 无括号文法泛化 (P2e, 规范性)

优先级表 (高→低): 后缀链(. [ ( ?) > 无括号绑定 > 二元运算(* / > + - > 比较)。

| 形态 | 解析 |
|------|------|
| 语句入口 `裸名 unary` | Call(裸名,[unary]) — 通用, 不限内建 |
| 表达式内 `ident unary起点` | Call(ident,[unary]) — val x = dup 5 |
| 表达式内 `expr Ident(m) unary起点` | MethodCall(lhs,m,[unary]) — a plus b |
| 表达式内 `expr Ident(m) 边界` | 零参 MethodCall(lhs,m,[]) — s shout |
| 内建名单 {println,print,increase,decrease} + Ident 实参 | 名单优先: println a = Call 而非方法 |

铁律: `dup 5 + 1` 编译错误 (+ 悬空); 写法 `(dup 5) + 1` 或 `dup (5 + 1)`。
函数值传参须显式括号 f(g); 零参调用必须 five()。
已知边界: 内建名单吞参后不可链式 (println wrap 'yo' 报错, 用 println (wrap 'yo'))。
