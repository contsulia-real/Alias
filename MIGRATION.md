# 迁移记录 — 解释器 → 编译器

每条记录 = 一个语义含义发生变化的测试/行为。格式: 变化 | 裁决依据 | 为何安全。
裁决编号见 `.omo/plans/compiler-migration.md` 冻结决策表。

## Phase 1 — Sema 层 (2026-08-24)

| # | 变化 | 裁决 | 为何安全 |
|---|------|------|---------|
| M1 | 缺 main 的 stderr 由「错误 @ 0:0 — 找不到顶层 func main」变为「找不到顶层 func main」(无位置前缀); `tests/golden.rs::missing_main_error` 期望同步改写 | Q⑤ | Display 仅在 `Span == default()` (全零) 时省略前缀; lexer 保证真实 span line≥1 且 col≥1 (lexer.rs 初始化与 span_here 下限), 全零不可能自然产生。其余所有错误消息格式逐字节不变 |
| M2 | `true < false` 等 bool 有序比较由静默求值 `false` 变为编译错误「运算符 {op} 不适用于 bool 与 bool — 有序比较仅限 i32 与 string」; EqEq/NotEq 对 bool 保持合法 | Q① | 无任何既有测试/夹具依赖 bool 有序比较的静默 false; 新诊断只拒绝此前未定义语义的程序。锁定于 `sema_laws::tightened_q1_ordered_comparison_on_bool`, 正向控制 `q1_bool_equality_still_legal`、`q1_string_ordering_still_legal` |
| M3 | 声明返回非 unit 的函数块体落空由静默得 Unit 变为编译错误「返回类型为 {ty} 的函数体必须以 return 语句收尾」 | Q③ (严格版终裁见 spec-notes §一) | **用户终裁严格版** (2026-08-24): 推翻早期「驱动尾豁免」, 循环语句收尾同等拒绝。count_to_ten.as 已补 `return 0` (stdout/stderr/exit 三元组不变, 黄金记录零改动)。锁定于 `tightened_q3_block_fall_off_rejected`、`tightened_q3_loop_tail_rejected` |
| M4 | main 静态校验: 零参 + 返回 ∈ {i32,bool,string,unit}, 违者编译错误; string/unit main 静默退 0 的映射不变 | Q④ | 此前畸形 main 在运行时以不同方式失败或静默退 0; 校验前移不改变任何被接受程序的退出码。注: 返回类型非法分支在 D3 一致性检查全开下不可达 (防御纵深), 锁定于 `tightened_q4_main_no_params`、正向 `q4_string_main_exits_zero` |
| M5 | 对参数赋值 / increase 参数 由运行时错误前移为编译期错误, 消息沿用 val 绑定文案 | Q② | 参数本就注册为 immutable (interp.rs:354), 运行时必然报错; 前移只是时机提前, 消息字节不变。锁定于 `tightened_q2_param_assignment_rejected` |
| M6 | D3 一致性矩阵新诊断: 声明↔初始化「绑定 '{n}' 声明类型为 {T}, 实际 {U}」、实参↔形参「第 {i} 个实参需要 {T}, 实际 {U}」、return↔声明「return 需要 {T}, 实际 {U}」、未知类型名「未知类型名 '{n}'」、泛型形状「泛型类型 {shape} 尚未实现 (Phase 5+)」 | D3 (冻结类型集全量强制) | 全部为新发明检查 — 只拒绝此前带病运行的程序, 不改变任何被接受程序的可观察行为; 推断类型永不外泄, 诊断词汇表与 interp type_name 逐字对齐 (unit 显示为 "()") |

**金丝雀状态**: `tests/smoke.rs` 八测零修改通过 (`val_reassignment_rejected` 含 "val"、
`missing_type_slot_rejected` 含 "类型槽" — 后者源于 parser.rs:171-177, 未触碰)。

**运行时检查保留声明**: interp.rs 全部运行时检查原样保留 (Phase 4 才物理删除);
sema 通过的程序其运行时行为与 Phase 0 黄金记录完全一致 (golden 15/16 行零变化,
唯一变化行即 M1)。

## Phase 2 — Codegen 骨架 (2026-08-24)

### 依赖引入记录 (律 14 清单)

| 项 | 内容 |
|---|---|
| 问题 | 原生代码生成; D5 禁自研 VM |
| 版本 | cranelift-{codegen,frontend,module,native,object} = **0.135.0**，Cargo.lock 锚定版本+哈希 |
| 拥有者 | `src/codegen/` 唯一触点 cranelift-* API — ast/sema/lib/linker 零 cranelift 类型 (已核验) |
| 来源 | crates.io 经 rsproxy 镜像; 默认 features, 无 feature 传递引入 wasm/其他后端 |
| 安全边界 | 编译器只生成 COFF 并调用 rust-lld；生成代码只在独立原生进程中运行 |
| 演练路径 | `cargo run -- demos/hello_native.as` 完整编译临时 exe 后运行 |

### 行为记录

