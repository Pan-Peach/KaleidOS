# KaleidOS

组件化、多架构操作系统，面向学习、实验与个人创作。

> A small resource-authority core beneath a composable graph of operating-system components.

> **Build the machine, compose the system, run your own world.** 🎮

## 核心哲学

- **Core owns truth. Components own policy and semantics.** Core 保存真实且不可撒谎的系统状态，并拥有为保存这份真相、推进 Core 自身资源/生命周期操作所必需的机制；Component 实现可替换的算法、策略与语义。
- **Policy proposes, Core validates and commits.** 策略/调度器只能"提议"，由 Core 验证存在性、状态、所有权后才生效；物理帧分配本身是 Core 内部机制（canonical，不热卸载），不是"提议"的策略。
- **Authority ≠ Interface。** 驱动拿类型化 Handle（`MmioHandle`/`IrqHandle`/`DmaHandle`...），永远不拿裸地址/裸 IRQ；Interface 是语义，传输是绑定策略。

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
  boot/            成品镜像层（bin，按目标架构分目录）：riscv64/ ——
                   _start → FDT discovery → MachineInfo → core::init() → Core Monitor，
                   与 core 链接成 kaleidos.elf（单镜像，职责分离装载合一）
  core/            Resource Core **library**（host-testable）：task/memory/object/handle/component/irq/timer/trace/inspector/machine/print
  arch/            统一 arch crate：trait Arch（静态方法接口）+ ArchImpl（cfg 选择 riscv64 / fake）
  components/      策略/服务组件 crates：scheduler_rr/ core_test/ logger/
  drivers/         设备驱动组件（预留，由 Machine Discovery 发现）
third_party/   外部依赖（git submodule）：fdt/（FDT 解析器）/ buddy_system_allocator/（MetadataHeap，O(1) buddy）——workspace exclude，clippy 不检索
tests/         测试 fixture：fixtures/fdt/（qemu-virt.dts，供 discovery host test）
docs/          设计文档（架构/哲学/组件模型/测试/路线图/参考）
tools/         工具脚本（待建设）
```

> **Cargo 依赖图 ≠ Component 图。** 组件运行时的加载/组合由 Component Manager 决定（未来：.kcomp + cpio + manifest，Linux insmod/depmod/initramfs 模式）——不写在 Cargo.toml 里。

## 当前状态

**架构定案：`kaleidos.elf` 单镜像（os/boot + core 链接，职责分离装载合一；组件未来独立 `.kcomp`=Linux insmod 模式）。** 当前完成：boot 阶段（FDT discovery → MachineInfo）→ `core::init`（BSS 清零 + MetadataHeap 帧分配器 + 探测）→ **Core Monitor 交互 shell**（`core> machine/memory/frame/help/shutdown/reboot`），全链路 `BOOT DISCOVERY OK → BOOT CORE OK → core>`。日志走 `printk!`/`log!` 宏（格式化在 core，传输在 arch：host=FakeArch/std，riscv64=SBI DBCN）。质量工具链：`make fmt` / `make clippy` / `make check`。详见 `docs/roadmap.md`。

## 构建

外部依赖是 git submodule，克隆后先初始化：

```sh
git submodule update --init --recursive
cargo check
```

QEMU 运行命令待 M0 落地后补充。

## 文档

| 文档 | 内容 |
|---|---|
| `docs/architecture.md` | 架构总览（分层 / Core / Component / ExecutionDomain / Profile） |
| `docs/core-philosophy.md` | 核心哲学与判断标准 |
| `docs/component-model.md` | 组件模型（生命周期 / ResourceDomain / 依赖图） |
| `docs/testing.md` | 测试策略（host test / CoreTest / trace） |
| `docs/roadmap.md` | 路线图（M0–M4 与后续方向） |
| `docs/references.md` | 参考资料与借鉴方向 |