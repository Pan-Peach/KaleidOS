# KaleidOS

组件化、多架构操作系统，面向学习、实验与个人创作。

> A small mechanism-first core beneath a composable graph of operating-system components.

> **Build the machine, compose the system, run your own world.** 🎮

## 核心哲学

- **少即是多：Core 做得越少，形态越多。** 默认外置——能安全、清晰外置的能力就不进 Core；Core 只提供稳定极小的 mechanism，策略与语义在上层可替换。不同信任等级使用不同边界（KernelNative / IsolatedNative / U-mode），**部署形态本身就是安全策略**；Core 提供 mechanism，不拥有 policy。
- **Core owns truth. Components own policy and semantics.** Core 保存真实且不可撒谎的系统状态，并拥有为保存这份真相、推进 Core 自身资源/生命周期操作所必需的机制；Component 实现可替换的算法、策略与语义。
- **Policy proposes, Core validates and commits.** 策略/调度器只能"提议"，由 Core 验证存在性、状态、所有权后才生效；物理帧分配本身是 Core 内部机制（canonical，不热卸载），不是"提议"的策略。
- **机制与所有权 ≠ Interface。** Core 提供 mechanism 并**不伪造不存在的 security boundary**：`kcore_device_nth`（纯发现）→ `DeviceId`（identity，非权限）→ `kcore_device_claim` 记 owner 并返回本执行域访问窗口（KernelNative = 裸寄存器基址，driver 自己 `volatile` 读写，稳态不进 Core）。**KernelNative 就是可信代码**，Core 不做 per-access 鉴权；真正的强制只来自执行域（私有 AS + 页表）。Core 保留的是设备/IRQ/DMA 归属记账，用于独占、unload、失败清理、quarantine。Interface 是语义，传输是绑定策略。

## 架构

```text
Applications / System Personality
        │
 Services / Devices
        │
     Components          ← 可替换的算法、策略、驱动
        │
   Resource Core         ← 机制 + 所有权：存在性/状态/所有权/生命周期
        │
   Arch + Machine Discovery ← arch/（ISA） + 机器发现（FDT / ACPI / Probe）
        │
     Hardware
```

OS = **Resource Core + Component Graph + Profile**。同一个底座，通过重新组合 Component，可以长出完全不同的操作系统。

## 目录

```
Kconfig        构建配置顶层入口；`.config`（gitignored）是配置唯一真相，configs/*_defconfig 是具名 profile（见 docs/architecture/kconfig.md）
configs/       具名 profile（defconfig）：qemu_rv64 / qemu_rv32 / qemu_rv32_nommu
scripts/       Kconfig 胶水脚本：kconfig/configure.py（创建/归一化 .config）+ kconfig/genmk.py（生成 Make 片段）
os/            全部 OS 源码（seL4/Theseus 式收敛，不再散在仓库根）：
  boot/            成品镜像层（bin，按目标架构分目录）：riscv/ ——
                   RV64/Sv39 与 RV32/Sv32 profile 共用 RISC-V family，
                   _start → FDT discovery → MachineInfo → core::init() → Core Monitor，
                   与 core 链接成 kaleidos-<arch>（单镜像，职责分离装载合一）
  core/            Resource Core **library**（host-testable）：task/memory/resource/object/component/irq/timer/trace/inspector/machine/print
  arch/            统一 arch crate：CpuArch/Console/SystemReset backend traits + cfg 选择 riscv / fake
  components/      组件 crates：生产组件（策略 / 服务 / 驱动 / 文件系统 / SDK）+ tests/（test-only fixture 与 CoreTest）
  components/drivers/  驱动组件（驱动多而杂，统一归纳在这里）：uart/ virtio_blk/ …
third_party/   外部依赖（git submodule）：fdt/（FDT 解析器）/ buddy_system_allocator/（MetadataHeap，O(1) buddy）/ Kconfiglib/（Kconfig 前端）——workspace exclude，clippy 不检索
tests/         测试 fixture：fixtures/fdt/（qemu-virt.dts，QEMU virt 真实 DTB 转储；供未来 parser 测试与人工对照）
docs/          设计文档（索引 docs/README.md）：philosophy/（为什么）architecture/（是什么）interfaces/（契约）modules/（各模块现状）development/（怎么干活）notes/（历史归档）
tools/         构建辅助脚本（build-kcomp.sh 等）
```

> **Cargo 依赖图 ≠ Component 图。** 组件运行时的加载/组合由 Component Manager 决定（未来：.kcomp + cpio + manifest，Linux insmod/depmod/initramfs 模式）——不写在 Cargo.toml 里。

## 当前状态

**架构定案：`kaleidos.elf` 单镜像（os/boot + core 链接，职责分离装载合一；组件未来独立 `.kcomp`=Linux insmod 模式）。** 当前完成（2026-09，全部 QEMU 端到端验证，RV64 + RV32 双 profile）：

