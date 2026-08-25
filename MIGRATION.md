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
| 版本 | cranelift-{codegen,frontend,jit,module,native} = **0.135.0** (crates.io 最新稳定, 五包同轨发布), Cargo.lock 锚定版本+哈希 |
| 拥有者 | `src/codegen.rs` 唯一触点 cranelift-* API — ast/sema/interp/lib 零 cranelift 类型 (已核验) |
| 来源 | crates.io 经 rsproxy 镜像; 默认 features, 无 feature 传递引入 wasm/其他后端 |
| 安全边界 | 进程内 JIT 执行本地生成代码; 无网络/文件系统能力引入; 宿主函数仅 print/abort/槽区分配 |
| 演练路径 | `cargo run -- --backend native demos/hello_native.as` 端到端即真实产品路径 |

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
| M30 | **基线重探** (探测后冻结): file_wc.as 语料前进至 import 名解析 — stderr 由「错误 @ 34:21 — 无法开始一个表达式: Some(LBrace)」变为「错误 @ 34:10 — 未定义的绑定 'open'」(match/result/?/转义全部可解析, 标准库 Phase 5 前 open 未定义); recursion.as 前进至 P7 字面量模式 match — stderr 由「错误 @ 13:40 — 顶层只允许…」变为「错误 @ 14:4 — match 臂构造器必须是 ok 或 err, 实际 Some(Int(0))」(Phase 2b 仅 result 模式, P7 字面量/通配符模式未立案); 新增 demos/result_match.as 黄金基线 (stdout 15 行含 <ok>/<err>/转义字节, exit 9), JIT 与 AOT 产物三元组逐字节一致 (aot_parity 语料机械入册 structs.as + result_match.as) | M23/M16 测试政策「探测后冻结」 | forward-spec 文档非行为契约 (spec-notes §五); 其余 demo 基线零改动通过 |

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