| # | 变化 | 裁决/依据 | 为何安全 |
|---|------|------|---------|
| M7 | 新增 `--backend native` 路由 (`run_native`); 默认无 flag 路径逐字节不变 | 计划 Phase 2 CLI 约定 | golden 全部用例零改动通过; default 回归哨兵 `default_backend_interpreter_unchanged` 锁定 |
| M8 | 除零/INT_MIN÷-1 由运行时检查变为**编译期显式守卫** → 中止存根, 消息/span/退出码与解释器逐字节一致 | 计划红线「包装算术用显式指令; 禁后端默认陷阱行为渗入」 | span-ID 表回查原始 .as 行:列 (0 起始列与解释器同规约); 锁定于 `native_div_zero_abort`、`native_int_min_div_m1_abort`、正向 `native_normal_division` |
| M9 | Q⑥ 入口 wrapper: 顶层初始化按序求值 (insert-after-eval 可见性) 后调 main; 退出映射 i32→原样 / bool→0:1 / 其余→0 | Q⑥ + Q④ 映射表 | 与 interp.rs:96-124 语义逐点对齐; hello_native 的 999 先于 main 输出即哨兵 |
| M10 | 已知有意缺口 (Phase 3 收口): 字面量超 i32 按 wrapping 收窄 (解释器 i64 承载); 驱动尾非 main 函数返回值恒 0 (解释器为 Unit); 字符串/函数值/方法等按命名 Phase 拒绝 | 计划范围边界「OUT」 | 三者均不在 Phase 2 语料内; 边界拒绝有测试锁定 (`native_string_rejected`) |

## Phase 3 — 全量 Phase-1 对等 (2026-08-24)

| # | 变化 | 裁决/依据 | 为何安全 |
|---|------|------|---------|
| M11 | 原生后端补齐字符串+插值、闭包引用捕获、一等函数值; 值模型升级为统一 64 位规范字 (Int=sext(i32)/Bool=0/1/Str/Func=泄漏堆指针); 每绑定一泄漏单元格 → 循环每迭代新鲜作用域与双向引用捕获由构造保证 (镜像 interp RefCell 作用域链); 泄漏即 GC | 计划 Phase 3 补齐清单 | 解释器为预言机: demos 语料 `read_dir` 机械枚举双后端三元组 diff 全等; count_to_ten 引用捕获哨兵 native 通过 (1..10+通知+退 0); 定向黄金 6 例逐字节锁定 |
| M12 | P2 缺口收口与残留边界: 字符串/插值/函数值已实现; 打印静态类型不可知表达式改为编译期拒绝「原生后端无法推断该表达式的显示类型」; 函数体未定义名编译期报错 (解释器运行时); **语言级限制**: `func` 为词法关键字, 无法出现在任何类型槽 — 一等函数仅匿名立即调用形态可达 (sema FuncPoly 路径为防御纵深) | 计划范围边界 | 语料无此类构造; 双后端 diff 测试保证任何可解析程序行为一致; 限制属语法层事实而非后端偏差 |
| M13 | import 在原生模式打印与解释器逐字节相同的通知后继续 (P2 为拒绝) | Q⑥/黄金记录 (interp.rs:89-94 文本冻结) | count_to_ten native 哨兵依赖; 文本逐字节复用 |

## Phase 4 — 翻转默认 + 弃用解释器 (2026-08-24)

| # | 变化 | 裁决/依据 | 为何安全 |
|---|------|------|---------|
| M14 | **物理删除 src/interp.rs** (451 行: Value/Env/Flow/Interpreter/execute 全部); lib.rs 移除 `mod interp` 与 `run_native`; `run()` 直接路由 lex→parse→sema→codegen, 公共签名逐字不变 | 计划 Phase 4「物理删除」红线 | P3 双后端 diff 已证明两实现对全部可解析语料三元组全等 — 删除不改变任何可观察行为; smoke.rs 零修改通过 (run() 契约保持) |
| M15 | CLI 移除 `--backend native` flag, 回归 `alias <source.as>` 单形态; 用法/读文件错误退出码 2 逐字节保留 (用法占位符后由术语清理 M18 从 `<script.as>` 改为 `<source.as>`) | 计划 Phase 4 | golden noargs/missing_file 用例随 M18 同步; 每个源程序现在默认编译执行 |
| M18 | 面向用户与文档的「脚本/script」术语全部清除: 用法消息占位符 `<script.as>` → `<source.as>` (golden 同步), 文档改称「源程序/源文件/示例程序」; .as 文件是编译单元, 不是脚本 | 用户裁决 | 仅一处行为字符串变更 (用法消息), golden noargs 用例同步断言; 其余均为注释/文档措辞 |
| M16 | tests/native_parity.rs 重构: 双后端 diff 不再可能 → 语料机械枚举 (read_dir 保持) 改为黄金基线断言; 6 个 demo 三元组于切换时实际探测冻结 (含 forward-spec demos 的确定性失败基线); 定向用例与哨兵保留 | 计划测试政策「诚实重写, 每处变更引用 MIGRATION」 | 基线探测自最终编译器本体; 未登记 demo 即测试失败 (防静默漂移); golden.rs 期望零改动全部通过 |
| M17 | 文档同步 (律 13): Cargo.toml 头注释 / lib.rs 模块文档 / AGENTS.md (OVERVIEW=Cranelift 编译器、STRUCTURE 树、CODE MAP、CONVENTIONS/NOTES) / 本计划状态头 → 已执行完毕 | 同步文档律 | 与代码同阶段完成 |

