# 参考资料与借鉴方向（references.md）

> 本文档整理 KaleidOS 设计时可以对照的参考项目、论文与工具链。
> 每个条目说明三件事：**它是什么**、**我们借鉴什么**、**怎么映射到 KaleidOS 的具体设计点**（以及明确不照搬什么）。
> 本文档是"设计时的阅读清单"，不是实现清单 —— 第一阶段不要求读懂全部，用到哪一块再深入哪一块。

---

## 总览表

| 参考 | 类型 | 借鉴的核心 | 对应 KaleidOS 设计点 |
|---|---|---|---|
| Asterinas | 开源 OS（Rust） | 安全策略注入、策略输出校验 | Core/Component 分离、Policy proposes → Core validates |
| seL4 | 微内核 OS（形式化验证） | typed capability、不可伪造授权 | Handle / Authority、内核对象 |
| Exokernel | 论文（经典） | 保护与管理分离、底层资源暴露 | Core 保护资源、Component 决定策略 |
| SPIN | 论文（经典） | 语言级安全的内核扩展 | 类型安全组件运行在内核地址空间 |
| Singularity | 研究 OS（微软） | 软件隔离、契约式通信 | 不依赖硬件地址空间的组件隔离 |
| Inferno | 研究 OS（Bell Labs） | Dis 虚拟 ISA、可移植程序 | Wasm 作为组件执行后端的先例 |
| RedLeaf | 研究 OS（Rust） | 语言级隔离、驱动故障恢复 | 驱动组件化、ResourceDomain 回收 |
| Theseus | 研究 OS（Rust） | 细粒度组件、状态管理、在线演化 | 组件生命周期、替换/恢复模型 |
| WebAssembly | 标准/运行时 | 虚拟 ISA、沙箱、导入导出 | 未来的组件执行后端（v0 不实现） |
| Wasm Component Model | 标准 | WIT、Interface Types、Canonical ABI | 未来 Interface 跨 ABI 参考 |
| eBPF | 内核机制（Linux） | 受验证的虚拟 ISA、安全内核扩展 | 与 Wasm 路线对比、策略注入参考 |
| CHESS | 论文（微软） | 确定性并发测试 | 未来 Test Scheduler / Hunt Mode |
| FSCQ | 论文 | 文件系统验证、崩溃一致性 | 未来对 Component contract 的强验证 |
| Kani / Loom / Miri / Verus | 工具链 | 模型检查 / 并发探索 / UB 检查 / 演绎验证 | 未来 Developer-First 测试工具链 |

---

## 1. Asterinas

**是什么**：用 Safe Rust 编写的开源 OS（目标是 Linux ABI 兼容），强调"安全策略注入"：把调度器、内存分配器等策略移出可信核心（TCB），但底层 framework 保留最终验证权。核心设施是 OSTD（类似内核版的 std）。架构称"framekernel"：TCB 极小且 sound，其余为 Safe Rust 服务。另有 USENIX ATC 2025 论文 *Asterinas: A Linux ABI-Compatible, Rust-Based Framekernel OS with a Small and Sound TCB*。

**借鉴什么**：
- 调度器 / 分配器等策略组件可以移出可信核心，由"框架"注入；
- 策略组件的输出（比如"运行这个任务"）必须经过底层框架的**验证**才能生效 —— 这正是我们 `Policy proposes, Core validates and commits` 的直接先例；
- 用 Safe Rust 约束 TCB 规模：Core 越小，可证明/可审查的部分越少。

**怎么映射**：
- 我们的 SchedulerPolicy / FrameAllocatorPolicy 组件 ≈ Asterinas 的策略注入；
- 我们的 Core 校验路径（任务是否存在、是否 Runnable、是否在别的 CPU）≈ 它的 policy output validation。

**不照搬**：Linux ABI 兼容目标；OSTD 那样庞大的内核基础库 —— 我们第一阶段 Core 词汇表保持最小。

---

## 2. seL4

**是什么**：形式化验证的微内核，以 capability 为核心：一切内核对象（task、frame、IRQ、IPC endpoint...）都通过不可伪造、不可混淆的 capability 访问，授权粒度极细（typed capability）。

**借鉴什么**：
- **typed authority**：每种授权有明确类型，`FrameHandle` 只能操作 frame，不能当 IRQ 用 —— 我们 Handle 的类型化设计直接来源于此；
- **不可伪造**：capability 只能由内核创建和传递，用户无法构造 —— 我们要求"驱动永远拿不到裸物理地址/裸 IRQ 号/裸指针"就是这个原则；
- 内核对象（kernel object）作为资源存在性/所有权记录在核心 —— 我们的 `Handle / Authority` + 内核对象表。

