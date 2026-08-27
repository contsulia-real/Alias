# Pattern AST + Match Foundation

日期：2026-08-27

## 当前冻结状态

`match` 已不再把 result 构造器和绑定名直接存放在 `MatchArm` 上；每个 match 臂持有独立的 `Pattern` AST 节点。

当前公开语法**没有扩大**，仍只接受：

```alias
match value {
    ok(v) -> ...
    err(e) -> ...
}
```

`Pattern` 当前承载：

- result constructor：`ok` / `err`；
- 臂内不可变绑定名；
- pattern 自身 span。

`MatchArm` 只负责 pattern + arm body + arm span，不再自己拥有构造器/绑定状态，因此不存在两份 pattern 状态。

## 兼容语义

本次是 AST/编译器结构迁移，不改变已接受程序的可观察行为：

- `match` 主语仍必须是 `result<T, E>`；
- 必须恰好覆盖一个 `ok` 和一个 `err`；
- 重复 `ok` / `err` 继续报原有诊断；
- `ok(name)` / `err(name)` 的绑定仍是 arm-local `val`；
- match 值类型、never/return 臂、`?` 传播规则保持不变；
- `some(x)` 等其它 constructor pattern 仍被拒绝，错误文本保持现有契约。

## 后续扩展边界

未来若增加 wildcard、literal、struct 或其它 constructor pattern，应扩展 `Pattern` 节点和统一 coverage 检查，而不是重新把 pattern 语义塞回 `MatchArm` 或为每种类型增加独立 match AST。

这次不引入这些新语法，也不提前定义其穷尽性规则。
