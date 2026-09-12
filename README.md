# KaleidOS

组件化、多架构操作系统，面向学习、实验与个人创作。

> A small resource-authority core beneath a composable graph of operating-system components.

> **Build the machine, compose the system, run your own world.** 🎮

## 核心哲学

- **Core owns truth. Components own policy and semantics.** Core 保存真实且不可撒谎的系统状态，并拥有为保存这份真相、推进 Core 自身资源/生命周期操作所必需的机制；Component 实现可替换的算法、策略与语义。
- **Policy proposes, Core validates and commits.** 策略/调度器只能"提议"，由 Core 验证存在性、状态、所有权后才生效；物理帧分配本身是 Core 内部机制（canonical，不热卸载），不是"提议"的策略。
- **Authority ≠ Interface。** 驱动拿类型化 Handle（`MmioHandle`/`IrqHandle`/`DmaHandle`...）获得 authority，不能靠知道裸地址/裸 IRQ 获得 authority（裸指针只存在于 Core 派生并持有 provenance 的 typed Lease 内部）；Interface 是语义，传输是绑定策略。

## 架构

```text
Applications / System Personality
        │
 Services / Devices
        │
     Components          ← 可替换的算法、策略、驱动
        │
   Resource Core         ← 资源权威：存在性/状态/所有权/生命周期
        │
   Arch + Machine Discovery ← arch/（ISA） + 机器发现（FDT / ACPI / Probe）
        │
     Hardware
```

OS = **Resource Core + Component Graph + Profile**。同一个底座，通过重新组合 Component，可以长出完全不同的操作系统。

## 目录

```
os/            全部 OS 源码（seL4/Theseus 式收敛，不再散在仓库根）：
  boot/            成品镜像层（bin，按目标架构分目录）：riscv/ ——
                   RV64/Sv39 与 RV32/Sv32 profile 共用 RISC-V family，
                   _start → FDT discovery → MachineInfo → core::init() → Core Monitor，
                   与 core 链接成 kaleidos-<arch>（单镜像，职责分离装载合一）
  core/            Resource Core **library**（host-testable）：task/memory/object/handle/component/irq/timer/trace/inspector/machine/print
  arch/            统一 arch crate：CpuArch/Console/SystemReset backend traits + cfg 选择 riscv / fake
  components/      组件 crates（策略 / 服务 / 测试）：scheduler_rr/ core_test/ logger/ …
  components/drivers/  驱动组件（驱动多而杂，统一归纳在这里）：uart/ virtio_blk/ …
third_party/   外部依赖（git submodule）：fdt/（FDT 解析器）/ buddy_system_allocator/（MetadataHeap，O(1) buddy）——workspace exclude，clippy 不检索
tests/         测试 fixture：fixtures/fdt/（qemu-virt.dts，供 discovery host test）
docs/          设计文档（架构/哲学/组件模型/测试/路线图/参考）
tools/         工具脚本（待建设）
```

> **Cargo 依赖图 ≠ Component 图。** 组件运行时的加载/组合由 Component Manager 决定（未来：.kcomp + cpio + manifest，Linux insmod/depmod/initramfs 模式）——不写在 Cargo.toml 里。

## 当前状态

**架构定案：`kaleidos.elf` 单镜像（os/boot + core 链接，职责分离装载合一；组件未来独立 `.kcomp`=Linux insmod 模式）。** 当前完成（2026-09，全部 QEMU 端到端验证，RV64 + RV32 双 profile）：

- **Boot 全链**：FDT discovery → MachineInfo → `core::init` → **Core Monitor 交互 shell**（`core> help/machine/memory/frame/tasks/load/components/shutdown/reboot`）；RV64 走 Sv39 identity+高半区双映射，RV32 走 Sv32 identity
- **MMU**：`KernelAddressSpace`（Core 语义 ledger + `AddressSpaceBackend` contract）+ `Sv39PageTable`/`Sv32PageTable`（buddy 回调分配页表页，mid-map 失败回滚，host 测试直驱生产实现）
- **组件加载链**（Linux insmod 教学版）：`.kcomp`（ELF32/ELF64 ET_REL，no_std Rust）→ `make init.kpkg`（cpio+manifest）→ `.initpkg` 内嵌 → `store`（cpio 解析）→ `loader`（段放置 + RV32/RV64 重定位）→ `registry`（生命周期状态机：Declared → Resolved → Ready）→ monitor `load` 命令
- **导出白名单**（EXPORT_SYMBOL 教学版，33 条 `kcore_*`：共享堆 alloc/dealloc + 输出 + 机器/系统只读查询 + v2 语义入口——组件加载/接口发布/任务控制/调度 + C6 资源 authority `kcore_mmio_claim/read_u32/write_u32/release/lease` + `kcore_irq_claim/register/enable/register_polled/poll/ack` + DMA authority `kcore_dma_alloc/lease/release`，错误码统一 `0/-Errno`）：组件只能调白名单，未导出符号 → 加载失败
- **Component Interface Registry**（骨架）：组件→组件依赖只走 Interface binding（publish/resolve/unbind，versioned vtable），不建立 flat ELF symbol 全局符号表
- **C4 调度执行链**（第一条完整系统链，全程只走导出白名单）：`load core_test` → Core 加载 scheduler_rr（`kcore_component_load`）→ 接口 publish/bind（SchedulerPolicy v1）→ 任务创建/启动 → Core propose→validate→commit 调度（RR 交替）→ yield/exit → 状态验证；core_test 12 项自检全 PASS（RV64+RV32）

实机输出：

```text
core> load kcomp_smoke
[smoke] hex=12            ← 组件调内核 console/count（通过白名单重定位）
!load kcomp_smoke: OK (id=1, entry=0x81a00000)
```

日志走 `printk!`/`log!` 宏（格式化在 core，传输在 arch 的 `Console` backend：host=Fake/std，当前 RISC-V=OpenSBI）。质量工具链：`make fmt` / `make clippy` / `make check`（CI 快车道） + `make test-qemu`（自动 boot smoke + core_test 判定） + `make test-arch`（ArchTest 白盒 selftest，独立 CI job）。默认构建 RV64，也可用 `make kernel ARCH=rv32` 构建 RV32（QEMU 内存 rv32=1G，见 Makefile）。

## 构建

外部依赖是 git submodule，克隆后先初始化：

```sh
git submodule update --init --recursive
cargo check
```

QEMU 运行：`make qemu`（默认 RV64），或 `make qemu ARCH=rv32`。

## 文档

| 文档 | 内容 |
|---|---|
| `docs/architecture.md` | 架构总览（分层 / Core / Component / ExecutionDomain / Profile） |
| `docs/core-philosophy.md` | 核心哲学与判断标准 |
| `docs/component-model.md` | 组件模型（生命周期 / ResourceDomain / 依赖图） |
| `docs/driver-model.md` | 驱动与执行域模型（Handle→Lease / MMIO·IRQ·DMA / 撤销不变式） |
| `docs/testing.md` | 测试策略（host test / CoreTest / trace） |
| `docs/roadmap.md` | 路线图（M0–M4 与后续方向） |
| `docs/references.md` | 参考资料与借鉴方向 |