**怎么映射**：
- `FrameHandle`、`MmioHandle`、`IrqHandle`、`DmaHandle`、`TaskHandle`、`TimerHandle`、`AddressSpaceHandle` 就是 seL4 风格 typed capability 的简化形态；
- Core 校验 handle 的持有者、状态、生命周期 = capability 的 access control。

**不照搬**：完整 capability 系统（派生、revoke 树、badge 等）、形式化证明、IPC endpoint 体系 —— 第一阶段只做"不可伪造的类型化 Handle + Core 验证"。

---

## 3. Exokernel（论文）

> *Exokernel: An Operating System Architecture for Application-Level Resource Management*（MIT, 1995）

**是什么**：经典论文。核心主张"保护与管理分离"：内核只做保护（protection），把资源管理策略（management）下放到应用层 LibOS。内核极薄，只强制资源所有权与访问控制。

**借鉴什么**：
- **Protection vs Management 分离**：这正是"Core 保护资源，Component 决定策略"的源头；
- 向低层组件暴露接近硬件的资源接口（当然我们要包一层 Handle，这是对裸暴露的修正）。

**怎么映射**：
- Core = 保护层（资源存在性、所有权、权限）；Component = 管理层（调度策略、分配算法）；
- Exokernel 的"每个应用有自己的 LibOS" ≈ 我们的"每个 Profile 组合自己的 Component Graph"。

**不照搬**：把裸物理地址/裸中断直接暴露给应用 —— 我们坚持 Authority（Handle）抽象，这是 exokernel 实践中最被诟病的点，我们通过 typed Handle 修正。

---

## 4. SPIN（论文）

> 主要论文：*Extensibility, Safety and Performance in the SPIN Operating System*（Bershad 等, SOSP 1995, 华盛顿大学）
> 早期同名技术报告：*SPIN: An Extensible Microkernel for Application-specific Operating System Services*（UW TR-94-03-03, 1994）

**是什么**：用 Modula-3（类型安全语言）写的可扩展微内核。允许应用把类型安全的扩展代码**直接加载进内核地址空间**，通过语言保证（类型安全、GC、强制接口）而不是硬件隔离来保护内核。

**借鉴什么**：
- "类型安全组件可以安全地运行在内核地址空间" —— 这是 KernelNative ExecutionDomain 的历史依据；
- 扩展必须通过受限接口与内核交互，编译器/语言层面强制接口边界。

**怎么映射**：
- 我们的 Component（Rust 实现，静态注册）运行在 KernelNative 域，靠 Rust 类型系统 + Core 验证保证边界，而不是每个组件一个地址空间；
- 组件只能通过 Interface（trait）和 Handle 与外界交互。

**不照搬**：Modula-3 语言运行时依赖；它的动态扩展模型 —— 我们第一阶段静态注册。

---

## 5. Singularity

**是什么**：微软研究院用托管语言（Sing#，C# 方言）写的 OS。核心思想是**软件隔离（SI）**：进程（SIP = Software-Isolated Process，软件隔离进程）之间不用硬件地址空间隔离，而是靠托管语言 + 编译期验证的**契约式通信**（contract-based communication，channel 上的消息协议）保证。

**借鉴什么**：
- **软件隔离**：隔离不一定来自 MMU —— 语言/运行时/契约也能提供隔离，这为未来多种 ExecutionDomain 并存提供了理论空间；
- **契约式通信**：进程间只通过声明好的契约交互 —— 类似我们的 Interface 语义化（Interface 是语义，传输是绑定策略）。

**怎么映射**：
- 未来若出现 UserAddressSpace / WasmSandbox 域，Singularity 说明"同一个 Component Graph 用不同隔离方式"是可行的；
- Interface contract（如 BlockDevice 契约）≈ SIP 的 channel 契约。

**不照搬**：托管语言运行时作为内核基础；SIP 模型本身 —— 我们第一阶段全部 KernelNative + Rust。

---

## 6. Inferno

**是什么**：Bell Labs 的研究 OS。整个系统围绕一个**虚拟 ISA**（Dis）构建：程序编译为 Dis 字节码，在任何有 Inferno 的平台（含虚拟机宿主）上运行。应用语言是 Limbo（并发、通道风格）。

**借鉴什么**：
- "OS 围绕虚拟 ISA 构建"的先例 —— 程序可移植、隔离由虚拟机提供；
- 这是我们未来 Wasm 组件执行后端的历史参照（Inferno ≈ Wasm 时代的先行者）。

