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
- **Authority ≠ Interface。** 驱动只能拿 Core 授予的类型化 Handle（`MmioHandle`/`IrqHandle`/`DmaHandle`/`TaskHandle`/`TimerHandle`/`AddressSpaceHandle`）；内存映射以 Core 管理的 `PhysicalRange`/`VirtualRange` 为单位，不能把逐帧 identity 当成组件 authority。组件**永远不能**拿裸物理地址、裸 IRQ 号或裸指针。Interface（`BlockDevice`、`SchedulerPolicy`...）是语义；传输（direct call / IPC / Wasm host call）是绑定策略，不要写死。
- **Core 只收真相，不收功能。** Core 不包含：RR/CFS 算法、文件系统格式、VFS、TCP/IP、VirtIO/NVMe 协议、POSIX 进程语义、ELF loader、Wasm runtime。
- **物理内存分配是 Core 内部机制**（canonical，不热卸载；可能按 build/profile 选择实现）。底层可以按页或 buddy block 实现，但公共资源模型以 region/address-space 为单位；没有 `FrameAllocatorPolicy` Component，也没有 allocator_simple 组件；未来 `MemoryPolicy` 只能提议偏好（NUMA 偏好、配额）。
- **Core 与组件共享一个 Core heap**，不做 per-component 内存记账（无 per-ComponentId 字节计费、无 per-component arena/私有堆）；ResourceDomain 只记设备/执行域 authority，不把每个物理页做成组件 handle，也不记内存字节配额。
- **组件失败 = 逻辑死亡、物理驻留**：标记 Failed、停止调度、在 Core 边界阻断过期访问、逻辑重启（全新实例）；phase 1 不承诺内存回收（KernelNative 无隔离），完整回收留给未来 ExecutionDomain；目标上暂无 panic recovery（panic=abort），phase 1 用 Result 传播错误。
- **判断标准**：如果一个完全错误的 Component 能通过某个 API 破坏其他 Component 或全局 invariant，就缩小 API，或把最终 authority 收回 Core。
- **架构定案：`kaleidos.elf` 单镜像 + 组件独立镜像（目标）。** bootstrap 与 Core 职责分离、装载合一（都只启动一次、永不热替换 → 链接成一个 `kaleidos.elf`，bootstrap 阶段 → `core::init(&MachineInfo)` 函数调用交接；职责边界 ≠ 装载边界，高半区 = 链接两段 + 页表双映射，Linux 同款）。**组件才是热插拔边界**（`.kcomp` = ELF 可重定位文件 + 符号表，Linux `.ko` 模式；打包 = cpio 归档 + 文本 manifest，`initramfs`/`modules.dep` 模式；embedded init.kpkg fallback → Persistent Store → Runtime Graph）。**当前不做**：组件 loader/kpkg 实现（方向定，等动态组件里程碑）、热迁移、复杂 IPC、微内核执行域、Wasm runtime、WIT/IDL、完整 capability 系统、完整 POSIX、Linux syscall 兼容、复杂 VFS、复杂 SMP 调度、形式化证明、完整 driver framework、完整依赖解析器。
- **Cargo 依赖图 ≠ Component/运行时组合图。** Cargo 边是编译期构建关系；运行时组件加载什么、如何组合由 Component Manager 决定，不写在 Cargo.toml 里。**实现 crate ≠ 运行时镜像**：`os/core` 是 host-testable 的 Rust library（`cargo test` 专用）；组件 .kcomp 才是运行时加载的镜像（未来）。
- **人类是实现者。** 代码保持极简、可手写。不要为了展示架构生成大量抽象、宏、动态注册系统、复杂 trait 层级、unsafe loader 或 runtime。小模块（几十行）就是普通 module，不要强行造 crate。**测试（host test / 单元测试 / CoreTest 用例）可由 Agent 编写；实现逻辑由人类手写。**
- **OS 源码统一收敛在 `os/` 下**（core/ arch/ components/ drivers/ boot/）；成品镜像在 `os/boot/<arch>/`（bin，链接 core 成 kaleidos.elf），不要散到任意位置。
- **外部依赖一律用 git submodule**（放 `third_party/`；克隆后先 `git submodule update --init --recursive`），不要本地 vendored 一份拷贝。
- **Core 与硬件无关的 truth logic 必须 host-testable。** Core 与 Arch/硬件 的真实契约（寄存器保存、页表生效、IRQ/timer 实际触发等）走 QEMU/CoreTest/真机验证；若某段 Core 逻辑只能整机测，先怀疑 Arch 耦合。CoreTest 无 god-mode，只能走真实 Core API（最多只读 `TestInspector`）。
- **Wasm 只是未来 Component 的执行后端之一，永远不是整个内核。** Core/Arch 保持 native Rust。

## 维护本文件的规则

只有"Agent 不看就会错"的稳定原则可以进本文件；易变内容（状态/命令/布局/里程碑）进 docs 与 README。