**终态验证**: `grep -ri "interp\|Flow" src/` 零命中; `cargo build` 零警告; 全套件 42 测试绿
(golden 2 + native_parity 3 + sema_laws 29 + smoke 8); count_to_ten 默认编译执行打印 1..10+通知退 0。
## Phase 5 — AOT 独立可执行文件 (2026-08-25)

| # | 变更 | 依据 | 安全性 |
|---|------|------|--------|
| M19 | CLI 子命令化: `alias run\|build <source.as>`, 裸单参向后兼容 (= run); 用法消息更新, golden noargs 同步 | Phase 5 计划 | 裸形态行为逐字节不变; run 路径零改动 |
| M20 | 字符串表示由 Box<String> 句柄统一为 C 布局泄漏块 {ptr,len} 16 字节 (JIT 宿主函数与 AOT shim 同契约); display/print 输出逐字节不变 (黄金背书) | AOT 无宿主, Rust 布局不可复现 | 黄金记录零改动全过 |
| M21 | FN_PTRS 运行时表退役 → func_addr 直取函数地址 (双后端通用) | AOT 无宿主表可查 | 行为等价: 地址获取时机不同, 值语义相同 |
| M22 | 新增 src/linker.rs (rust-lld 子进程) 与 codegen AOT shim 区; 纯 kernel32.lib 依赖, 无 CRT (自写入口 alias_start + ExitProcess; IR 手写十进制转换/字节比较) | lld_rs 拖 llvm-sys 违清单律; 本机新 SDK 无 msvcrt.lib | tests/aot_parity.rs 5 用例逐字节 parity |
| 已知限制 | cranelift-object 不写 .pdata/.xdata; SEH 展开穿越 Alias 帧暂不支持 (console 程序无碍) | 上游 backend.rs 文档 | 记录于 spec-notes §六 |

## Phase 2a — struct 类型 (2026-08-25)

| # | 变更 | 依据 | 安全性 |
|---|------|------|--------|
| M23 | **struct 定义落地**: `struct N { (val\|var) T f (= expr)? ... }` 顶层项; 实例 = 泄漏 n×8 槽区 (复用 alias.env.new), 字段按声明序偏移 idx*8 存规范字, 变量持指针 — 引用语义 (赋值/传参/闭包捕获共享实例); 构造 `N(k = v)` 全命名实参按声明序求值, 缺省字段取声明默认值; 字段读/写字段级可变性独立于绑定 val/var; 打印结构体值 → 固定 `<struct>` (与 `<func>` 对称); 结构体名与 func/绑定单一命名空间 (重名即编译错误); 类型槽接受已登记结构体名 | 用户批准 Phase 2a 设计 (spec-notes 附录三) | 全部为新发明检查/语法 — 只拒绝此前无法解析的程序, 不改变任何既有被接受程序的可观察行为。锁定于 tests/struct_laws.rs 22 用例 (负向 16 断言精确中文消息+行:列, 正向 6 含引用别名/传参共享/闭包捕获最新值) |
| M24 | 调用实参文法统一: `Ident = expr` (单 '=', lexer 已折 '==' 为 EqEq) 统一解析为带标签实参 — 被调方为结构体 → 必须全命名; 为函数/内建 → 一律拒绝「函数调用不接受命名实参」; 名字被绑定遮蔽时构造分派让位于普通调用路径 (与 sema 逐点镜像) | 用户批准的 parser 歧义裁决 | 此前 `f(x = 1)` 无法解析 (报「期望 RParen/无法开始表达式」类错误), 无既有语料依赖; 新诊断只作用于新语法空间 |
| M25 | demos/file_wc.as 黄金基线重探: struct 定义段已可解析, 语料前进至 match 语句 (未实现) 处以新确定性失败拒绝 — stderr 由「错误 @ 25:1 — 顶层只允许…Some(Ident("struct"))」变为「错误 @ 34:21 — 无法开始一个表达式: Some(LBrace)」, exit 1 不变 | M23 的必然结果; 测试政策「探测后冻结」 | forward-spec 文档非行为契约 (spec-notes §五); 其余 5 个 demo 基线零改动通过 |

**终态验证**: golden 2 + aot_parity 5 + native_parity 3 + sema_laws 29 + smoke 8 +
struct_laws 22 全绿; demos/structs.as JIT 与 AOT 产物三元组逐字节一致;
`cargo build` 零警告。

## Phase 2b — result 枚举 + match 表达式 + ? 传播糖 + 字符串转义 (2026-08-25)

