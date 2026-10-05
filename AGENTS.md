# AGENTS.md

KaleidOS 是面向学习、实验与个人创作的组件化操作系统。
本文件只保留 Agent 必须遵守的稳定规则；命令见 [README.md](README.md)，
现状与计划见 [STATUS.md](STATUS.md)，契约入口见 [docs/README.md](docs/README.md)。
涉及某个模块前，先读对应契约与模块页。文档之间的权威归属按文档索引判定。

## 工作约定

- **人类是实现者。** Agent 可以审计、整理文档、编写 host/单元/CoreTest 测试；
  实现逻辑由人类手写。代码必须极简、可手写；普通小模块不另造 crate，
  不为展示架构引入大量抽象、宏、动态注册系统或复杂 trait 层级。
- **事实与目标分开。** 状态、里程碑、构建命令与目录细节放 README、STATUS 与 docs，
  不写进本文件。模块页描述代码事实；未实现的目标明确标注。
- **一件事一处权威。** 契约细节写在对应文档，其他地方链接它。移动或重命名文档时，
  同步更新文档、源码注释、构建脚本与 CI 中的引用。

## Core 与组件

- **默认外置。** Core owns truth. Components own policy and semantics.
  Core 管资源存在性、状态、所有权、生命周期与跨组件安全；组件管算法、协议与业务语义。
  Policy proposes, Core validates and commits：可替换策略提出建议，Core 复验全局不变式、
  提交并记录 trace。物理内存分配是 canonical、不可热卸载的 Core 内部机制，
  无 `FrameAllocatorPolicy` 组件；未来 MemoryPolicy 也只能提议偏好。
- **Core 只收真相与必要机制。** RR/CFS、文件系统格式、VFS、TCP/IP、设备协议、
  POSIX 语义、应用 ELF loader、Wasm runtime 属于组件。保留 Core API 的判据是：
  只有 Core 能操作页表、裁决全局设备所有权、路由 IRQ、管理生命周期或防止 DMA backing
  被错误复用。错误组件若能通过 API 破坏跨组件不变式，应缩小 API 或把提交收回 Core。
- **部署决定信任边界。** KernelNative 与 Core 同特权、同地址空间，属于受信代码；
  Core 不做 MMIO per-access 鉴权，撤销是协作式的。私有执行域依靠真实地址空间与页表
  强制访问。HeapState 分离、panic containment 与 CPU isolation 均不能替代内存/DMA 隔离。
- **机制、接口、传输分开。** device discovery 得到 `DeviceId`（身份，非权限），claim
  记录 owner 并返回本执行域访问窗口；IRQ 锚在已认领设备，DMA allocation 与 mapping 分离。
  Interface 描述语义，direct call / IPC / Wasm host call 由绑定与部署决定。
- **Memory 与 Heap 分开。** 公共资源以 PhysicalRange / VirtualRange / AddressSpace 为单位，
  不暴露逐帧身份或 Core 私有 VA；MemoryLease 仅为 Core 内部 RAII。
  Core 提供本执行域可用的 backing/mapping 窗口（native 为本域 VA，Wasm 为 linear-memory
  offset），不另立 region owner 账本，不记录 malloc/free 对象、字节计费或配额。
  KernelNative 可经窄 ABI 共享 Core heap；这两个 heap 符号在 Isolated/Sandboxed 装载时拒绝。
  私有域共享分配器实现代码，但 HeapState 位于各实例自己的可写 image。
- **失败先保证逻辑失效。** 普通错误用 Result；组件 panic 经独立栈协作式 containment，
  打印诊断、返回 Core、标记 Failed 并停止该实例调度。保持 panic=abort、无 unwinding。
  KernelNative 已发布 backing 保留驻留，失败不承诺裸指针撤销或物理回收；重启创建新实例。
  完整回收依赖真实执行域与 DMA 静默条件，不能从 containment 推导 fault isolation。

以上细节以 [核心哲学](docs/philosophy/core-philosophy.md)、
[驱动契约](docs/architecture/driver-model.md)、
[内存与堆](docs/architecture/memory-and-heap.md)、
[调度契约](docs/architecture/scheduling.md) 为准。

## 镜像、ABI 与构建

- bootstrap 与 Core 职责分离、装载合一，链接成单个 `kaleidos.elf`；组件是独立镜像边界。
  `.kcomp` 是链接好的 ELF 可重定位组件程序，包为 cpio + 文本 manifest；loader 只做段放置
  与白名单重定位。Cargo 依赖图与运行时组件图分开，Rust library 与运行时镜像分开。
- 组件边界只用稳定窄 C ABI。禁止 Rust mangled symbol、trait object、fmt::Arguments、
  PanicInfo、Rust enum layout、allocator internals 与编译器私有结构；组件私有携带 SDK
  与第三方实现，不建共享 Rust runtime。ABI 靠 exact fingerprint 与协调替换演进，
  类型、函数、链接名和文档名不加版本后缀，不保留陈旧 ABI 兼容别名。
- 构建配置以本次构建选定的 resolved `.config` 为唯一真相：
  Kconfig → `.config` → `scripts/kconfig/genmk.py` → Make → Cargo features。
  genmk 是唯一 config→build 映射；Cargo features 只是内部传输，cfg/compile_error 是防御。
  不在 crate 中另选 profile，不手工同步各 crate 的系统 features。
- OS 源码统一在 `os/`，镜像入口在 `os/boot/<arch>/`，驱动在 `os/components/drivers/`，
  test-only 组件与 fixture 在 `os/components/tests/`。外部依赖使用 `third_party/` git submodule，
  不复制一份本地 vendor。Wasm 只是未来组件执行后端，Core/Arch 保持 native Rust。

详见 [组件生命周期](docs/architecture/component-lifecycle.md)、
[部署契约](docs/architecture/deployment.md) 与 [配置契约](docs/architecture/kconfig.md)。
执行域的依赖顺序见 deployment；实现范围与未实现能力统一查 STATUS。

## 验证

- 与硬件无关的 Core truth logic 必须 host-testable；只能整机测的逻辑先检查 Arch 耦合。
- CoreTest 是组件/系统集成场景的统一编排者，只走真实公开 Core API，无 god-mode，
  不修改 Core 私有状态。寄存器保存、页表生效、IRQ/timer 等真实硬件契约通过
  QEMU/ArchTest/真机验证；host fake 不作为隔离或跨域执行的证明。
- 新功能同时补相应测试，包含错误提案、错误 owner、过期身份与失败路径。
  测试结论注明验证层次；同一事实避免重复断言，协议层与硬件生效分别提供证据。

运行方式与测试职责以 [测试指南](docs/development/testing.md) 为准。
