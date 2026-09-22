# inspector（os/core/src/inspector/）

> **只读观察门面**（`TestInspector`）：CoreTest / 调试用，只返回值拷贝，**无 god-mode**。
> `Inspector` 是 ZST，构造子是 `pub(crate)`——外部无法凭空拿到它。

## owns 什么真相

无。它是观察口，不持有状态，也不修改状态。

## 暴露什么机制

- `Inspector::task(TaskId) -> Option<TaskSnapshot>`
- `Inspector::component(ComponentId) -> Option<ComponentSnapshot>`
- `Inspector::component_image(ComponentImageId) -> Option<ImageSnapshot>`
- `Inspector::memory_region(base) -> Option<MemoryRegionSnapshot>`
- `Inspector::visit_trace_since(seq, visitor)`
- 快照类型：`TaskSnapshot`、`ComponentSnapshot`、`ImageSnapshot`、`MemoryRegionSnapshot`。

## 明确不做

- **不改状态**：不写 Core 私有状态、不发句柄、不强制调度、不打补丁。
- 绝不泄漏 `&mut` 或内部引用——只返回拷贝。
- CoreTest 必须和普通组件一样受限：若测试组件能直接改 Core 私有状态，"Core 不可破坏"就没被真正验证过。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/inspector/mod.rs` | `Inspector`（ZST）与查询方法 |
| `os/core/src/inspector/snapshot.rs` | `TaskSnapshot` / `ComponentSnapshot` / `ImageSnapshot` / `MemoryRegionSnapshot` |