| # | 变更 | 依据 | 安全性 |
|---|------|------|--------|
| M26 | **result<T,E> 内建泛型枚举落地**: 类型槽接受 `result<T,E>` (恰两参, 递归校验); 构造器 `ok(expr)`/`err(expr)` 为类型构造器 (非名字分派函数, 被绑定遮蔽即普通调用 — 与 struct 分派镜像); 单侧推断 ok(e):result<typeof e, Unknown> / err(e):result<Unknown, typeof e>, 另一侧由声明上下文经 types_match 结构统一; match 表达式穷尽分臂 (恰一 ok + 恰一 err), 臂绑定 val 语义新作用域, 值 = 非 never 臂公共类型; `expr?` 传播糖仅限所在函数声明返回同型错误 result (P6); 字符串转义扩充 \r \" \0 (P8) | 用户批准 Phase 2b 设计 (spec-notes 附录四) | 全部为新发明检查/语法 — 只拒绝此前无法解析的程序, 不改变任何既有被接受程序的可观察行为。锁定于 tests/result_laws.rs 22 用例 (负向 14 断言精确中文消息+行:列, 正向 6 含值产出臂/never 流臂/? 快乐与穿透循环/转义往返/结构体载荷) |
| M27 | **文法使能**: match 臂间分隔符逗号可选 (`[,]?` — 换行亦可分隔, 尾逗号容忍); 调用实参尾逗号容忍 (file_wc.as 构造实参跨行书写带尾逗号, 不使能则语料卡在 match 臂体内 47:12 无法「越过 match」) | 用户批准的 match 文法参照 (file_wc.as 34-52 为冻结形状) | 纯增量接受 — 此前这些形状一律解析报错, 无既有语料依赖; golden/sema_laws/smoke/struct_laws 全部零改动通过 |
| M28 | **Q③ 终结性扩展**: 全 never 臂的 match 表达式语句等价 return 收尾 (每臂 return 即无落空路径) — file_wc.as count 函数体以 match 收尾是该设计的必然形状, 不扩展则规范 demo 本身被 Q③ 拒绝 | 用户批准设计「branch ending in return contributes never flow」+ Q③ | 只放宽此前误拒的空间: 旧规则下此类程序报「必须以 return 语句收尾」, 新规则接受; 锁定于 `result_laws::struct_payload_arms_and_never_tail` |
| M29 | **funclit 返回类型取声明侧词汇**: expected 已知时函数类型的 ret 用声明类型而非体推断值 — 否则 `ok(1)` 的单侧推断 (E=Unknown) 经函数签名外泄, 使调用点的 ? 同型检查被级联抑制 (other() 返回 result<i32,bool> 被吞成 Unknown) | D3 推断类型永不外泄 + M26 类型流完整性 | 修复 M26 引入的检查缺口; 对 Phase 2b 前类型集无观察差异 (无部分类型时推断==声明); 锁定于 `result_laws::propagate_error_type_mismatch_rejected` |
| M30 | **基线重探** (探测后冻结): file_wc.as 语料前进至 import 名解析 — stderr 由「错误 @ 34:21 — 无法开始一个表达式: Some(LBrace)」变为「错误 @ 34:10 — 未定义的绑定 'open'」(match/result/?/转义全部可解析, 标准库 Phase 5 前 open 未定义); recursion.as 前进至 P7 字面量模式 match — stderr 由「错误 @ 13:40 — 顶层只允许…」变为「错误 @ 14:4 — match 臂构造器必须是 ok 或 err, 实际 Some(Int(0))」(Phase 2b 仅 result 模式, P7 字面量/通配符模式未立案); 新增 demos/result_match.as 黄金基线 (stdout 15 行含 <ok>/<err>/转义字节, exit 9), JIT 与 AOT 产物三元组逐字节一致 (aot_parity 黄金基线 + native_pipeline 语料) | M23/M16 测试政策「探测后冻结」 | forward-spec 文档非行为契约 (spec-notes §五); 其余 demo 基线零改动通过 |

**终态验证**: golden 2 + aot_parity 5 + native_parity 3 + sema_laws 29 + smoke 8 +
struct_laws 22 + result_laws 22 全绿; demos/result_match.as 双形态逐字节一致;
`cargo build` 零警告。

## Phase 2c — 扩展方法 + self + 字符串内建 (2026-08-25)

| # | 变更 | 依据 | 安全性 |
|---|------|------|--------|
| M31 | **扩展方法落地**: 文法 `public? func <Ret> <RecvType>.<name> = (params) -> 体` (parser 单点路径名拆出接收者); 接收者 ∈ {string, bool, i32, 已登记结构体}; self 为隐式首参数 (val 语义, 不在参数表, 类型 = 接收者); sema 方法表按接收者类型划分命名空间 (签名先入表后查体 — 可递归); Expr::MethodCall 占位退役 → 静态分派 (元数不含 self, 实参逐位校验, 返回类型流入推断); public 标志解析并存储, 单编译单元内恒可调 (import 阶段翻转强制) | 用户批准 Phase 2c 设计 (spec-notes 附录五) | 全部为新发明检查/语法 — 此前 MethodCall 在 codegen 处以「尚未接入原生后端」拒绝, 无被接受语料依赖; 新诊断只作用于新语法空间。锁定于 tests/method_laws.rs 26 用例 (负向 14 断言精确中文消息+行:列, 正向 12 含引用别名穿透/self 插值洞/三重命名空间共存) |
| M32 | **内建字符串方法** len/upper/lower/trim 双后端同符号契约 (alias.str.len→I32 / upper·lower·trim→I64): len=字节长; upper/lower 仅 ASCII a-z/A-Z 平移; trim 剥离首尾空格+\t+\r+\n; JIT 宿主函数 + AOT IR shim (upper/lower 逐字节范围 icmp+select 循环; trim 双边界扫描+RtlMoveMemory 子块复制; 无 CRT); 空结果 data_ptr 恒 null (§五契约); 内建不可被用户方法覆盖 (「内建方法不可覆盖」) | 用户批准 Phase 2c 设计 | 全部为新发明能力 — 只接受此前无法编译的程序; 双后端逐字节 parity 由 aot_parity 机械枚举背书 |
| M33 | **基线重探** (探测后冻结): helper.as 方法可解析后, 语料前进至 main 存在性 — stderr 由「错误 @ 1:74 — 未定义的绑定 'self'」变为「找不到顶层 func main」(Q⑤ 无位置前缀), exit 1 不变 | M31 的必然结果; 测试政策「探测后冻结」 | forward-spec 文档非行为契约 (spec-notes §五); 其余 demo 基线零改动通过 |
| M34 | 新增 demos/methods.as 黄金基线 (stdout 13 行含内建往返/trim 边界/链式/结构体方法 var 字段变异, exit 0); aot_parity 语料机械入册 methods.as | M16 测试政策 | JIT 与 AOT 产物三元组逐字节一致 |