- **Boot 全链**：FDT discovery → MachineInfo → `core::init` → **Core Monitor 交互 shell**（`core> help/machine/memory/tasks/load/components/catalog/shutdown/reboot`；行编辑支持光标移动/退格/Ctrl-U·K·W、8 条历史 ↑/↓、Tab 命令补全；空闲时不忙等——arm ~10ms one-shot timer 后 `wfi`，由时钟中断唤醒）；RV64 走 Sv39 identity+高半区双映射，RV32 走 Sv32 identity
- **MMU**：`KernelAddressSpace`（Core 语义 ledger + `AddressSpaceBackend` contract）+ `Sv39PageTable`/`Sv32PageTable`（buddy 回调分配页表页，mid-map 失败回滚，host 测试直驱生产实现）
- **组件加载链**（Linux insmod 教学版，语言无关）：`.kcomp`（ELF32/ELF64 ET_REL；Rust 走 `tools/build-kcomp.sh`，freestanding C 走 `tools/build-kcomp-c.sh`）→ `make init.kpkg`（cpio+manifest）→ `.initpkg` 内嵌 → `store`（cpio 解析）→ `loader`（段放置 + RV32/RV64 重定位）→ `registry`（生命周期状态机：Declared → Resolved → Ready）→ monitor `load` 命令。C 组件只 `#include "kcomp.h"` 直调 `kcore_*`，SDK 的 C 运行时（`kcomp-sdk/c/kcomp_rt.c`，weak `mem*`）随组件私有携带；`make test-qemu`（CoreTest `c-frontend` + runner 机器级 `load`/`unload kcomp_c_smoke`）用最小 C 组件在 RV64/RV32 端到端验证
- **导出白名单**（EXPORT_SYMBOL 教学版，一组 `kcore_*`：共享堆 alloc/dealloc + 输出 + 机器/系统只读查询 + v2 语义入口——组件加载/接口发布/任务控制/调度 + C6 设备/IRQ/DMA 机制 `kcore_device_nth`（纯发现）/`kcore_device_claim/release`（认领确切设备，返回本执行域 MMIO 窗口） + `kcore_irq_register/enable/disable/release`（锚在已认领 DeviceId） + `kcore_dma_alloc/free/map/unmap`（allocation 与 mapping 分离，backing 撤销进 QUARANTINE），错误码统一 `0/-Errno`）：组件只能调白名单，未导出符号 → 加载失败
- **Component Interface Registry**（骨架）：组件→组件依赖只走 Interface binding（staged publish / bind / refresh / unbind，exact ABI fingerprint + typed `#[repr(C)]` function table），不建立 flat ELF symbol 全局符号表
- **C4 调度执行链**（第一条完整系统链，全程只走导出白名单）：`load core_test` → Core 加载 scheduler_rr（`kcore_component_load`）→ 接口 publish/bind（SchedulerPolicy）→ 任务创建/启动 → Core propose→validate→commit 调度（RR 交替）→ yield/exit → 状态验证；core_test 端到端自检全 PASS（RV64+RV32）
- **组件 panic containment**（init + task 边界，协作式）：组件跑在 Core 拥有的独立栈上；panic 时先用直接 SBI 打印诊断（`[panic] component=<id> task=<id> at <loc>: <msg>`）再 stack-switch 回 Core 上下文，标记该 instance Failed 后重新调度。`panic=abort` 不变、无 unwinding，不承诺内存回收（containment ≠ fault isolation）。ArchTest `panic-containment` / `task-panic`（RV64+RV32）

实机输出：

```text
core> load kcomp_smoke
[smoke] hex=12            ← 组件调内核 console/count（通过白名单重定位）
!load kcomp_smoke: OK (id=1, entry=0x81a00000)
```

日志走 `printk!`/`log!` 宏（格式化在 core，传输在 arch 的 `Console` backend：host=Fake/std，当前 RISC-V=OpenSBI）。质量工具链：`make fmt` / `make clippy` / `make check`（CI 快车道） + `make test-qemu`（自动 boot smoke + core_test 判定） + `make test-arch`（ArchTest 白盒 selftest，独立 CI job）。默认 profile 为 RV64；切换架构 / VM 走 Kconfig：`make qemu_rv32_defconfig` 或 `make qemu_rv32_nommu_defconfig`，再 `make qemu`（见 `docs/architecture/kconfig.md`）。

## 构建

外部依赖是 git submodule，克隆后先初始化：

```sh
git submodule update --init --recursive
cargo check
```

配置走 Linux Kconfig 风格：`.config` 是唯一配置真相，先选 profile 再构建（详见 `docs/architecture/kconfig.md`）：

```sh
make qemu_rv64_defconfig        # RV64 / supervisor / MMU（默认 profile）
make qemu_rv32_defconfig        # RV32 / supervisor / MMU
make qemu_rv32_nommu_defconfig  # RV32 / supervisor / NoMMU
make qemu                       # 构建并在 QEMU 中运行（Ctrl-A X 退出）

make menuconfig                 # 交互式编辑 .config
make olddefconfig               # 用新默认值刷新 .config
```

主工作流：`make <board>_defconfig && make qemu`。

## 文档

索引与权威归属见 [`docs/README.md`](docs/README.md)（先看这个）。

| 文档 | 内容 |
|---|---|
| `docs/README.md` | 文档索引：每个文件夹/文件干嘛、冲突时谁赢 |
| `docs/philosophy/core-philosophy.md` | 核心哲学与判断标准 |
| `docs/architecture/overview.md` | 架构总览（分层 / Core / Component / ExecutionDomain / Profile） |
| `docs/architecture/component-model.md` | 组件模型（Interface / ResourceDomain / 依赖图） |
| `docs/architecture/component-lifecycle.md` | 组件生命周期与实例契约（已冻结） |
| `docs/architecture/driver-model.md` | 驱动与执行域模型（device claim / MMIO·IRQ·DMA / teardown 安全） |
| `docs/architecture/kconfig.md` | 配置系统（Kconfig / `.config` 唯一真相） |
| `docs/modules/README.md` | 模块地图：每个 Core 模块 owns 什么真相、代码在哪 |
| `docs/development/testing.md` | 测试策略（host test / CoreTest / trace） |
| `docs/development/benchmark.md` | 性能基准（harness / 拆 primitive / 回归策略 / FS roadmap） |
| `docs/development/roadmap.md` | 路线图（M0–M4 与后续方向） |
| `docs/philosophy/references.md` | 参考资料与借鉴方向 |
