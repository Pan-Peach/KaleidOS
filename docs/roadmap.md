# 路线图（roadmap.md）

## 1. 总体阶段

```text
v0（当前）：kaleidos.elf 单镜像（bootstrap + core 链接，职责分离装载合一）
  ↑ bootstrap 阶段：firmware 世界（FDT/MachineInfo）；core 阶段：KaleidOS 世界（资源真相）。
    Bootstrap 与 Core 都只启动一次、永不热替换 → 不放独立装载边界。
    Cargo 依赖图 ≠ 运行时组件图：组件层的 .kcomp/cpio/manifest（Linux insmod 模式）是未来方向。
之后：     组件动态加载（.kcomp）→ 组件持久化（Persistent Store）→ Wasm 执行后端 → IPC/隔离 → 多 profile → 多架构 → 热替换
```

**v0 的野心很小**：不是"功能完整"，而是"架构骨架立起来，边界验证舒服"。
第一阶段最重要的四件事：

1. Core 的最小词汇表
2. ResourceDomain
3. Policy → Core 验证路径
4. 静态 Component Graph

## 2. M0 —— Boot（当前目标）

```text
QEMU RISC-V 启动
  → bootstrap 阶段（kaleidos.elf）：early console（_start 接收 a0=hartid / a1=dtb）
  → Machine Discovery（FDT backend）：解析 DTB → MachineInfo（内存映射/CPU 清单）
  → core::init(&MachineInfo) → Core 初始化（校验 → 提交资源真相）
  → BOOT DISCOVERY OK / BOOT CORE OK
```

**验收标准**：结构化启动日志，四行全 OK：

```text
BOOT ARCH_ENTRY OK
BOOT DISCOVERY OK backend=fdt
BOOT MEMORY OK
BOOT CORE OK
```

**打包方式（组件层面，Linux 模式，既定方向）**：

```text
kaleidos.elf = bootstrap + core（链接，firmware 能启动的外壳：普通 ELF，entry=_start）
                 └ .initpkg（opaque blob，KaleidOS 自己解析，firmware 不理解）
                    → cpio 归档（initramfs 模式）：组件 .kcomp + manifest（文本）
组件 .kcomp = ELF 可重定位文件 + 符号表（.ko 模式：insmod = 放段+重定位+调 init）
manifest   = 文本清单（modules.dep 模式：depmod 生成 / modprobe 读取）
热替换 ≠ 永久安装：embedded init.kpkg（fallback）→ Persistent Store（用户安装）
                → Runtime Graph（真正在跑），manifest 决定选择
开发/发布：开发分开（kaleidos.elf + 外部 init.kpkg）；发布内嵌（重打包 → 单文件）
```

## 3. M1 —— 最小 Resource Core

实现最小的核心词汇表：

```text
TaskId        —— 任务身份
FrameId       —— 物理帧身份
ComponentId   —— 组件身份
Handle        —— 不可伪造的授权（类型化，如 FrameHandle）
ResourceDomain—— 组件资源集合（拥有什么、如何回收）
```

**验收标准**：host test 覆盖上述类型的创建/存在性/所有权语义；CoreTest 能在 QEMU 上跑基础断言。

## 4. M2 —— 第一批 Component

实现：

```text
RR Scheduler      （轮转调度器）
Simple Allocator  （简单帧分配器）
Logger            （日志组件）
CoreTest          （核心测试组件）
```

**验收标准**：完整走通 **propose → validate → commit** 链路：

```text
Scheduler 提议运行 Task #7  →  Core 验证（存在/Runnable/未在别 CPU）→ commit
Allocator 提议 Frame #100   →  Core 验证（存在/空闲/权限）→ commit ownership
```

对抗性测试开始建立：double free、wrong owner、invalid scheduler proposal 全部被 Core 拒绝。

## 5. M3 —— 静态 Component Graph

支持最简单的组件图操作：

```text
provides / requires / bind / start / stop
```

- 组件注册是**静态的**（代码里声明，不做动态 ELF / Wasm）；
- 生命周期状态机（Declared → Resolved → Starting → Ready → Quiescing → Stopped → Destroyed）落地；
- Ownership Tree 与 Dependency DAG 两套关系分开维护。

**验收标准**：一个配置好的 minimal profile 能按声明完成 bind → start → stop → destroy 全流程，资源完整回收。

## 6. M4 —— 第一个 Device Component

建议选择：**UART** 或**简单 VirtIO block**。

```text
Core Resource Authority（grant MmioHandle / IrqHandle / DmaHandle）
        ↓
Driver Component（驱动组件）
        ↓
Device Interface（如 BlockDevice / UART 设备）
```

**验收标准**：上层组件只通过 Interface 使用设备，从不接触裸地址/裸 IRQ；
驱动可以被 stop → 回收 → 重新 start。走到这里如果边界仍然舒服，说明架构基本成立。

## 7. 第一阶段明确不做（务必遵守）

```text
✗ 真正动态加载          ✗ 真正 hot migration
✗ 复杂 IPC              ✗ 微内核模式
✗ Wasm runtime          ✗ WIT / IDL
✗ 完整 capability 系统  ✗ 完整 POSIX
✗ Linux syscall 兼容    ✗ 复杂 VFS
✗ 复杂 SMP scheduler    ✗ 形式化证明
✗ 完整 driver framework ✗ 完整 dependency solver
```

> 这些属于后续实验。第一阶段目的是**让架构骨架立起来**，不是功能完整。

## 8. M0 之后的方向（v0 之后，按兴趣与需要选择）

- **动态组件**：运行时加载/卸载组件（ELF 或未来 Wasm）—— 需要先有稳定的静态图做基准；
- **Wasm 执行后端**：`scheduler.wasm`、`filesystem.wasm`、`game.wasm` 作为 Component 的一种执行方式（Core/Arch 保持 native）；
- **IPC / 隔离**：UserAddressSpace 执行域，微内核形态 profile；
- **多 profile**：game、unix（POSIX personality）、micro、debug 等；
- **多架构**：x86_64 → aarch64 → loongarch64（Arch 层换实现，Core 不动）；
- **热替换**：在 Phase-1 替换模型（quiesce → stop → unbind → reset → replace → bind → start）基础上，向无感替换演进；
- **验证工具链**：Kani / Loom / Miri / Verus 逐步引入；
- **确定性测试**：Test Scheduler / Hunt Mode（CHESS 思路）。

## 9. 长期愿景

```text
Power On
  → Own Kernel
  → Own Resource Core
  → Component Graph
  → Graphics / Input / Audio / FS / Network
  → Game Runtime
  → Own Game
```

以后甚至可以：Game → AI Runtime → Small Model。

最终形态：一个用来探索操作系统、体系结构、虚拟机、runtime、驱动、组件化、可靠性和游戏系统的**个人实验平台**。BusyBox / POSIX 兼容从来不是终点，只是一种可选 Profile。