**终态验证**: golden 2 + aot_parity 5 + native_parity 3 + sema_laws 29 + smoke 8 +
struct_laws 22 + result_laws 22 + method_laws 26 全绿; demos/methods.as 双形态
逐字节一致; `cargo build` 零警告。

## Phase 2d — array<T> 数组类型 (2026-08-25)

| # | 变更 | 依据 | 安全性 |
|---|------|------|--------|
| M35 | **array<T> 内建泛型落地**: 类型槽接受 `array<T>` (恰一参, T 递归含 array<array<i32>>/结构体/string); 其余泛型仍按 Phase 5+ 拒绝; 字面量 `[e1, e2, ...]` 元素类型一致 (首元素定候选, 违规元素报「数组元素类型不一致: X 与 Y」, 空字面量元素类型 Unknown 由声明上下文统一); 下标读 `expr[i]` 真语义 (主语须 array → 「下标访问需要 array 类型, 实际 X」; 下标须 i32 → 「下标需要 i32, 实际 X」); `arr[i] = x` 解析层拒绝「下标赋值尚未支持」(只读索引裁决) | 用户批准 Phase 2d 设计 (spec-notes 附录六) | 全部为新发明检查/语法 — 此前 Index 为占位报错、数组字面量无法解析, 无被接受语料依赖。锁定于 tests/array_laws.rs 21 用例 (负向 9 断言精确中文消息+行:列, 正向 9 含增长/LIFO/别名/嵌套/结构体元素/闭包捕获, 运行时中止 3 走子进程 CLI) |
| M36 | **运行时表示与内建方法**: 实例 = 泄漏 24 字节头块 {data_ptr, len, cap} + n×8 元素缓冲 (空数组 data_ptr 恒 null, 镜像空串契约); 引用语义 (赋值/传参/捕获共享头块指针); push 满 len==cap 换新缓冲 (2x, 空 cap 取 1) RtlMoveMemory 复制旧元素, 头块原地更新 — 别名立即可见; pop LIFO; 内建 len/push/pop 编译器提供且用户不可定义 (接收者文法不含 '<'); 越界读/负下标/pop 空数组 → span-ID 中止存根 (「错误 @ L:C — 下标越界」/「— pop 空数组」, exit 1, 与除零同机制); 打印数组值 → 固定 `<array>`; 双后端同符号契约 alias.arr.new·len·push·pop / alias.abort_oob / alias.abort_pop / alias.display.array | 用户批准 Phase 2d 设计 | 全部为新发明能力 — 只接受此前无法编译的程序; 中止消息经子进程探测逐字节冻结 (array_laws bounds/negative/pop 三用例); 双后端 parity 由 aot_parity 机械枚举背书 |
| M37 | **基线重探** (探测后冻结): file_wc.as 语料不前进 — fail-fast 在 ok 臂 `.split/.map` 之前先撞上未解析 import 名 `open`, 基线维持「错误 @ 34:10 — 未定义的绑定 'open'」exit 1 逐字节不变; sema_laws/result_laws 中以 array 作「未实现泛型」例证的两用例轮换为 sender (法律本身不变: 非 result/array 泛型仍 Phase 5+ 拒绝); 新增 demos/arrays.as 黄金基线 (stdout 18 行, exit 0), native_parity/aot_parity 语料机械入册 | M23/M16 测试政策「探测后冻结」 | forward-spec 文档非行为契约 (spec-notes §五); 例证轮换不改变被测法律; 其余 demo 基线零改动通过 |

**终态验证**: golden 2 + aot_parity 5 + native_parity 3 + sema_laws 29 + smoke 8 +
struct_laws 22 + result_laws 22 + method_laws 26 + array_laws 21 全绿;
demos/arrays.as 双形态逐字节一致; `cargo build` 零警告。

## Phase 2e — 无括号文法泛化 (2026-08-25)

