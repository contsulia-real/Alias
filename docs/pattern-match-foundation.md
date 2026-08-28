# Pattern AST + Match 当前语义

> 本文是 Pattern 专题说明；总规范以 `docs/spec-notes.md` 为准。这里仅描述当前有效状态，历史演进只记录在 `MIGRATION.md`。

## 1. AST / HIR 边界

parser 的每个 match arm 持有独立 `Pattern` AST。`MatchArm` 不保存 result-only 的构造器特例，也不存在兼容 `Deref` 层。

进入 sema 后，Pattern 的类型适配、绑定、覆盖关系、穷尽性和不可达检查统一完成；成功后的 `CheckedProgram` typed HIR 已固化主语/臂表达式最终静态类型。codegen 不得重新根据 Pattern 文本猜类型。

## 2. 当前公开 Pattern

```alias
match value {
    _ -> ...
    name -> ...
    0 -> ...
    true -> ...
    'text' -> ...
    ok(v) -> ...
    err(_) -> ...
}
```

语义：

- `_`：匹配任意值，不创建绑定；
- 普通合法标识符：匹配任意值，以不可变 `val` 绑定整个主语；
- 整数字面量：仅适用于整数主语，并按主语静态整数类型检查字面量范围；
- `true` / `false`：仅适用于 `bool`；
- 纯字符串字面量：仅适用于 `string`；含插值的字符串不能做 Pattern；
- `ok(name|_)` / `err(name|_)`：仅适用于 `result<T,E>`，名字绑定对应 payload。

`match` 主语不限于 `result`。Pattern 是否适用于主语由统一 sema 检查决定。

## 3. 穷尽性与不可达

- `_` 与普通绑定 Pattern 都是 catch-all；
- catch-all 后的 arm 不可达；
- `bool` 可由 `true + false` 或 catch-all 穷尽；
- `result<T,E>` 可由 `ok(...) + err(...)` 或 catch-all 穷尽；
- 整数、字符串等开放取值域不能靠有限字面量穷尽，必须提供 catch-all；
- 重复字面量 Pattern 编译错误；
- 重复 `ok` / `err` 编译错误；
- 前序 Pattern 已覆盖全部剩余取值后，后续 arm 编译错误。

普通标识符绑定整个主语，而 `ok(name)` / `err(name)` 只绑定对应 payload。

## 4. match 结果与控制流

match 的产生值臂必须统一到共同静态类型，并接受外层目标类型向各产生值臂递归传播。

`-> return expr` 或以 return 终止的块臂属于 never 控制流；全部 arm 都直接返回的 match 可以满足非 unit 函数的必返回要求。

`expr?` 使用同一 result 语义：成功路径得到 payload，错误路径要求与当前函数声明的 result 错误类型一致并提前返回。

## 5. 当前仍未实现

- match guard；
- struct Pattern；
- 嵌套 constructor payload Pattern；
- 用户自定义 Pattern 构造器。

未来若扩展，必须继续进入统一 `Pattern` AST / sema coverage 模型，不得重新把类型特例塞回 `MatchArm` 或 codegen。

## 6. 回归锚点

主要法律：

- `tests/pattern_laws.rs`
- `tests/result_laws.rs`
- `tests/function_value_laws.rs`（match 产生函数值并直接调用）

历史演进见 `MIGRATION.md`；当前语义以 `docs/spec-notes.md` 为准。
