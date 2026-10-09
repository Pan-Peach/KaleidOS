# 参考资料与借鉴方向（references.md）

> 本文档整理 KaleidOS 设计时可以对照的参考项目、论文与工具链。
> 每个条目说明三件事：**它是什么**、**我们借鉴什么**、**怎么映射到 KaleidOS 的具体设计点**（以及明确不照搬什么）。
> 本文档是"设计时的阅读清单"，不是实现清单 —— 第一阶段不要求读懂全部，用到哪一块再深入哪一块。

---

## 总览表

| 参考 | 类型 | 借鉴的核心 | 对应 KaleidOS 设计点 |
|---|---|---|---|
| Asterinas | 开源 OS（Rust） | 安全策略注入、策略输出校验 | Core/Component 分离、Policy proposes → Core validates |
| seL4 | 微内核 OS（形式化验证） | typed capability、资源真相在核心 | 资源身份 / 所有权记账、内核对象 |
| Exokernel | 论文（经典） | 保护与管理分离、底层资源暴露 | Core 保护资源、Component 决定策略 |
| SPIN | 论文（经典） | 语言级安全的内核扩展 | 类型安全组件运行在内核地址空间 |
| Singularity | 研究 OS（微软） | 软件隔离、契约式通信 | 不依赖硬件地址空间的组件隔离 |
| Inferno | 研究 OS（Bell Labs） | Dis 虚拟 ISA、可移植程序 | Wasm 作为组件执行后端的先例 |
| RedLeaf | 研究 OS（Rust） | 语言级隔离、域间所有权与驱动恢复 | 跨组件借用/状态归属研究；不直接等同 ResourceDomain 回收 |
| Theseus | 研究 OS（Rust） | 细粒度组件、状态管理、在线演化 | 组件生命周期、替换/恢复模型 |
| WebAssembly | 标准/运行时 | 虚拟 ISA、沙箱、导入导出 | 未来的组件执行后端（v0 不实现） |
| Wasm Component Model | 标准 | WIT、Interface Types、Canonical ABI | 未来 Interface 跨 ABI 参考 |
| eBPF | 内核机制（Linux） | 受验证的虚拟 ISA、安全内核扩展 | 与 Wasm 路线对比、策略注入参考 |
| CHESS | 论文（微软） | 确定性并发测试 | 未来 Test Scheduler / Hunt Mode |
| FSCQ | 论文 | 文件系统验证、崩溃一致性 | 未来对 Component contract 的强验证 |
| Kani / Loom / Miri / Verus | 工具链 | 模型检查 / 并发探索 / UB 检查 / 演绎验证 | 未来 Developer-First 测试工具链 |
| Zephyr | 开源 RTOS | arch / SoC / board / device model 的硬件边界 | `Machine Discovery`、设备描述、驱动 Component |
| Linux 内核模块（`rmmod` / livepatch） | 内核机制（Linux） | 引用保活与 callback 排空 | 停止与物理回收分别论证（§17） |
| CAmkES | seL4 组件框架 | 同址 Direct 与隔离 RPC connector | Contract / Transport / 部署分离（§18） |
| Genode | 组件化 OS 框架 | 父级路由、服务 Session 与通信 | 建连控制路径与数据路径分离（§19） |
| Fuchsia DFv2 | 驱动框架 | node / 匹配 / host / 实例 / dispatcher | 多设备组合与服务并发纪律（§20） |
| MINIX 3 | 微内核 OS | 静止点、状态迁移、回滚 | Restart / Recovery / Live Update 分离（§21） |
| FlexOS | ASPLOS 2022 论文 | 可选择隔离布局与负载实验 | 同一负载跨部署的保护/性能比较（§22） |

---

## 1. Asterinas

**是什么**：用 Safe Rust 编写的开源 OS（目标是 Linux ABI 兼容），强调"安全策略注入"：把调度器、内存分配器等策略移出可信核心（TCB），但底层 framework 保留最终验证权。核心设施是 OSTD（类似内核版的 std）。架构称"framekernel"：TCB 极小且 sound，其余为 Safe Rust 服务。另有 USENIX ATC 2025 论文 *Asterinas: A Linux ABI-Compatible, Rust-Based Framekernel OS with a Small and Sound TCB*。