| # | 变更 | 依据 | 安全性 |
|---|------|------|--------|
| M38 | 表达式位置无括号: `ident unary` 吞参调用 (val a = dup 5); breaking: `f g` 现解析为方法中缀 f.g() — 函数值传参须显式 f(g) | 用户裁决 | 全套件重探, 语料零破坏 (无此形态) |
| M39 | 方法中缀: `expr Ident [unary]` → lhs.m([arg]), 左结合链; 内建名单 {println,print,increase,decrease} 的 Ident 实参优先吞参 (println a 不被劫持为方法) | 用户裁决 a.plus(b)≡a plus b | method_laws/array_laws 全绿回归 |
| M40 | hello_native.as 基线轮换: `println wrap 'yo'` 由「误打印 <func>」修复为真实调用 "[yo]" — 旧行为是静默 bug | P2e 修复价值实证 | 探测后冻结政策 |

## P3a 稳定性修复 (2026-08-26)

| # | 变更 | 依据 | 安全性 |
|---|------|------|--------|
| M41 | **统一 `alias.cell.new(bytes)` 双后端契约**：JIT 宿主与 AOT shim 均按调用端 `size_align` 给出的字节数分配清零存储区，初值继续由调用端按声明宽度写入；移除遗留的“固定分配 8 字节并把参数当初值”实现 | WER dump 在 `alias.str.concat` 读到被污染的 `counter.tag` 指针；AOT 反汇编确认 16 字节 `counter` 实例只分配 8 字节，字段 `tag@+8` 越界写堆，导致 `methods.as` 间歇性 `0xC0000005` | 修复未定义行为，不改变合法程序语义；确定性单元测试冻结参数语义，`methods.as` AOT 高频运行与全量测试连续 5 轮验证 |
| M42 | **Q④ 终裁：main 仅接受零参 `func i32`**；移除 bool/string/unit 退出映射，后端入口也做 i32 防御校验 | 用户终裁 | 非 i32 main 统一在 sema 阶段给出中文诊断；黄金记录覆盖 bool/string/unit 三种拒绝路径 |
| M43 | **类型流与混型 ABI 修复**：函数值携带完整参数/返回类型；名字类型查找与地址查找同为最内层优先；顶层重名绑定各占独立物理槽；f32 经 I32 位型后扩入 8 字节载荷字；match 臂绑定参与结果类型投影，return/join 做宽度桥接 | 审计发现全局按名字猜函数签名、遮蔽时地址/类型可命中不同层，以及 f32→i64 非法 bitcast | 消除窄单元格越界读写、GPR/XMM ABI 错配与 F32 result/match/array verifier 失败；定向回归覆盖全部路径 |
| M44 | **JIT 中止改为宿主可恢复错误**：除零、越界、空 pop、转换越界只记录首个 `AliasError`，生成代码逐帧零值退栈，`run()` 返回 Err；执行上下文串行化避免全局 span/error 串案 | `extern "C"` 宿主函数直接 `process::exit` 会杀死嵌入 Alias 的进程 | CLI 可观察 stderr/exit 不变；库调用在错误后仍能继续执行下一程序 |
| M45 | **运行时空值与 AOT shim 加固**：空字符串/trim/concat 与空数组复制路径不再对 null 做零长度指针运算或内存拷贝；HeapAlloc 失败显式 exit 1；补齐 i64/u64/f32/f64 双后端 display，并统一浮点规范化输出 | Rust `from_raw_parts(null,0)` 与 C/Windows 零长度 memcpy 的 null 前提不安全；宽整数/浮点符号契约此前不完整 | 空值回归、混型结构体 self ABI、宽数值和浮点 JIT/AOT 逐字节 parity 覆盖 |
| M46 | **不可信源码健壮性上限**：源码 8 MiB、token 200000、语法/类型/插值嵌套 128、表达式链 256；换行和一元链改迭代；整数解析 checked、科学计数法保留整数部并拒绝非有限值；i64 字面量不再先截成 i32 | 深递归可栈溢出，超长整数曾在 lexer 算术溢出，`1e5` 与宽整数被误解析/截断 | 全部失败均为中文 `AliasError`，不 panic；安全回归覆盖超深/超大输入与 2^31 以上字面量 |
| M47 | **链接和输出路径竞态修复**：临时 COFF 名含 PID+时间+原子序号并以 create_new 独占创建，RAII 覆盖所有失败路径清理；build 只接受 `.as` 输入，禁止输入与 `.exe` 输出自覆盖 | 同进程并发 build 旧名仅含 PID，会互相覆盖；以 `.exe` 为输入会覆盖源文件 | 32 路并发临时对象单测验证唯一与清理；CLI 回归验证非 `.as` 输入原字节不变 |
| M48 | **统一 ABI/布局层**：新增 `codegen/abi.rs`，`ValueAbi` 集中声明每种 VTy 的规范寄存器、存储类型/宽度/对齐、参数、返回和载荷字编码；用户直接/间接函数签名统一生成；结构体名字两阶段登记后集中计算字段偏移、最大对齐与尾随填充 | 后端曾在 mod/emit/funcgen 多处复制宽度与签名判断，新增类型时容易出现 load/store 与 GPR/XMM 漂移 | ABI 矩阵与混型布局单测冻结物理契约；调用模块只消费元数据，不再拥有独立宽度表 |
| M49 | **runtime 机器契约表**：新增 `RUNTIME_CONTRACTS`，逐符号声明参数、返回、可空性和 JIT/AOT 覆盖；调用点由表生成签名并核验实参类型；JIT host 注册集合、AOT shim 实际定义集合分别与契约表做精确相等校验 | host 注册、调用点与 shim 三方手写签名会静默漂移；文档中的 arr.new 已落后于双参数实现 | 缺失、重复、多余、后端缺席或实参类型漂移在构建/测试时失败；修正文档 `arr.new(cap,elem_size)` |
| M50 | **破坏性语料扩充与传递捕获修复**：固定种子生成 160 个深度 6 的 wrapping i32 AST；覆盖全整数宽度边界、三组混型布局排列、32 层 JIT/16 层 AOT 闭包、16 路并发 JIT；同一 methods AOT 产物由 30 次提高到 100 次重复执行 | 正常 demo 难覆盖组合态；深层闭包测试实证 `scan_captures` 只识别父局部、不把父 env 捕获继续传给子闭包 | 捕获扫描同时识别 `frame.caps`，祖先单元格可逐层透传；随机种子固定，失败可复现；AOT/JIT 边界输出逐字节比较 |