**怎么映射**：
- 未来的 `scheduler.wasm`、`game.wasm` 等 ≈ Inferno 的 Dis 程序；
- 虚拟 ISA 让同一份组件二进制跑在 RISC-V / x86_64 / ARM64 / LoongArch 上。

**不照搬**：把整个 OS 建在虚拟 ISA 上 —— 我们 Core/Arch 保持 native Rust，Wasm 只是组件的一种执行方式。

---

## 7. RedLeaf（论文）

> *RedLeaf: Isolation and Communication in a Safe Operating System*（加州大学尔湾分校 UCI / VMware Research, OSDI 2020, Anton Burtsev 团队）

**是什么**：用 Rust 写的研究 OS。核心思想是**语言级隔离（language-based isolation）**：OS 域（domain）不是硬件地址空间，而是 Rust 所有权/借用检查保证的隔离单元；域内分配器、域间通信都由类型系统约束。支持驱动故障恢复。

**借鉴什么**：
- Rust 所有权模型可以充当隔离机制 —— 我们的 ResourceDomain 概念与它同源：**谁拥有什么资源，由类型系统 + Core 记录保证，而不是靠地址空间边界**；
- 驱动故障恢复路径（域崩溃 → 回收资源 → 重建）≈ 我们的 ResourceDomain revoke → replace → restart。

**怎么映射**：
- 每个 Component 的 ResourceDomain = 一个轻量"域"：它拥有的 Handle 集合由 Core 记录；
- 驱动（如 VirtIO block）作为 Component 实现并支持替换，直接参考 RedLeaf 的驱动恢复。

**不照搬**：它的域间通信语言设施（语言内 channel 等）；我们第一阶段组件间只是 Rust direct call，不引入新通信机制。

---

## 8. Theseus（论文）

> *Theseus: an Experiment in Operating System Structure and State Management*（Rice University + Yale University, OSDI 2020）

**是什么**：用 Rust 写的 OS，把内核拆成**细粒度组件**（cell），每个组件明确声明自己管理哪些状态（state management）。支持**在线演化（live evolution）**：运行中替换组件、更新系统，无需重启。单地址空间（single address space），组件间通过所有权转移传递状态、降低锁依赖。

**借鉴什么**：
- **细粒度组件 + 明确的状态归属**："哪个组件拥有哪份状态"必须在结构上清晰 —— 我们 Core/Component 状态清单（谁存 Task 真相、谁存 runqueue）就是这种思想的静态化；
- 组件生命周期与替换流程的建模（quiesce → 替换 → 恢复）—— 我们 Phase-1 替换模型的灵感来源之一。

**怎么映射**：
- 我们的"Core 存真相、Scheduler 存 runqueue、Buddy 存 free list"边界划分 = Theseus 的状态归属原则；
- 未来做热替换时，Theseus 的 live evolution 是主要参考。

**不照搬**：在线演化本身（第一阶段明确不做）；单地址空间无锁消息传递模型（我们现在也不需要）。

---

## 9. WebAssembly（Wasm）

**是什么**：可移植的虚拟 ISA + 沙箱执行环境。二进制模块通过 import/export 声明边界，运行时（Wasmtime 等）提供隔离、JIT/解释执行。

**借鉴什么**：
- **虚拟 ISA**：一份组件二进制跨架构运行（RISC-V / x86_64 / ARM64 / LoongArch）；
- **import/export 边界**：模块显式声明依赖什么、提供什么 —— 与 Component 的 requires/provides 天然对应；
- 沙箱隔离可作为 ExecutionDomain 的一种。

**怎么映射**：
- 未来：`driver.wasm`、`filesystem.wasm`、`game.wasm` 作为 Component 执行后端之一；
- Wasm import 表 ≈ requires 声明；export 表 ≈ provides 声明。

**不照搬（第一阶段）**：不实现 Wasm runtime。唯一要做的是**现在就把 Component contract 设计成不绑定 native Rust ABI**（Interface 是语义，传输是绑定策略）。

---

## 10. WebAssembly Component Model

**是什么**：Wasm 之上的组件互操作标准：WIT（IDL）、Interface Types、Canonical ABI，让不同语言编译的组件跨语言互调。

**借鉴什么**：
- 接口描述（WIT）与实现分离；
- Canonical ABI：一种传输策略的完整范例。

**怎么映射**：
- 未来若 Interface 需要跨 ExecutionDomain（Wasm 组件 ↔ native 组件），Component Model 是现成参考；
- 但现在**不引入 WIT/IDL**（第一阶段明确不做）—— Rust trait 就是我们的接口描述。

