# AGENTS.md

> 本文件只保留**稳定原则**。现状、进度、里程碑、目录布局、构建命令等易变内容一律放在 `docs/` 与 `README.md` —— 项目迭代快，这里只写"Agent 不看就会错"的东西。
> 设计契约详见 `docs/`（先看索引 `docs/README.md`；`docs/architecture/` 与 `docs/philosophy/` 是权威契约，`docs/modules/` 描述各模块现状与代码位置，`docs/development/` 讲怎么干活，`docs/notes/` 是历史归档）。如有冲突，以 docs 为准。

## 项目

KaleidOS —— 组件化、多架构操作系统，面向学习、实验与个人创作。POSIX / BusyBox 兼容只是未来可选的一种 Profile。

> A small mechanism-first core beneath a composable graph of operating-system components.
> Build the machine, compose the system, run your own world.

## 不可违背的原则

- **少即是多：默认外置。** 能安全、清晰外置的能力就不进 Core；Core 只提供稳定极小的 mechanism，不拥有 policy。**不同信任等级使用不同边界**（KernelNative / IsolatedNative / U-mode），**部署形态本身就是安全策略**——不信任它就不要部署成 KernelNative；Core 不靠"所有组件都过同一套重机制"解决信任问题。
- **Core owns truth. Components own policy and semantics.** 真相（存在性/状态/所有权/生命周期）在 Core；算法、策略、协议、语义在 Component。
- **Policy proposes, Core validates and commits.** 调度器/分配器只能"提议"；存在性、状态、所有权、跨 CPU 状态由 Core 验证通过后才生效，并记录 trace。
- **机制与所有权 ≠ Interface。** 驱动经 Core 的 mechanism 认领设备：`kcore_device_nth`（纯发现）→ `DeviceId`（identity，非权限）→ `kcore_device_claim` 记 owner 并返回**本执行域访问窗口**（KernelNative = 裸寄存器基址，driver 自己 `volatile` 读写，稳态不进 Core；Isolated 未来 = mapped VA，由页表强制）。**Core 提供 mechanism，不伪造不存在的 security boundary**——KernelNative 就是可信代码，Core **不做 per-access 鉴权**，真正的访问强制只来自执行域（私有 AS + 页表）。Core 保留的是**所有权 / 生命周期记账**（device owner / IRQ route / DMA mapping），用于独占、unload、失败清理、quarantine，不是拦住受信组件；撤销在 KernelNative 是协作式的。IRQ 以已认领 `DeviceId` 为锚点，DMA **allocation 与 mapping 分离**。内存映射以 Core 管理的 `PhysicalRange`/`VirtualRange` 为单位，不能把逐帧 identity 当成组件资源；`MemoryLease` 只是 Core 内部 RAII。Interface（`BlockDevice`、`SchedulerPolicy`...）是语义；传输（direct call / IPC / Wasm host call）是绑定策略，不要写死。
- **Core 只收真相，不收功能。** Core 不包含：RR/CFS 算法、文件系统格式、VFS、TCP/IP、VirtIO/NVMe 协议、POSIX 进程语义、ELF loader、Wasm runtime。
- **物理内存分配是 Core 内部机制**（canonical，不热卸载；可能按 build/profile 选择实现）。底层可以按页或 buddy block 实现，但公共资源模型以 region/address-space 为单位；没有 `FrameAllocatorPolicy` Component，也没有 allocator_simple 组件；未来 `MemoryPolicy` 只能提议偏好（NUMA 偏好、配额）。
- **Core 管 Memory，不管 Heap（唯一例外：KernelNative 共享 Core 堆的窄后端）。** Core 的对象堆仅供 Core 内部使用；**KernelNative 组件与 Core 同特权、同地址空间，经 `kcore_heap_alloc/dealloc` 共享同一个 Core heap**——这是 KernelNative-only 的部署后端，**不是**通用 / 跨域内存 ABI（Isolated / Sandboxed 装载时显式拒绝这两个符号）。私有执行域的组件运行时**共享分配器实现代码**，在实例自己的可写 image 内拥有独立 `HeapState`（组件仍只写 `malloc`/`Vec`/`Box`）。**Core 不做内存记账**：KernelNative 无隔离，记 owner 没有可裁决的对象；Isolated/Sandboxed 的**分配归属与映射由该实例的地址空间 / 页表承载**，不另立账本。Core 只提供 backing / mapping 机制，**不**记 malloc/free 对象、**不**做 per-instance 字节计费或配额。
- **HeapState 分离不构成安全隔离。** KernelNative 的 instance failure 只保证**逻辑失效**；已发布 backing 保留驻留，不承诺撤销裸指针或物理回收。真正的访问强制与安全复用依赖真实执行域（私有 AS + 页表）及 DMA 静默条件。
- **内存资源语义统一，访问表示不统一。** Core 返回**本执行域 / 执行后端可用的访问窗口**（与 `kcore_device_claim` 同形：native = 本域 VA；Isolated = component-local VA；WASM = linear-memory offset），**不**向组件暴露 Core 私有 VA 或物理 backing identity。
- **组件失败 = 逻辑死亡、物理驻留**：标记 Failed、停止调度、在 Core 边界阻断过期访问、逻辑重启（全新实例）。普通失败用 `Result`；意外 panic 走**协作式 containment**（组件跑在 Core 拥有的独立栈上，先打印诊断再 stack-switch 回 Core，标记该 instance 失败后重调度）——`panic=abort` 不变、无 unwinding。**panic containment ≠ fault isolation**（KernelNative 仍可能写坏 Core 内存/UB/持锁死亡）；phase 1 不承诺内存回收（KernelNative 无隔离），完整回收留给未来 ExecutionDomain。teardown 以资源生命周期为核心，且 **CPU isolation ≠ DMA isolation**。
- **判断标准**：如果一个完全错误的 Component 能通过某个 API 破坏其他 Component 或全局 invariant，就缩小 API，或把最终裁决权收回 Core。保留一个 Core API 的判据：是否**只有 Core 能**操作页表 / 知道全局设备所有权 / 路由 IRQ / 管理组件生命周期 / 避免 DMA backing 被错误复用；否则删除。
- **架构定案：`kaleidos.elf` 单镜像 + 组件独立镜像（目标）。** bootstrap 与 Core 职责分离、装载合一（都只启动一次、永不热替换 → 链接成一个 `kaleidos.elf`，bootstrap 阶段 → `core::init(&MachineInfo)` 函数调用交接；职责边界 ≠ 装载边界，高半区 = 链接两段 + 页表双映射，Linux 同款）。**组件才是热插拔边界**（`.kcomp` = ELF 可重定位文件 + 符号表，Linux `.ko` 模式；打包 = cpio 归档 + 文本 manifest，`initramfs`/`modules.dep` 模式；embedded init.kpkg fallback → Persistent Store → Runtime Graph）。**当前不做**：运行期动态组件热插拔 / Runtime Graph（`.kcomp` loader 与 `init.kpkg` 已落地，见 `docs/architecture/component-lifecycle.md`）、热迁移、复杂 IPC、Wasm runtime、WIT/IDL、完整 capability 系统、完整 POSIX、Linux syscall 兼容、复杂 VFS、复杂 SMP 调度、形式化证明、完整 driver framework、完整依赖解析器。**执行域（进行中，已从"当前不做"解禁）**：先做 `IsolatedNative`（S-mode + 私有 AS：`satp`/ASID 切换 + 按域放段 / 按域 import 再解析 + `kcore_memory_acquire` 返回本域 VA），`SandboxedNative`（U-mode + `ecall`）后置；依赖序见 `docs/architecture/deployment.md` §6.3/§7，进度见 `STATUS.md`。
- **Cargo 依赖图 ≠ Component/运行时组合图。** Cargo 边是编译期构建关系；运行时组件加载什么、如何组合由 Component Manager 决定，不写在 Cargo.toml 里。**实现 crate ≠ 运行时镜像**：`os/core` 是 host-testable 的 Rust library（`cargo test` 专用）；组件 .kcomp 才是运行时加载的镜像（已落地，见 `docs/architecture/component-lifecycle.md`）。
- **组件 ABI 使用稳定窄 C ABI；Rust ABI 永不成为 Component ABI**：边界上禁止 mangled symbol / Rust trait object / `fmt::Arguments` / `PanicInfo` / allocator internals / Rust enum layout / 编译器私有结构。不建共享 Rust runtime；第三方 crate（smoltcp / virtio-drivers...）是 `.kcomp` **私有实现**，Core 只暴露固定 `kcore_*` ABI。`.kcomp` 是**链接好的组件程序**，不是 rustc `.o`；loader 只做段放置 + 白名单重定位，不是 Rust dynamic linker。
- **标识符不带版本后缀。** 类型 / 函数 / 链接名禁止 `V1`、`v2`、`_v2` 之类版本后缀（如 `SchedulerPolicyV1`、`kcore_device_claim_v2`、`VirtioProbeV1`）。契约演进靠 **exact ABI fingerprint + 协调替换**，不靠版本化命名：契约变了就**原地替换**名字/签名，本阶段不维护 ABI 兼容，也不保留旧名（不保证陈旧 `.kcomp` 可加载）。
- **人类是实现者。** 代码保持极简、可手写。不要为了展示架构生成大量抽象、宏、动态注册系统、复杂 trait 层级、unsafe loader 或 runtime。小模块（几十行）就是普通 module，不要强行造 crate。**测试（host test / 单元测试 / CoreTest 用例）可由 Agent 编写；实现逻辑由人类手写。**
- **OS 源码统一收敛在 `os/` 下**（core/ arch/ components/ boot/；**驱动也是组件，统一归 `components/drivers/`**）；成品镜像在 `os/boot/<arch>/`（bin，链接 core 成 kaleidos.elf），不要散到任意位置。
- **test-only fixture / 组件一律放 `os/components/tests/`**；`os/components/` 根只留生产组件与 SDK（`.kcomp` 名取目录 basename，故移动路径不改组件名）。
- **外部依赖一律用 git submodule**（放 `third_party/`；克隆后先 `git submodule update --init --recursive`），不要本地 vendored 一份拷贝。
- **构建配置以 `.config` 为唯一真相。** 配置走 Kconfig：`Kconfig` → `.config` → 生成 Make 片段（`scripts/kconfig/genmk.py`，唯一的 config→build 映射）→ Cargo features（**只是内部传输机制**）。不要手工同步各 crate 的 Cargo features，也不要让某个 crate 自己决定 profile；`#[cfg]`/`compile_error!` 是不变式与防御，不是配置来源。详见 `docs/architecture/kconfig.md`。
- **Core 与硬件无关的 truth logic 必须 host-testable。** Core 与 Arch/硬件 的真实契约（寄存器保存、页表生效、IRQ/timer 实际触发等）走 QEMU/CoreTest/真机验证；若某段 Core 逻辑只能整机测，先怀疑 Arch 耦合。CoreTest 无 god-mode，只能走真实 Core API。
- **Wasm 只是未来 Component 的执行后端之一，永远不是整个内核。** Core/Arch 保持 native Rust。

## 维护本文件的规则

只有"Agent 不看就会错"的稳定原则可以进本文件；易变内容（状态/命令/布局/里程碑）进 docs 与 README。