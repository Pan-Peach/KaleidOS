# machine（os/core/src/machine.rs）

> **已提交的机器事实**（`MachineInfo`）+ **纯设备发现**。
> Core 不知道这些事实来自 FDT 还是 ACPI——boot 把 backend 归一化成 `MachineInfo` 后交给它。

## owns 什么真相

- 已提交的归一化机器信息：`static COMMITTED`（RAM、CPU 清单、设备描述符）。monitor 与导出表共用这一份快照。
- `commit(info)` 由 `monitor::mount` 在 `core::init` 末尾调用（`machine::commit` 不在 `lib.rs` 里直接调）。

## 暴露什么机制

- 类型：`MachineInfo`、`CpuId`、`CpuInfo`、`MemoryRegion`、`DeviceDescriptor`、`IoSpace`（Mmio / Pio）、`CompatStr`、`DeviceId`、`DeviceLookupError`。
- `commit(info)` / `committed()`。
- `nth_compatible(compatible, ordinal)`：纯设备枚举（含已认领设备，顺序跨 claim/release 稳定；`ordinal` 越界 → `DeviceLookupError`）。

## 明确不做

- **不解析 DTB**：FDT 解析在 boot；Core 只收已归一化的值。
- **发现是纯的**：不分配、不触碰设备寄存器、不读 claim 状态、不授权。
- `DeviceId` 是 **identity，不是权限**；零可以是合法值。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/machine.rs` | `MachineInfo` 及子类型、`commit` / `committed`、`nth_compatible` |
