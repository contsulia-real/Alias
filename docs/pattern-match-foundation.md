# Pattern AST + Match 第一批正式语义

日期：2026-08-27

## 当前冻结状态

`match` 的每个臂持有独立 `Pattern` AST 节点。`MatchArm` 只负责 pattern + arm body + arm span，不再直接拥有构造器或绑定状态，也不存在兼容 `Deref` 层。

第一批公开 Pattern：

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

其中：

- `_`：匹配任意值，不创建绑定；
- 任意合法标识符：匹配任意值，并以不可变 `val` 语义绑定整个主语；
- 整数字面量：仅适用于整数主语，并按主语静态整数类型执行范围检查；
- `true` / `false`：仅适用于 `bool`；
- 纯字符串字面量：仅适用于 `string`；插值字符串不是字面量 Pattern；
- `ok(name|_)` / `err(name|_)`：仅适用于 `result<T, E>`，可绑定或丢弃对应 payload。

## 穷尽性与不可达

- `_` 和普通绑定 Pattern 都是 catch-all；其后的 arm 不可达；
- `bool` 可由 `true` + `false` 穷尽，也可由 catch-all 穷尽；
- `result<T, E>` 可由 `ok(...)` + `err(...)` 穷尽，也可由 catch-all 穷尽；
- 整数、字符串等开放取值域不能靠有限字面量列表证明穷尽，必须提供 `_` 或普通绑定 Pattern；
- 重复字面量 Pattern、重复 `ok` / `err` 均为编译错误；
- 一旦前序 Pattern 已覆盖全部剩余取值，后续 arm 为编译错误。

`match` 不再限定主语必须为 `result`。Pattern 是否适用于某个主语类型由统一 sema 检查决定。

## 仍未加入

本批不加入：

- match guard；
- struct Pattern；
- 嵌套 constructor payload Pattern；
- 用户自定义 Pattern 构造器。

这些能力以后继续扩展同一个 `Pattern` AST 和统一 coverage 检查，不得重新把类型特例塞回 `MatchArm`。