**终态验证**：180 个测试连续 5 轮全绿；每轮都把同一个 `methods.as` AOT
产物重复执行 100 次并逐字节校验 13 行输出、空 stderr 与 exit 0；另有
固定种子随机 AST、全整数宽度边界、混型布局排列、32 层 JIT/16 层 AOT
传递捕获、16 路并发 JIT、F32 result/match/array、链接临时文件竞态和输入
覆盖保护定向回归。`cargo check` 与 `cargo clippy --all-targets` 均成功
（仓库仍有既存 clippy 风格警告）。

## Phase 7 — 唯一原生编译管线 (2026-08-27)

| # | 变更 | 依据 | 安全性 |
|---|------|------|--------|
| M51 | **物理删除进程内代码执行后端**：移除 `cranelift-jit`、`codegen/host.rs`、`codegen::execute`、全局 span/error 槽、执行串行锁和 `alias.runtime.failed`；`Compiler` 不再携带后端模式位 | 用户终裁：Alias 只有完整编译行为，不允许进程内执行生成代码 | Cargo 依赖树和 `src/` 均无进程内执行入口；runtime 契约由唯一原生实现精确覆盖 |
| M52 | **run 改为完整编译工作流**：`run(path,src)` 与 `build` 共用 `lex → parse → sema → COFF → rust-lld`，前者把产物放入唯一临时目录、启动独立 exe 后清理；运行时错误由子进程打印并以 1 退出 | `run` 只是便捷命令，不得成为第二执行后端 | 缺失链接器时 `run` 必须失败的架构门禁阻止旁路；并发编译以独立临时目录和链接器独占目标文件隔离 |
| M53 | **术语和测试收敛**：`aot_shim.rs` 更名 `native_runtime.rs`，`aot_parity.rs` 更名 `native_pipeline.rs`；对照对象改为 run 临时产物与 build 持久产物，同一 methods 原生产物仍高频执行 100 次 | 删除双形态后继续保留旧名会误导维护者，也无法证明 run 真的走链接 | 源码、测试、Cargo 清单和当前规范不再含双后端分支或宿主 runtime；黄金三元组保持不变 |

**终态验证（2026-08-27）**：179 个测试连续 5 轮全绿；每轮均验证 `run`
缺失链接器时失败、run/build 原生产物逐字节一致、16 路并发完整编译及同一
`methods.as` exe 重复执行 100 次。`cargo check`、`cargo clippy --all-targets`
和依赖/符号静态审计均通过；Clippy 仅保留非阻断风格警告。

## 控制流 / iterator / 运算方法收口（2026-08-27）

- `func` 收紧为函数字面量 RHS；非 unit 返回检查升级为所有可达路径显式 return。
- 新增 `if/else if/else`、短路 `&&/||`、三元 `?:`、`break/continue`。
- `for` 改为 `for Type name in Expr`，仅作集合迭代；旧 condition-for 与 C 风格 for 退役，条件循环统一归 `while`。
- `iterator<T>` 落地；array iterator 使用共享结构版本号进行别名可见的 fail-fast 失效检测。
- 数值内建 `plus/minus/times/div` 与符号运算共用 lowering，`bool.not` 与 `!` 共用取反路径。
- 赋值/字段赋值补齐静态类型一致性；恢复数组和方法领域诊断的既有精确文案/span。
- CLI 缺文件错误改由 Alias 生成确定性中文文本，避免宿主 Windows UI 语言改变黄金字节。
- 新增 `tests/control_flow_operator_laws.rs`，覆盖上述控制流、iterator 与运算函数契约。

## Pattern / 整数运算 / pub 收口（2026-08-27）