**不照搬**：WIT / IDL 工具链、Canonical ABI 实现 —— 等真的需要 Wasm 组件互操作时再引入。

---

## 11. eBPF

**是什么**：Linux 内核的受验证虚拟 ISA：用户态程序（bytecode）经**验证器**检查安全性（终止性、内存访问合法、无危险指令）后注入内核，JIT 编译执行，用于观测、网络、安全策略等。

**借鉴什么**：
- **受验证的安全内核扩展**：与 Wasm 路线并列的另一种"安全注入策略"；
- 验证器思想 ≈ Core 对组件行为的校验（虽然我们第一阶段不注入字节码，但"进入 Core 的每个动作都要过验证"的精神一致）。

**怎么映射**：
- 与 Wasm 对比：eBPF 验证器是静态的、受限的；Wasm 是通用的。未来选择组件执行后端时参考两者的权衡；
- 政策注入场景（如调度策略热更新）可以借鉴 eBPF 的"受限语言 + 验证 + JIT"链路。

**不照搬**：eBPF 指令集本身、验证器实现 —— 我们不做字节码注入（第一阶段）。

---

## 12. CHESS（论文）

> *Finding and Reproducing Heisenbugs in Concurrent Programs*（微软研究院, 2008）

**是什么**：系统化的并发测试框架：用**确定性调度**（controlled interleaving）系统地探索线程交错，重现"偶发 bug"（Heisenbug），而不是随机调度碰运气。

**借鉴什么**：
- **确定性并发测试**：调度器可以记录/重放交错序列；
- 把"随机跑一遍"变成"系统地跑所有关键交错"。

**怎么映射**：
- 未来做 Test Scheduler / Hunt Mode：CoreTest 中用一个特殊调度器确定性重放交错，验证 Core 不变式在并发下不被破坏；
- 这也与 trace（记录事件序列）结合：先记录、后重放。

**不照搬**：完整模型检查基础设施；CHESS 的搜索算法实现 —— 第一阶段只要 trace + 简单重放能力。

---

## 13. FSCQ

**是什么**：用 Coq 形式化验证的文件系统（MIT CSAIL / PDOS, SOSP 2015），证明其实现满足规范，包括**崩溃一致性**（任意时刻断电，文件系统仍一致）。

**借鉴什么**：
- 对 Component contract 的强验证：一个复杂组件（文件系统）可以被形式化地证明满足规范；
- **崩溃一致性**作为文件系统组件质量标准的参考。

**怎么映射**：
- 未来若要对某个关键 Component（如 Core 的 handle 管理、或文件系统组件）做更强验证，FSCQ 是方法论参考；
- 现阶段只借鉴它的思想：**每个 Component 应该有一份清晰可验证的 contract 描述**（在 Interface 文档中）。

**不照搬**：Coq 证明工作量、证明基础设施 —— 第一阶段不做形式化证明。

---

## 14. Kani / Loom / Miri / Verus

**是什么**：Rust 生态的验证/检查工具链：
- **Kani**：基于模型检查（model checking），自动探索函数所有可能输入路径，验证 panic 自由、断言成立等；
- **Loom**：并发探索（concurrency exploration），系统地探索线程交错；
- **Miri**：解释执行 Rust，检测未定义行为（UB）—— 尤其是 `unsafe` 代码；
- **Verus**：Rust 的演绎验证（deductive verification）语言扩展。

**借鉴什么**：
- 未来 Developer-First 测试工具链的四个层次：模型检查（Kani）、并发探索（Loom）、UB 检查（Miri）、演绎验证（Verus）；
- 特别适合验证 Core 的不变式（handle 生命周期、资源所有权规则）。

**怎么映射**：
- Core 的关键逻辑（任务状态机、handle 生命周期、资源域回收）先用普通 host test，成熟后加 Kani/Miri；
- 并发部分（Core 被多个 CPU 访问）未来用 Loom 探索。

**不照搬**：不在第一阶段引入任何验证工具链依赖 —— 先保证代码可以用普通 `cargo test` 测试。

---

## 使用建议

1. **动手写之前**：读一遍 `core-philosophy.md` 和 `architecture.md`，对照本表的"对应设计点"列。
2. **设计某个具体机制时**（如 Handle、生命周期、替换流程）：先看对应条目的"借鉴什么/不照搬"，避免重复发明或过度设计。
3. **第一阶段**：只需要 Asterinas（策略验证）、seL4（typed authority）、Exokernel（保护/管理分离）、Theseus（状态归属）这四条作为主要思想来源，其余条目留作未来参考。
4. 本文件是活文档：每深入一个方向（如 Wasm、IPC、验证），就把对应的参考条目写详细。