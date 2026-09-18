# AGENTS.md

> 本文件只保留**稳定原则**。现状、进度、里程碑、目录布局、构建命令等易变内容一律放在 `docs/` 与 `README.md` —— 项目迭代快，这里只写"Agent 不看就会错"的东西。
> 设计契约详见 `docs/`（architecture.md / core-philosophy.md / component-model.md / component-lifecycle.md / driver-model.md / kconfig.md / testing.md / roadmap.md / references.md）。如有冲突，以 docs 为准。

## 项目

KaleidOS —— 组件化、多架构操作系统，面向学习、实验与个人创作。POSIX / BusyBox 兼容只是未来可选的一种 Profile。

> A small resource-authority core beneath a composable graph of operating-system components.
> Build the machine, compose the system, run your own world.

## 不可违背的原则

- **少即是多：默认外置。** 能安全、清晰外置的能力就不进 Core；Core 只提供稳定极小的 mechanism，不拥有 policy。**不同信任等级使用不同边界**（KernelNative / IsolatedNative / U-mode），**部署形态本身就是安全策略**——不信任它就不要部署成 KernelNative；Core 不靠"所有组件都过同一套重机制"解决信任问题。
- **Core owns truth. Components own policy and semantics.** 真相（存在性/状态/所有权/生命周期）在 Core；算法、策略、协议、语义在 Component。
- **Policy proposes, Core validates and commits.** 调度器/分配器只能"提议"；存在性、状态、所有权、跨 CPU 状态由 Core 验证通过后才生效，并记录 trace。
- **Authority ≠ Interface。** 驱动只能拿 Core 授予的类型化 Handle（`MmioHandle`/`IrqHandle`/`DmaHandle`/`TaskHandle`/`TimerHandle`/`AddressSpaceHandle`）；内存映射以 Core 管理的 `PhysicalRange`/`VirtualRange` 为单位，不能把逐帧 identity 当成组件 authority。组件不能通过知道一个裸地址来获得 authority；裸指针只来自 Core 派生并持有 provenance 的 typed Lease——受信 KernelNative 驱动可经 Lease 取得映射指针（撤销为协作式），Sandboxed/Isolated 域由地址空间映射 + 页表强制。Interface（`BlockDevice`、`SchedulerPolicy`...）是语义；传输（direct call / IPC / Wasm host call）是绑定策略，不要写死。**Handle 属于 control plane**（身份/所有权/authority/generation/生命周期/撤销/记账）；在 KernelNative 它**不是内存安全屏障**——已泄漏的裸 MMIO 指针不因撤销而失效，快路面是经 Core 校验一次的 typed Lease/mapping。
- **Core 只收真相，不收功能。** Core 不包含：RR/CFS 算法、文件系统格式、VFS、TCP/IP、VirtIO/NVMe 协议、POSIX 进程语义、ELF loader、Wasm runtime。
- **物理内存分配是 Core 内部机制**（canonical，不热卸载；可能按 build/profile 选择实现）。底层可以按页或 buddy block 实现，但公共资源模型以 region/address-space 为单位；没有 `FrameAllocatorPolicy` Component，也没有 allocator_simple 组件；未来 `MemoryPolicy` 只能提议偏好（NUMA 偏好、配额）。
- **Core 与组件共享一个 Core heap**，不做 per-component 内存记账（无 per-ComponentId 字节计费、无 per-component arena/私有堆）；ResourceDomain 只记设备/执行域 authority，不把每个物理页做成组件 handle，也不记内存字节配额。
- **组件失败 = 逻辑死亡、物理驻留**：标记 Failed、停止调度、在 Core 边界阻断过期访问、逻辑重启（全新实例）。普通失败用 `Result`；意外 panic 走**协作式 containment**（组件跑在 Core 拥有的独立栈上，先打印诊断再 stack-switch 回 Core，标记该 instance 失败后重调度）——`panic=abort` 不变、无 unwinding。**panic containment ≠ fault isolation**（KernelNative 仍可能写坏 Core 内存/UB/持锁死亡）；phase 1 不承诺内存回收（KernelNative 无隔离），完整回收留给未来 ExecutionDomain。teardown 以资源生命周期为核心，且 **CPU isolation ≠ DMA isolation**。
- **判断标准**：如果一个完全错误的 Component 能通过某个 API 破坏其他 Component 或全局 invariant，就缩小 API，或把最终 authority 收回 Core。
- **架构定案：`kaleidos.elf` 单镜像 + 组件独立镜像（目标）。** bootstrap 与 Core 职责分离、装载合一（都只启动一次、永不热替换 → 链接成一个 `kaleidos.elf`，bootstrap 阶段 → `core::init(&MachineInfo)` 函数调用交接；职责边界 ≠ 装载边界，高半区 = 链接两段 + 页表双映射，Linux 同款）。**组件才是热插拔边界**（`.kcomp` = ELF 可重定位文件 + 符号表，Linux `.ko` 模式；打包 = cpio 归档 + 文本 manifest，`initramfs`/`modules.dep` 模式；embedded init.kpkg fallback → Persistent Store → Runtime Graph）。**当前不做**：运行期动态组件热插拔 / Runtime Graph（`.kcomp` loader 与 `init.kpkg` 已落地，见 `docs/component-lifecycle.md`）、热迁移、复杂 IPC、微内核执行域、Wasm runtime、WIT/IDL、完整 capability 系统、完整 POSIX、Linux syscall 兼容、复杂 VFS、复杂 SMP 调度、形式化证明、完整 driver framework、完整依赖解析器。
- **Cargo 依赖图 ≠ Component/运行时组合图。** Cargo 边是编译期构建关系；运行时组件加载什么、如何组合由 Component Manager 决定，不写在 Cargo.toml 里。**实现 crate ≠ 运行时镜像**：`os/core` 是 host-testable 的 Rust library（`cargo test` 专用）；组件 .kcomp 才是运行时加载的镜像（已落地，见 `docs/component-lifecycle.md`）。
- **组件 ABI 使用稳定窄 C ABI；Rust ABI 永不成为 Component ABI**：边界上禁止 mangled symbol / Rust trait object / `fmt::Arguments` / `PanicInfo` / allocator internals / Rust enum layout / 编译器私有结构。不建共享 Rust runtime；第三方 crate（smoltcp / virtio-drivers...）是 `.kcomp` **私有实现**，Core 只暴露固定 `kcore_*` ABI。`.kcomp` 是**链接好的组件程序**，不是 rustc `.o`；loader 只做段放置 + 白名单重定位，不是 Rust dynamic linker。
- **标识符不带版本后缀。** 类型 / 函数 / 链接名禁止 `V1`、`v2`、`_v2` 之类版本后缀（如 `SchedulerPolicyV1`、`kcore_irq_claim_v2`、`VirtioProbeV1`）。契约演进靠 **exact ABI fingerprint + 协调替换**，不靠版本化命名：契约变了就**原地替换**名字/签名，本阶段不维护 ABI 兼容，也不保留旧名（不保证陈旧 `.kcomp` 可加载）。
- **人类是实现者。** 代码保持极简、可手写。不要为了展示架构生成大量抽象、宏、动态注册系统、复杂 trait 层级、unsafe loader 或 runtime。小模块（几十行）就是普通 module，不要强行造 crate。**测试（host test / 单元测试 / CoreTest 用例）可由 Agent 编写；实现逻辑由人类手写。**
- **OS 源码统一收敛在 `os/` 下**（core/ arch/ components/ boot/；**驱动也是组件，统一归 `components/drivers/`**）；成品镜像在 `os/boot/<arch>/`（bin，链接 core 成 kaleidos.elf），不要散到任意位置。
- **外部依赖一律用 git submodule**（放 `third_party/`；克隆后先 `git submodule update --init --recursive`），不要本地 vendored 一份拷贝。
- **构建配置以 `.config` 为唯一真相。** 配置走 Kconfig：`Kconfig` → `.config` → 生成 Make 片段（`scripts/kconfig/genmk.py`，唯一的 config→build 映射）→ Cargo features（**只是内部传输机制**）。不要手工同步各 crate 的 Cargo features，也不要让某个 crate 自己决定 profile；`#[cfg]`/`compile_error!` 是不变式与防御，不是配置来源。详见 `docs/kconfig.md`。
- **Core 与硬件无关的 truth logic 必须 host-testable。** Core 与 Arch/硬件 的真实契约（寄存器保存、页表生效、IRQ/timer 实际触发等）走 QEMU/CoreTest/真机验证；若某段 Core 逻辑只能整机测，先怀疑 Arch 耦合。CoreTest 无 god-mode，只能走真实 Core API（最多只读 `TestInspector`）。
- **Wasm 只是未来 Component 的执行后端之一，永远不是整个内核。** Core/Arch 保持 native Rust。

## 维护本文件的规则

只有"Agent 不看就会错"的稳定原则可以进本文件；易变内容（状态/命令/布局/里程碑）进 docs 与 README。