# KaleidOS

组件化、多架构操作系统，面向学习、实验与个人创作。

> A small resource-authority core beneath a composable graph of operating-system components.

> **Build the machine, compose the system, run your own world.** 🎮

## 核心哲学

- **Core owns truth. Components own policy and semantics.** Core 保存真实且不可撒谎的系统状态；Component 实现可替换的算法、策略与语义。
- **Policy proposes, Core validates and commits.** 调度器/分配器只能"提议"，由 Core 验证存在性、状态、所有权后才生效。
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
   Arch + FDT            ← arch/（ISA） + FDT（机器描述数据）
        │
     Hardware
```

OS = **Resource Core + Component Graph + Profile**。同一个底座，通过重新组合 Component，可以长出完全不同的操作系统。

## 目录

```
kernel/       全部 OS 源码（seL4/Theseus 式收敛，不再散在仓库根）：
  core/            Resource Core crate：task/memory/object/handle/component/irq/timer/trace/inspector
  arch/            ISA 层 crate：riscv64/（dts/ = 解析器测试 fixture）
  interfaces/      Interface 契约 crate：device/ service/ policy/
  components/      策略/服务组件 crates：scheduler_rr/ allocator_simple/ core_test/ logger/
  drivers/         设备驱动组件（预留，由 FDT 发现）
  profiles/        最终镜像/Profile 组合点（启动编排 + panic handler）：minimal/
third_party/   外部依赖（git submodule）：fdt/（FDT 解析器，no_std 零依赖）
docs/          设计文档（架构/哲学/组件模型/测试/路线图/参考）
tools/         工具脚本（待建设）
```

## 当前状态

仓库骨架与设计文档已就绪，代码尚未开始。目标里程碑：**M0（QEMU RISC-V 启动，产出结构化 BOOT 日志）**。详见 `docs/roadmap.md`。

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