**借鉴什么**：
- 调度器 / 分配器等策略组件可以移出可信核心，由"框架"注入；
- 策略组件的输出（比如"运行这个任务"）必须经过底层框架的**验证**才能生效 —— 这正是我们 `Policy proposes, Core validates and commits` 的直接先例；
- 用 Safe Rust 约束 TCB 规模：Core 越小，可证明/可审查的部分越少。

**怎么映射**：
- 我们的 SchedulerPolicy 组件对应策略注入；物理帧分配器是 canonical Core 内部机制，无 FrameAllocatorPolicy 组件；
- 我们的 Core 校验路径（任务是否存在、是否 Runnable、是否在别的 CPU）≈ 它的 policy output validation。

**不照搬**：Linux ABI 兼容目标；OSTD 那样庞大的内核基础库 —— 我们第一阶段 Core 词汇表保持最小。

---

## 2. seL4

**是什么**：形式化验证的微内核，以 capability 为核心：一切内核对象（task、frame、IRQ、IPC endpoint...）都通过不可伪造、不可混淆的 capability 访问，授权粒度极细（typed capability）。

**借鉴什么**：
- **typed capability / 资源身份**：设备与执行域资源有明确类型，不能把一种资源当成另一种；内存映射采用 Core 管理的 region/address-space 语义，不把每个 frame 暴露成组件资源；
- **资源真相在核心**：capability 只能由内核创建和传递，用户无法构造。我们只借用"资源存在性 / 所有权 / 生命周期由 Core 记录"的思想，**不实现 capability 系统**——KernelNative 不做 per-access 鉴权，访问强制交给执行域（`driver-model.md` §1.1）；
- 内核对象（kernel object）作为资源存在性/所有权记录在核心 —— 对应我们的设备表 / IRQ route 表 / DMA 表。

**怎么映射**：
- KaleidOS 的 `DeviceId`（identity）+ `kcore_device_claim`（记 owner 并返回本执行域访问窗口）是"资源在 Core 有真相"的简化形态，但**不是** capability——没有 per-access 校验，也没有"不可伪造"承诺；
- Core 记录设备 / IRQ route / DMA mapping 的 owner 与生命周期，用于独占、unload、失败清理与 teardown。

**不照搬**：完整 capability 系统（派生、revoke 树、badge 等）、形式化证明、IPC endpoint 体系 —— 第一阶段只做"资源身份 + Core 归属记账"，不做 capability 派生/撤销树。

---

## 3. Exokernel（论文）

> *Exokernel: An Operating System Architecture for Application-Level Resource Management*（MIT, 1995）

**是什么**：经典论文。核心主张"保护与管理分离"：内核只做保护（protection），把资源管理策略（management）下放到应用层 LibOS。内核极薄，只强制资源所有权与访问控制。

**借鉴什么**：
- **Protection vs Management 分离**：这正是"Core 保护资源，Component 决定策略"的源头；
- 向低层组件暴露接近硬件的资源接口（KaleidOS 用 Core 的 device claim 返回本域访问窗口：KernelNative = 裸寄存器基址，同时对设备/IRQ/DMA 做归属记账）。

**怎么映射**：
- Core = 保护层（资源存在性、所有权、权限）；Component = 管理层（调度策略、分配算法）；
- Exokernel 的"每个应用有自己的 LibOS" ≈ 我们的"每个 Profile 组合自己的 Component Graph"。

**不照搬**：把裸物理地址/裸中断直接暴露给不受信代码 —— KernelNative 是可信代码，可以直接拿访问窗口；真正不信任的组件交给未来的执行域（私有 AS + 页表）强制。

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
- 组件只能通过 Interface（trait）和 Core mechanism（device claim / IRQ route / DMA mapping）与外界交互。

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

