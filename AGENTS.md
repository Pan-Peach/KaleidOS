# AGENTS.md

> 本文件只保留**稳定原则**。现状、进度、里程碑、目录布局、构建命令等易变内容一律放在 `docs/` 与 `README.md` —— 项目迭代快，这里只写"Agent 不看就会错"的东西。
> 设计契约详见 `docs/`（architecture.md / core-philosophy.md / component-model.md / testing.md / roadmap.md / references.md）。如有冲突，以 docs 为准。

## 项目

KaleidOS —— 组件化、多架构操作系统，面向学习、实验与个人创作。POSIX / BusyBox 兼容只是未来可选的一种 Profile。

> A small resource-authority core beneath a composable graph of operating-system components.
> Build the machine, compose the system, run your own world.

## 不可违背的原则

- **Core owns truth. Components own policy and semantics.** 真相（存在性/状态/所有权/生命周期）在 Core；算法、策略、协议、语义在 Component。
- **Policy proposes, Core validates and commits.** 调度器/分配器只能"提议"；存在性、状态、所有权、跨 CPU 状态由 Core 验证通过后才生效，并记录 trace。
- **Authority ≠ Interface。** 驱动只能拿 Core 授予的类型化 Handle（`FrameHandle`/`MmioHandle`/`IrqHandle`/`DmaHandle`/`TaskHandle`/`TimerHandle`/`AddressSpaceHandle`），**永远不能**拿裸物理地址、裸 IRQ 号或裸指针。Interface（`BlockDevice`、`SchedulerPolicy`...）是语义；传输（direct call / IPC / Wasm host call）是绑定策略，不要写死。
- **Core 只收真相，不收功能。** Core 不包含：buddy/RR/CFS 算法、文件系统格式、VFS、TCP/IP、VirtIO/NVMe 协议、POSIX 进程语义、ELF loader、Wasm runtime。
- **判断标准**：如果一个完全错误的 Component 能通过某个 API 破坏其他 Component 或全局 invariant，就缩小 API，或把最终 authority 收回 Core。
- **第一阶段不做**：动态加载、热迁移、复杂 IPC、微内核模式、Wasm runtime、WIT/IDL、完整 capability 系统、完整 POSIX、Linux syscall 兼容、复杂 VFS、复杂 SMP 调度、形式化证明、完整 driver framework、完整依赖解析器。组件注册是静态的。
- **人类是实现者。** 代码保持极简、可手写。不要为了展示架构生成大量抽象、宏、动态注册系统、复杂 trait 层级、unsafe loader 或 runtime。小模块（几十行）就是普通 module，不要强行造 crate。**测试（host test / 单元测试 / CoreTest 用例）可由 Agent 编写；实现逻辑由人类手写。**
- **OS 源码统一收敛在 `kernel/` 下**（core/ arch/ interfaces/ components/ drivers/ profiles/），不要散到仓库根或另起平行目录。
- **外部依赖一律用 git submodule**（放 `third_party/`；克隆后先 `git submodule update --init --recursive`），不要本地 vendored 一份拷贝。
- **Core 必须 host-testable。** 不许"必须启动 QEMU 才能测 Core"；若某个 Core 功能只能整机测，先怀疑 Arch 耦合。CoreTest 无 god-mode，只能走真实 Core API（最多只读 `TestInspector`）。
- **Wasm 只是未来 Component 的执行后端之一，永远不是整个内核。** Core/Arch 保持 native Rust。

## 维护本文件的规则

只有"Agent 不看就会错"的稳定原则可以进本文件；易变内容（状态/命令/布局/里程碑）进 docs 与 README。