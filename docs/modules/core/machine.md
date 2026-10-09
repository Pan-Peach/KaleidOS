# machine（os/core/src/machine.rs）

> **已提交的机器事实**（`MachineInfo`）+ **纯设备发现**。
> Core 不知道这些事实来自 FDT 还是 ACPI——boot 把 backend 归一化成 `MachineInfo` 后交给它；
> 原始固件描述本身不再被丢弃：`firmware` 字段保留**已校验的来源位置**（见下）。

## owns 什么真相

- 已提交的归一化机器信息：`static COMMITTED`（RAM、CPU 清单、设备描述符）。monitor 与导出表共用这一份快照。
- **保留的原始固件描述源**（`MachineInfo.firmware` / `FirmwareInfo`）：`Fdt { phys, size }` / `Acpi { rsdp }` / `Static`。只记"字节在哪、有多大"，**不认证内容**；下游表（RSDT/XSDT/…）由未来消费者读取时各自校验。保留字节不回收（所在区间由 boot 的 arena 选择永久排除）。
- **timebase 速率是显式 optional**（`timebase_frequency: Option<NonZeroU64>`）：`Some(non-zero)` = 已发现速率，`None` = **未知**（不是 0 约定，也不伪造常量）。消费者各自处理未知：`timer::init_preempt` fail-closed、`print::idle_wait` 回退轮询、组件导出 `kcore_timebase_hz` 映射为 0。
- `core::init` 在 `commit` 前拒绝形状非法的固件源（零地址 / 零长度）——绝不以 `Static` 之外的形式发布悬空固件根。
- `commit(info)` 由 `core::init`（`lib.rs`）校验通过后调用一次；第二次发布被拒绝。

## 暴露什么机制

- 类型：`MachineInfo`、`FirmwareInfo`、`CpuId`、`CpuInfo`、`MemoryRegion`、`DeviceDescriptor`（`spaces: Box<[IoSpace]>` 全部窗口、固件顺序、`spaces[0]` = 主窗口；`interrupts: Box<[InterruptResource]>` 完整中断资源；`compatibles: Box<[Box<str>]>` 完整 compatible，数量与长度都不截断）、`IoSpace`（Mmio / Pio）、`DeviceId`、`DeviceLookupError`、`InterruptResource` / `InterruptSpecifier`（设备的完整中断资源：固件 specifier + 可投递的逻辑 `line: Option<u32>`）。
- `commit(info)` / `committed()`。
- `nth_compatible(compatible, ordinal)`：纯设备枚举（含已认领设备，顺序跨 claim/release 稳定；`ordinal` 越界 → `DeviceLookupError`）。
- `component/export/query.rs` 的 `kcore_device_info` 把已有设备描述与 DeviceTable 的 owner / quarantine 投影为只读值；发现本身不变，不新增节点命名或另一套 ownership 账本。ABI 布局以 `abi/core.toml` 为准，语义见 [驱动契约](../../architecture/driver-model.md#121-已决设备选择原-q1)。

## 明确不做

- **不解析 DTB / ACPI**：FDT / RSDP 解析与校验在 boot；Core 只收已归一化的值与已验证的保留位置。
- **`FirmwareInfo` 不是 ABI**：不是 `repr(C)`、不进 `kcore_*` / 组件 SDK；`Static` = 没有保留的受支持固件描述（不是"校验失败但继续"）。
- **发现是纯的**：不分配、不触碰设备寄存器、不读 claim 状态、不授权。
- **不把固件解码值当逻辑 IRQ 号**：`specifier`（控制器 + 完整 cells）与 `line` 分开；`line` 的绑定是 boot 的 arch 特定工作（PLIC），Core 只消费。
- `DeviceId` 是 **identity，不是权限**；零可以是合法值。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/machine.rs` | `MachineInfo` 及子类型、`commit` / `committed`、`nth_compatible` |