- `Pattern` 从 result-only foundation 扩展为 `_`、普通标识符绑定、整数/bool/纯字符串字面量、`ok(name|_)` / `err(name|_)`；`match` 主语不再限 result，统一执行类型适配、重复覆盖、穷尽性与不可达检查。
- `_` 与普通标识符都是 catch-all；普通标识符以不可变 `val` 绑定整个主语。bool 可由 `true + false` 穷尽，result 可由 `ok + err` 穷尽，整数/string 等开放域必须有 catch-all。
- 新增 `%` 与 `& | ^ ~ << >>`；`%` 仅整数，位运算/移位要求同型整数；有符号/无符号 `>>` 分别为算术/逻辑右移；复合赋值仍未加入。
- 整数目标槽传播覆盖新运算表达式，避免窄整数/无符号声明中的字面量退回 `i32`；变量间仍禁止隐式数值混算。
- `public` 关键字物理退役；唯一公开可见性关键字为顶层 `pub`。不保留兼容 token、别名或迁移诊断；历史记录中旧 `public` 文法描述保留为当时事实。
- 原 result-only 的两条 match 法律更新为一般 Pattern 法律；demos/tests 当前语料迁移到 `pub`。
- 扩展 `tests/pattern_laws.rs`，新增 `tests/operator_pub_laws.rs`。本批验证仍需显式手动运行 `cargo test --all-targets`，不使用 CI。

## 泛型右尖括号 / 目标类型诊断 / 自增减收口（2026-08-27）

| # | 变更 | 依据 | 安全性 |
|---|---|---|---|
| M54 | 类型解析上下文把 lexer 的 `Shr` 拆为两个 `Gt` 消费，恢复无空格嵌套泛型 `array<array<i32>>`；表达式 `a >> b` 仍保留右移语义 | 新增 `>>` 后 lexer 最长匹配破坏既有递归泛型文法 | `array_laws::nested_arrays`、arrays demo 黄金基线及 run/build 全语料一致性共同锁定 |
| M55 | 数值目标类型只向字面量传播，不把已有变量改型；同型但不支持的运算符走“不适用于”诊断，异型数值才报“禁止隐式混算” | 目标槽传播不得抢先覆盖二元运算自身的语言诊断 | 原有四条运算符法律恢复精确消息；窄整数/无符号字面量采用声明类型的正向法律保持 |
| M56 | `increase/decrease` 从 i32-only unit 表达式收紧为独立语句，支持全部整数与浮点数值绑定；整数按声明宽度 wrapping ±1，浮点同型 ±1.0，任何值位置拒绝 | 用户裁决：语义是对可变数值绑定自增/自减 1，不能赋值给其它 ident | 新增全数值 ABI/边界正向用例与 statement-only 负向用例；既有 i32 程序行为不变 |
| M57 | `recursion.as` 黄金基线重探：字面量 Pattern 已落地后，语料前进至未实现的 `this` 当前函数自引用，stderr 更新为「错误 @ 15:13 — return 需要 i32: 未定义的绑定 'this'」 | M54 恢复 arrays demo 后，全语料机械枚举首次继续执行到该历史陈旧基线 | forward-spec demo 仍确定性拒绝、exit 1；只修正当前事实记录，不实现或发明 `this` 语义 |

## 整数算术溢出退役 wrapping（2026-08-27）

| # | 变更 | 依据 | 安全性 |
|---|---|---|---|
| M58 | 废除整数 `+/-/*`、一元负号与 `increase/decrease` 的 wrapping 结果；所有有/无符号宽度改为 checked 运算，越界经表达式 Span 输出「整数溢出」并退出 1。`INT_MIN / -1` 从旧「除以零」通道分离到溢出通道 | 用户最新裁决：任何整数算术结果超过声明类型宽度必须报错 | 八种宽度及加减乘、一元负号、自增减、除法极值负向矩阵；递归 `u32` 13 阶乘回归；安全边界与固定种子非溢出 AST 正向矩阵；run/build 三元组一致 |

## 转换 / u64 字面量 / this / 左移收口（2026-08-27）

| # | 变更 | 依据 | 安全性 |
|---|------|------|--------|
| M59 | 旧 `to_*` 数值内建物理退役；显式目标改为 `(T) value`，上下文目标改为 `from(value)`，不存在转换关系时允许保留源类型的形式为 `try_from(value)`。当前只定义数值族转换；所有整数目标转换按值域检查，`try_from` 对已存在关系的运行时越界不回退 | 用户终裁的三种目标类型来源与 fallback 边界 | 正负法律覆盖括号/无括号形式、无上下文、无转换关系、静默源类型回退、整数窄化和旧入口删除 |
| M60 | lexer/AST 整数字面量载荷从 i64 扩至 u64，目标槽与 Pattern 用符号+幅度检查；无上下文正字面量按 i32→i64→u64 推断，负字面量仍以 i64 为上限 | 旧 i64 token 无法直接表达 u64::MAX，借 `to_u64(-1)` 绕行违反无回绕与新转换契约 | u64::MAX 原生显示/比较/减法、u64 上溢词法错误、全既有宽度边界测试 |
| M61 | `<<` 加入声明宽度 checked 语义，丢失有效位或移位数达到类型宽度均报「整数溢出」；`& | ^ ~ >>` 保持位模式语义 | “超过声明类型宽度必须报错”同样约束左移结果 | 有符号、无符号和非法移位数负向矩阵；安全边界沿用运算符法律 |
| M62 | `this` 成为 func 体内当前函数的不可变自引用，携带完整函数签名与当前 closure env；嵌套 func 各自重绑定，func 外拒绝。`recursion.as` 从历史拒绝基线升级为原生成功黄金记录 | demo 中已定稿但实现长期缺失的当前函数绑定语义 | 改名不掉链递归、嵌套递归、体外拒绝与 demos 机械黄金基线 |
