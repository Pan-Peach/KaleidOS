# resource（os/core/src/resource/）

> 设备 / IRQ route / DMA allocation·mapping 的**归属记账**：裸机程序自己无法知道"这台设备归谁、这条中断线归谁、这段 buffer 映射给哪台设备"。
> **这不是 capability 系统**：`RequestContext` 是执行归属 + 生命周期所有权，不是 security principal。

## owns 什么真相

- 设备所有权与 quarantine：谁认领了哪个 `DeviceId`（独占锚在 device 记录 index），失败后哪些设备被隔离到 reboot。
- IRQ route：哪条线归哪个 owner、回调与 ctx（锚点 = 已认领 `DeviceId`）。
- DMA：allocation（device-agnostic）与 mapping（device-related）的拥有关系；mapping id 单调递增、从不复用。
- 调用归属：`RequestContext` 解析"这次 Core 调用是替哪个组件做的"（最内层活动的 Core-managed 执行边界）。

## 暴露什么机制

- `init()`；重导出 `ResourceKind`（Device / Irq / Dma）与 `RequestContext`。
- `RequestContext::ambient()` / `ambient_init()`。
- `device`：`DeviceTable`、`DeviceMapping`、`DeviceClaimError`、`DeviceReleaseError`；`claim` / `release` / `quarantine_owner` / `owner_of`。
- `irq`：`IrqTable`、`IrqError`、`IrqHandler`；`register` / `enable` / `disable` / `release` / `revoke_owner`。
- `dma`：`DmaTable`、`DmaDirection`、`DmaError`、`DmaMapping`、`DmaBuffer`；`alloc` / `free` / `map` / `unmap` / `revoke_owner` + 私有 `QUARANTINE`。

## 明确不做

- **不做 per-access 鉴权**：`kcore_device_claim` 之后 driver 直接拿裸 MMIO 指针，稳态不再进 Core（KernelNative 就是可信代码，见 `docs/architecture/driver-model.md` §1.1）。
- 只强制**正确性**不变式：设备独占、失败 quarantine、拆机顺序（仍有 live IRQ/DMA → `-EBUSY`）。撤销在 KernelNative 是协作式的。
- 不记堆内对象 / 堆字节（per-instance `HeapState` 由 runtime 拥有），**也不做内存记账**：无 region owner 记录、无 region 账本，`resource` 下没有 memory 模块（见 `docs/architecture/memory-and-heap.md`）。ResourceDomain 不是 struct 而是一个视图。
- 不碰中断控制器寄存器（那在 arch）；`irq` 模块只管投递（见 [`irq.md`](irq.md)）。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/resource/mod.rs` | `ResourceKind`、`init`、模块边界文档 |
| `os/core/src/resource/context.rs` | `RequestContext`、`ambient()` / `ambient_init()` |
| `os/core/src/resource/device.rs` | `DeviceTable`（owner + quarantine）、`claim` / `release` |
| `os/core/src/resource/irq.rs` | `IrqTable`（device-anchored）、`register` / `enable` / `disable` / `release` |
| `os/core/src/resource/dma.rs` | `DmaTable`（allocations + mappings）、`QUARANTINE` |