[RedLeaf: Isolation and Communication in a Safe Operating System](https://www.usenix.org/conference/osdi20/presentation/narayanan-vikram)，OSDI 2020。
论文以 Rust 类型/内存安全、域边界与跨域所有权支撑语言级隔离、零拷贝和驱动恢复。

借鉴跨组件借用/转移的有效期与故障收尾；不把 Rust 编写、HeapState 分离或 Core
归属记录等同于论文的隔离机制。KaleidOS 的 C/FFI、裸指针与 MMIO 不受安全 Rust
类型系统自动约束，ResourceDomain 只是 owner 视图。暂不添加语言隔离执行域，
也不引入共享 Rust runtime。相关研究判断见 [服务研究](../development/service-runtime-study.md)。

---

## 8. Theseus（论文）

[Theseus: an Experiment in Operating System Structure and State Management](https://www.usenix.org/conference/osdi20/presentation/boos)，OSDI 2020。
论文通过明确运行时组件边界、减少组件为彼此持有的状态，支持 live evolution 与故障恢复。

借鉴对 state spill 的审查：FS 失败后，Core 管 publication，VFS 管 mount/open 对象，
personality 管 fd 错误与进程策略；不让 Core 操作这些业务表。现有窄 C ABI、驻留
backing 与裸 Direct 表没有因此获得动态演化能力。在线替换需要另外证明引用、静止点
与状态转换，不照搬其 Rust 装载/链接模型或推断所有 panic 都可安全恢复。

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
- 特别适合验证 Core 的不变式（设备 / IRQ route / DMA mapping 归属生命周期、资源所有权规则）。

**怎么映射**：
- Core 的关键逻辑（任务状态机、设备/IRQ/DMA 归属生命周期、资源域回收）先用普通 host test，成熟后加 Kani/Miri；
- 并发部分（Core 被多个 CPU 访问）未来用 Loom 探索。

**不照搬**：不在第一阶段引入任何验证工具链依赖 —— 先保证代码可以用普通 `cargo test` 测试。

---

## 15. RT-Thread

**是什么**：国内广泛使用的开源嵌入式 RTOS（C 语言）。从早期 RTOS 演化为带**组件生态**（文件系统、网络、GUI、驱动框架等）的实时操作系统；组件通过 Kconfig 选配后**静态编译进镜像**，另有动态模块（ELF .mo，类 dlopen）加载机制。

**对比（形似神不似）**：

| | RT-Thread | KaleidOS |
|---|---|---|
| 组件 | **编译期** Kconfig 勾选，链进一个镜像 | **运行期** 加载（.kcomp ELF 可重定位）+ 权威分离 |
| 资源模型 | 平级"聚合"（内核与组件无边界） | **Core owns truth**（资源真相与归属记账在内核，组件拿到本域访问窗口） |
| 隔离 | 同地址空间，全部裸 API | 组件只能调白名单（本阶段=约定；隔离=未来执行域） |

**借鉴什么（具体机制，不是整体哲学）**：
- **实时调度器**：优先级抢占 + 同优先级轮转/时间片——我们未来 scheduler 组件的参考实现（算法层面，policy 仍由组件提交、Core 验证）；
- **动态模块机制**：它也有 ELF 模块加载，看它的模块清理链（卸载顺序）与我们的 `kcomp_exit`（**已删除**，现为 `kcomp_instance_destroy`）/卸载协议互相印证；
- **设备框架交互**：设备注册（rt_hw_* / 串口框架、I2C 框架）的"注册-分发"模式，对照我们的 Interface（trait 语义）+ device claim / IRQ / DMA mechanism 设计。

**不照搬**：它的整体"嵌入式通用内核聚合"架构（我们保持小、权威边界清楚）；Kconfig 组件选择心智（那是构建期配置，我们坚持运行时组件图）；BSP/板级模板堆叠（我们保持 bootstrap+core 单镜像）。

---

## 16. Zephyr：硬件模型与设备边界

**是什么**：面向多架构、众多 SoC 与开发板的 RTOS。这里重点参考它的
Architecture Porting Guide、RISC-V port、SoC/board 组织方式，以及 Device Driver
Model；不把它的 scheduler 或 RTOS API 当作 KaleidOS 的架构来源。

### 借鉴一：把硬件差异拆成不同问题

Zephyr 最有价值的提醒是：以下几件事不能混成一个 `hal/<board>.rs`：

```text
Architecture   = ISA / ABI 原语：trap、context、MMU/MPU、atomic、idle
SoC            = 芯片能力：中断控制器、clock/pinctrl、memory map
Board          = PCB 连接：RAM/flash、外设实例、连线和描述
Device driver  = 某个设备协议/实现：通过 generic API 提供语义
```

映射到 KaleidOS：

| 硬件模型概念 | KaleidOS 落点 | 边界要求 |
|---|---|---|
| Architecture | `os/arch` | 只放 ISA/特权级/翻译与 CPU 原语，不按板卡分支 |
| SoC / Board facts | Machine Discovery → `MachineInfo` / `DeviceDescriptor` | 归一化机器事实，Core 不知道来源是 FDT、ACPI 还是其他 backend |
| Device identity | `DeviceId` | 只能发现/选择，不是权限，不携带地址或 IRQ 权限 |
| Device driver | `os/components/drivers/` | 驱动是 Component；经 Core 的 device claim（本域访问窗口）+ IRQ / DMA mechanism 访问硬件 |
| Generic device API | Component Interface | 表达 BlockDevice/UART 等语义，不暴露板卡地址，也不绑定 transport |

这意味着 **VisionFive 2 不是一种 RISC-V**：RV64 是架构，JH7110 是 SoC，
VisionFive 2 是 board。未来增加板卡时，不能把板卡名渗透到 Core 的资源语义或驱动
接口中。

KaleidOS 当前**不建立独立 `platform` crate**，这是有意的实现取舍，而不是否认
上述分类：概念上仍然区分 arch / SoC / board；代码上先由 Machine Discovery
backend 把 SoC/board 事实归一化为 `MachineInfo`。只有当发现 backend 或平台特例
形成稳定、可复用的机制时，才增加窄的模块/目录；platform quirks 仍应是 escape hatch，
不是默认层。

### 借鉴二：按能力描述机器，不按板卡名称写条件分支

不应把机制写成：

```rust
if board == VisionFive2 { /* Sv39 / 某个 UART / 某种 IRQ */ }
```

而应让构建配置和机器发现分别表达能力：

```text
build/profile capability：XLEN、特权级、MMU/NoMMU、已选择的 backend
runtime machine fact：CPU topology、RAM、MMIO/PIO、IRQ、timebase、设备实例
future protection capability：PMP/MPU、IOMMU、DMA isolation 等
```

当前映射是 `.config` / Kconfig 负责 build/profile 选择，`MachineInfo` 负责启动时
提交的机器事实；`driver-model.md` §11 的 capability honesty 负责限制执行域承诺。
未来若引入 `ArchCapabilities` 或等价快照，应保持它是窄的 capability contract，
不能变成把所有板卡差异塞进 Core 的大枚举。新增机制优先依赖能力谓词，而不是
`#[cfg(feature = "某块板")]`。

### 借鉴三：设备描述、设备实例、驱动实现和设备语义要分开

Zephyr 的 device model 提醒我们，静态设备描述/API table、具体 driver instance
和 generic API 是不同层次。KaleidOS 采用更适合运行期组件图的版本：

```text
MachineInfo / DeviceDescriptor   -- 发现到的事实
        ↓
DeviceId                          -- 纯身份，选择候选
        ↓ Core claim
本域访问窗口（裸 MMIO 指针 / mapped VA）-- mechanism，非 capability
        ↓ driver 自己 volatile 访问
Driver Component                  -- 私有协议状态与 runtime data
        ↓ publish/bind
Device Interface                  -- BlockDevice/UART 等语义
```

因此：

- Core 记录设备存在性、资源范围、所有权和生命周期；不把 UART/virtio/NVMe 协议
  塞回 Core；
- generic API 是 Interface 语义，跨边界时用窄的 typed function table；不能让 Rust
  trait-object ABI 或裸 MMIO 地址成为设备模型；
- driver instance 的 runtime state 归组件，设备描述的权威事实归 Core；
- `DeviceId` 只负责发现，`kcore_device_claim` 才建立所有权并给出访问窗口；不能用设备号、地址或 IRQ 号
  自报身份；
- 这不意味着要建立一个庞大的统一 driver framework。匹配、probe、组件加载和
  Interface binding 仍按现有 Component Manager / prober 模型组合。

### 借鉴四：吸收硬件边界，不吸收“全部编译期解决”

Zephyr 的 Kconfig + Devicetree + build system 很适合为一个确定硬件生成固件，
但不能直接成为 KaleidOS 的整体哲学。KaleidOS 保留：

- Machine Description 与 FDT/ACPI 等具体来源分离；
- boot-time discovery → Core validation/commit 的硬件事实链；
- 窄 generic device API 与独立 driver implementation。

KaleidOS 不照搬：

- 把所有设备实例固定成静态全局 device；
- 用板卡配置取代运行期 Component Graph；
- 把 Kconfig、Devicetree、CMake/west 组合成一个新的强制框架；
- 为了模仿 Zephyr 的目录而提前建立 platform/SoC/board 大层级；
- Zephyr 的 scheduler、线程 API 或 RTOS 语义。

当前阅读入口：

- [Zephyr Architecture Porting Guide](https://docs.zephyrproject.org/latest/hardware/porting/arch.html)
- [Zephyr RISC-V architecture notes](https://github.com/zephyrproject-rtos/zephyr/blob/main/doc/hardware/arch/risc-v.rst)
- [Zephyr Device Driver Model](https://docs.zephyrproject.org/latest/kernel/drivers/index.html)
- [Zephyr board porting guide](https://github.com/zephyrproject-rtos/zephyr/blob/main/doc/hardware/porting/board_porting.rst)

---

## 17. Linux 内核模块：引用保活与回调排空

[Driver Basics](https://www.kernel.org/doc/html/latest/driver-api/basics.html) 规定：
`try_module_get` 在移除期间拒绝新引用，`module_put` 归还引用；获取引用前还须有
保证模块仍存活的保护，不能先使用悬挂指针再增加计数。
[RCU and Unloadable Modules](https://docs.kernel.org/RCU/rcubarrier.html) 说明
`synchronize_rcu` 等待 grace period 不等于异步 callback 完成；卸载前须停止新 callback
并以对应 barrier 排空。

| 需要分别证明的条件 | KaleidOS 的判断 |
|---|---|
| 不再接受新引用 | Stopping 的准入；当前无 Direct acquire/release 协议 |
| 长期引用已归还 | 已交付 api/ctx 不可追回；Native 非空 Direct publication 保守拒绝 Stop |
| 执行已结束 | Core-managed inflight 仅覆盖 Core 可见调用；另查 Task 与 callback |
| 设备不再访问 backing | IRQ 撤销不代表 DMA 静默；失败 backing/device quarantine |
| 可物理释放 | 前述条件不能只用 refcount=0 替代；当前不承诺完整回收 |

Linux module 不必是线程；服务选择 Inline 或 Worker，与代码装载分别考虑。
可以研究可选长期 binding 引用协议，保留 Direct 稳态零 Core 边界；不立即引入全局 Arc、
每次调用计数、RCU 框架或强制卸载。KaleidOS 同名 artifact 已可多实例，新实例有新
ComponentId；没有「先回收名字槽才能重启」这一前置。
[livepatch](https://docs.kernel.org/livepatch/livepatch.html) 是另一个行为切换课题，
不能等同于卸载/重装；本项目不因此采用 text patching。
现行停止与回收边界以 [组件生命周期](../architecture/component-lifecycle.md) 为准。

---

## 18. CAmkES：接口与连接方式

[CAmkES Manual](https://docs.sel4.systems/projects/camkes/manual.html) 的 assembly/connector
把组件实例与连接分别描述；seL4DirectCall 只用于 colocated 实例，否则报错。Direct
返回指针的堆归属也可能与隔离 RPC 不同。
[seL4 IPC tutorial](https://docs.sel4.systems/Tutorials/ipc.html) 则描述线程间 Call/Reply。

借鉴 Contract 与 Transport 的分离、组合者对通信需求的选择。KaleidOS Gate 是当前
同步服务栈调用，不能等同于独立 Server Thread receive/reply。不照搬静态 ADL，
transport preference 仍需真实消费者论证，Core 的合法性验证不能交给 SDK 绕过。

## 19. Genode：路由与 Session

[Recursive System Structure](https://genode.org/documentation/genode-foundations/25.05/architecture/Recursive_system_structure.html)
说明父级对 session 请求选择/拒绝/转发；
[Inter-component Communication](https://genode.org/documentation/genode-foundations/25.05/architecture/Inter-component_communication.html)
区分 RPC、通知与共享内存，服务端 RPC session 与客户端 connection 分别承载状态。

借鉴建连时路由、高频调用使用已建立服务引用；业务 open/socket/stream 状态由服务
保存，不要求每个对象成为 Core endpoint。不复制其 quota、capability 图或第二套服务
注册中心；KaleidOS 的 EndpointId 不自动具有 Genode session capability 的权限语义。

## 20. Fuchsia：设备、实例与执行环境

[DFv2](https://fuchsia.dev/fuchsia-src/concepts/drivers/driver_framework) 区分 node、Driver Index、
Driver Manager、Driver Host 和 driver instance；多个驱动可同 host 共址。
[Dispatcher and threads](https://fuchsia.dev/fuchsia-src/concepts/drivers/driver-dispatcher-and-threads)
说明 dispatcher 的异步工作与线程纪律由 driver runtime 实施。

借鉴发现→匹配→实例关联→部署的分步职责，以及 transport 不等于并发策略。
先用组件侧 DeviceId→实例/endpoint 记录检验双盘，不建立完整 Driver Manager 或共享
host 执行域。现有 ComponentRecord 与私有 AS 的实现不因此改变。

## 21. MINIX 3：更新需要静止、迁移和回滚

[Live update](https://wiki.minix3.org/doku.php?id=developersguide:liveupdate) 记录 RS 先使旧服务
到已知静止点，新实例转移/调整状态，成功切换、失败回滚；该机制有编译与运行时支持。

借鉴 Restart、Recovery、Live Update 的分离。KaleidOS instantiate 不迁移 socket、
打开引用或设备状态；故障驱动 quarantine 不可由新实例清除。暂不搬 RS、状态迁移工具链
或宣称透明恢复；先验证旧对象失败与显式建立新连接。

## 22. FlexOS：用真实负载比较部署

[FlexOS: Towards Flexible OS Isolation](https://arxiv.org/abs/2112.06566)，ASPLOS 2022，
以模块化 LibOS 在编译/部署时选择隔离和数据共享配置，使用 Redis/Nginx/SQLite 评估
设计空间。它的 compartment 配置不等同于 KaleidOS 的动态实例生命周期。

借鉴实验方法：固定业务语义与负载，改变合法部署/调用组合，同时报告延迟、吞吐、
数据搬运/栈成本及实际保护条件。不因 K/K、K/I、I/K、I/I 合成服务矩阵通过，就宣称
真实 FS/驱动可任意跨域部署。实验入口见 [服务研究 §4](../development/service-runtime-study.md#4-真实纵向负载与前置)。

---

## 使用建议

1. **动手写之前**：读一遍 `docs/philosophy/core-philosophy.md` 和 `docs/architecture/overview.md`，对照本表的"对应设计点"列。
2. **设计某个具体机制时**（如 device claim、生命周期、替换流程）：先看对应条目的"借鉴什么/不照搬"，避免重复发明或过度设计。
3. **第一阶段**：主要看 Asterinas（策略验证）、seL4（typed capability / 资源真相）、Exokernel（保护/管理分离）、Theseus（状态归属）；涉及硬件边界和第一个驱动时，优先补看 Zephyr，其余条目留作未来参考。
4. 本文件是活文档：每深入一个方向（如 Wasm、IPC、验证），就把对应的参考条目写详细。
