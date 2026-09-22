# irq（os/core/src/irq/mod.rs）

> **中断投递侧**：外部中断进入 Core 的入口，以及"关中断临界区"原语。
> IRQ 的**归属真相**（route / owner / 回调）在 `resource::irq`；本模块只有投递与临界区。

## owns 什么真相

- 外部中断到 Core 的投递路径（trap → `on_external` → 按线号 route）。
- `IrqSaveGuard`：进入临界区时保存 / 恢复中断状态的机制。

## 暴露什么机制

- `on_external()`：外部中断入口。
- `route(number)`：按中断号投递到已注册 route。
- `init()`。
- `IrqSaveGuard`。

## 明确不做

- **不拥有 route 真相**：route / owner / handler / ctx 在 `resource::irq` 的表中。
- **不碰中断控制器寄存器**：PLIC 等由 `arch` 的 `InterruptController` 负责；Core 不直接写 PLIC。
- 回调通过 `containment::with_irq_scope` 建立 IRQ 归属作用域（principal = 该线 owner、`task = None`）；作用域同步、不可 yield，回调内 panic 致命。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/irq/mod.rs` | 投递入口、`route`、`IrqSaveGuard`、IRQ 作用域接线 |
