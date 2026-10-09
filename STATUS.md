# KaleidOS 状态与计划

更新：2026-10-09。基线 develop `2e10304c389f` 已有 KernelNative IPC、混合
Local/Remote Fat VFS、virtio Block IPC-only 与 ksh/ELF 主链。用户授权后已实现
Echo/Block/Filesystem scalar/buffer 方法生成并接入真实服务，修复已审计回归；完整 check/host/
QEMU/Arch 门禁通过。普通旧通道安全退出仍未完成，见§3.29。Isolated新IPC、Sandbox、
通用VFS文件fd/libc startup和完整物理回收未实现，K链不证明私有域能力。

RV64 已进入真实 KernelNative 组件任务调度：每 CPU containment、固定 CPU 放置、Core 原子提交、远端 park/wake、AP idle 调度与 BSP 安全点均已接线。职责定案见 `docs/architecture/scheduling.md`。不包含 work stealing、迁移、抢占或第二 ISA 调度。

本文是审计快照加计划，不是设计文档。事实以代码、测试、构建配置为准。README 和设计文档里写了但代码没有的，按未实现记。

本文同时是状态与路线图的唯一入口：原 `docs/development/` 下的路线图文档已并入本文并删除（依赖链见第 6 节，硬件见第 7 节，生态与调包见第 8 节，明确未做见第 11 节）。

## 0. 一句话

KaleidOS 是一台能在 QEMU 启动、能交互观察、能加载 `.kcomp` 组件的 RISC-V 内核（RV64 支持协作式 SMP）。Core 的基本词汇（`TaskId` / `PhysicalRange` / `ComponentId` / `DeviceId` / `EndpointId` / `ExecutionDomain`）已经立住，`propose → validate → commit` 路径可用。现在还没有多个差异足够大的上层负载来把 Core 逼到定型，所以离 freeze-candidate 还有距离。block/wake 原语已可用；RV64 普通用户 task 已关联私有 AS；通用 VFS 文件 fd / libc startup、组件 SandboxedNative 后端和物理回收仍是主要缺口。

底座侧 RISC-V 的 CPU 身份、启动、IPI 与 per-CPU 执行现场已接通组件调度。第二 ISA 的现状见模块文档，本轮不扩展它的实现或测试范围。

## 1. 图例：状态与进度条

以下五个词在本文里始终按英文原词使用。

| 状态 | 含义 | 进度条 |
|---|---|---|
| IMPLEMENTED | 代码存在、已接线，能走真实入口执行，不是注释、`todo!()` 或仅 host 的替身 | `▰▰▰▰▱` |
| VALIDATED | 有可复现的验证证据并注明层次（host / CoreTest(QEMU) / ArchTest(QEMU) / 真机） | `▰▰▰▰▰` |
| EXPERIMENTAL | 部分路径能跑，覆盖、边界、失败路径、长期稳定性未证明，接口仍可能原地改 | `▰▰▰▱▱` |
| PLANNED | 有设计或方向，没有实现代码 | `▰▱▱▱▱` |
| NOT IMPLEMENTED | 没有代码，或只有占位：`todo!()`、未接线的 enum 成员、显式拒绝的分支 | `▱▱▱▱▱` |

进度条是从状态直接映射的定性指示，不是测量出来的百分比，也不要当百分比读。每个模块的条取它最弱的一环，所以"实现很全但只在 QEMU 跑过"的模块不会满格；现在没有任何模块满格，每个模块的 Status 都还没到 VALIDATED。一个通过的用例只证明它覆盖的那条路径，不等于模块整体被验证。

## 2. 系统总览

```text
Applications / System Personality        ksh 可 exec 静态 RV64 ELF；最小 POSIX fork/exec/wait；Win32/WASI 是未来
        │
Services / Devices（组件图组合的产物）     最小 FS 服务已有（fatfs/littlefs）；VFS Local 对象可用，服务未接线
        │
Components（策略/服务/驱动 .kcomp）         scheduler_rr, driver_prober, virtio_blk, fatfs, littlefs
        │
Resource Core（机制 + 所有权真相）           task/sched/memory/resource/component/irq/timer
        │
Arch + Machine Discovery                    arch(riscv/fake/nommu 已实现；x86_64/aarch64/loongarch64 骨架；SMP 契约) + boot 的 FDT→MachineInfo
        │
Hardware                                    目前只有 QEMU virt
```

组件边界只走 `Endpoint`（Contract + exact ABI + `EndpointId`），不建 flat ELF 符号表。跨组件调用的机制由 Core 在 bind 时按 `(caller domain, callee domain)` 选定。

## 3. 模块状态

### Core 机制

#### 3.1 物理内存 `▰▰▰▰▱` IMPLEMENTED

现状：buddy `MetadataHeap`（O(1)），`alloc_region`/`free_region`，`MemoryLease` 只是 Core 内部 RAII，没有内存账本。host 测了 `init_region_alloc_and_free`、`alloc_after_free_reuses`、property `size_and_address_helpers_hold_invariants`；QEMU CoreTest 有 `memory`。

缺口：phase 1 不承诺物理回收；没有碎片基准；Isolated 的 region backing 与回收路径只在 QEMU 走过。

下一步：补碎片基准；回收接口等 ExecutionDomain 完整化后再做；未来 `MemoryPolicy` 只能提议偏好（NUMA、配额），验证与提交留在 Core。

#### 3.2 地址空间 / VM `▰▰▰▰▱` IMPLEMENTED（私有 AS 部分 EXPERIMENTAL）

现状：`KernelAddressSpace` 是语义 ledger，`AddressSpaceBackend` 是 contract。`Sv39PageTable`/`Sv32PageTable` 在 host 上直驱生产实现，覆盖 map/unmap/translate、mid-map 回滚、`mapping_exact`、共享与私有别名排除；QEMU ArchTest 覆盖 `mapping`、`load-fault`、`store-readonly`、`execute-nx`、`tlb-flush`、`tlb-invalidate`。NoMMU 走 identity，`GRANULE=1`。

缺口：RV64 user AS 已接普通进程；没有 COW/lazy 或 mmap（这些属于 personality）；NoMMU 的 protection（PMP/MPU）没落地；`adopt`（接管 boot root）有代码但没接线。

下一步：复用普通 user AS 机制接 SandboxedNative 组件后端；PMP/MPU 跟 NoMMU 启动一起做；`adopt` 等 boot `vm/runtime.rs` 重构时接入。

#### 3.3 任务对象 `▰▰▰▰▱` IMPLEMENTED

现状：`TaskId`、`TaskTable`、owner、kernel stack、状态机（`Created`/`Runnable`/`Running(CpuId)`/`Blocked`/`Exited`，合法边 6 条）。host 覆盖非法迁移、owner、`EntryOutOfImage`、property `random_transition_sequence_preserves_task_truth`；QEMU 有 `task-create`/`task-switch`/`task-exit`，ArchTest 有 `task-panic`。

缺口：`TaskRecord` 与 `AddressSpaceId` 没有绑定，`Running(CpuId)` 没有 AS 概念。

下一步：task 与 AS 的绑定语义（当前调度只承诺 KernelNative 固定 CPU）。

#### 3.4 park/unpark 原语 `▰▰▰▰▱` IMPLEMENTED

现状：每任务一份 pending permit、owner 校验、Blocked → Runnable 已接入真实任务调度。最终 permit 检查与 block commit 共用一个 task table 事务，覆盖跨 CPU 在策略执行期间 unpark 的竞争。wake 提交后通知目标 CPU。host 确定性测试与 property 测试覆盖状态模型；RV64 SMP 真实组件跑 128 轮远端 park/unpark。

缺口：没有 task join、task-stop、Core event / waitqueue，也没有独立 TaskBlock / TaskWake trace。普通业务等待条件属于组件；Endpoint IPC等待者和完成谓词由Core Exchange管理（§3.29）。

#### 3.5 调度机制 `▰▰▰▰▱` IMPLEMENTED（协作式 SMP）

现状：每 CPU current / anchor / IRQ 状态和 containment；AP Online 后进入本 CPU 调度，BSP 在 monitor 空闲安全点服务工作。普通 start 固定到当前 CPU；start_on 接受 owner 的初始 CPU 请求，由 Core 验证 Online 并固定归属。调度事务复验 owner、状态、CPU 与 outgoing Running，快照失效重试。PolicyAccepted 只记录成功提交。scheduler_rr 保存每 CPU 游标，Core 不保存算法状态。

策略回调串行占用 Core policy 栈，忙时替换返回 EBUSY；错误提议 / panic 退役并使用已有确定性回退。host 并发测试覆盖同栈占用和每 CPU 身份；RV64 ArchTest 覆盖启动 / IPI / per-CPU；CoreTest 新增七项检查，覆盖真实组件并行、远端 wake、本地 RR、任务退出与 CPU0 / CPU1 双向 panic containment。职责契约见 `docs/architecture/scheduling.md`。

缺口：无抢占、迁移、work stealing、hotplug、私有 AS 任务调度；没有真机证明。其它 ISA 的调度不在本轮范围。

下一步：用真实驱动 / 服务的组件等待队列检验协作式 SMP；私有 AS 任务绑定与抢占单独定契约。

#### 3.6 时钟 / 抢占 `▰▰▰▱▱` EXPERIMENTAL

现状：`arm_deadline` 是一次性；`on_trap` 做 tick 计数与重编程。host 测了 `timer_lifecycle_covers_init_arm_trap_and_ticks`；QEMU ArchTest `timer` 证明 one-shot 恰好一次中断；monitor 空闲靠它唤醒。

缺口：抢占没落地也没接线。`timer::on_trap` 不调用 `sched::on_timer_tick`，后者本身是 `todo!("C5")`；`CONFIG_PREEMPT` 默认 n，没有任何 defconfig 打开。当前基线是纯协作式。

下一步：先定模型（延迟重调度标志，还是 trap 内直接切换），正面回答 `sstatus.SIE` 的保存恢复，然后接线并补 QEMU 用例。

#### 3.7 设备 `▰▰▰▱▱` EXPERIMENTAL

现状：`DeviceTable` 256 槽，owner 加 quarantine。`kcore_device_nth` 纯发现，`DeviceId` 是 identity，`kcore_device_claim` 记 owner 并返回本域 MMIO 窗口，`release` 与失败 `quarantine_owner` 到 reboot。host 覆盖独占、quarantine、PIO 拒绝；QEMU 有 `device-claim-magic`、`device-window-len`、`device-double-claim`、`device-release*`、`device-ordinal-miss`。

缺口：只在 QEMU virt 验证；没有真机；没有跨组件所有权转移（刻意推迟）；没有多 MSI-X 或共享线；`DeviceId` 只在单个已提交 `MachineInfo` 生命周期内有效。

下一步：真机 bring-up 时扩到非 virt 设备。共享线与 MSI-X 跟着 PCIe 一起做。

#### 3.8 MMIO `▰▰▰▰▱` IMPLEMENTED（KernelNative）

现状：`kcore_device_claim` 返回裸寄存器基址，driver 自己 `volatile` 读写，稳态不进 Core，没有 per-access 鉴权。QEMU 有 `device-write-readback`；`kbench` 量 `irq.uart_trigger_to_handler`。

缺口：Isolated 下把窗口映射进组件 AS 返回 VA 还没做；全局恒等 MMIO 映射让"移除派生映射"只具协作意义。

下一步：在 Isolated 执行域补 mapped VA 窗口（`driver-model.md` §12）。

#### 3.9 IRQ `▰▰▰▱▱` EXPERIMENTAL

现状：`IrqTable` 按二维 `(DeviceId, resource_index)` 锚定 route（设备中断资源下标），存 owner/handler/ctx；`register`/`enable`/`disable`/`release`/`revoke_owner`；投递走 trap→route→实例准入/在途计数→锁外 callback，回调带 IRQ 归属作用域；route 撤销不等待已取回调返回。固件 specifier（`InterruptSpecifier`：控制器 + 完整 cells）与逻辑 `line` 分离；RISC-V discovery 只把属于已配置 PLIC、source 在范围内的资源绑定成 `line`（AArch64/x86 `line: None`）。同一逻辑线换 key 重复注册 → `-EBUSY`（不做 shared-line fanout）。QEMU ArchTest `external-irq` 证明 PLIC 恰好投递一次 UART THRE 线；host 覆盖多资源独立路由、全宽 DeviceId、未绑定/越界/非 owner 拒绝与重复线拒绝。

缺口：polled、计数、掩蔽、ack 已删并推迟；回调内 panic 会致命；只在 QEMU/PLIC 验证；无 GIC/PIC/APIC 路由（对应资源保留 `line: None`）。

下一步：共享线与 MSI-X 跟 PCIe 一起（shared-line fanout 已明确不做）；ack/mask 等真机需求出现再补；回调 panic 并入 containment 的后续工作。

#### 3.10 DMA `▰▰▰▱▱` EXPERIMENTAL

现状：allocation（与设备无关）和 mapping（与设备相关）分离，`alloc`/`free`/`map`/`unmap` 齐备；map 插入持 device→dma 锁，release 不能漏掉新 mapping；失败 backing 进 Core 私有 `QUARANTINE`，不归还 buddy；mapping id 单调不复用。host 覆盖单调、quarantine、revoke；QEMU 有 `dma-ring`、`dma-invalid-size`，virtio_blk 真实走 `dma_alloc` 加 `map`。

缺口：Native map 按 device owner 记账、不检查 ambient caller，unmap 没有 caller 检查；普通借入 buffer 未 pin；没有 IOMMU，设备地址是 identity，不能声称 DMA 隔离；CPU 隔离不等于 DMA 隔离；没有 bounce buffer 或多 pool；`free` 后 backing 不归还，这是 correctness 决定，不是安全结论。

下一步：IOMMU 与 bounce/pool 等真机或安全需求出现再做；回收前提是设备静默。

### Arch 与启动

#### 3.11 Boot / 机器发现 `▰▰▰▰▱` IMPLEMENTED

现状：FDT → `MachineInfo` → `core::init` → monitor 全链打通。FDT 解析用 third_party 的 `fdt` crate。RV64 用 identity 加高半区双映射（Sv39），RV32 用 identity（Sv32）。`make test-qemu` 在 RV64/RV32 双 profile 有 boot smoke。

缺口：只认 FDT 和 QEMU virt；没有真机、板级 quirk、ACPI；M-mode/NoMMU 有入口与可编译配置，但 QEMU 尚未启动成功；固件/console 交接不自洽。RV32 S-mode/NoMMU 已在本次收敛审计用私有 profile 实跑 CoreTest，尚未进入默认门禁。

下一步：VisionFive 2 的 bring-up 从这里开始，串口出 `core>` 是第一步验收。板级差异集中在 boot，不进 Core。

#### 3.12 Arch 层 `▰▰▰▱▱` EXPERIMENTAL（层是 ACTIVE，不是 stable）

现状：backend trait（`CpuArch`/`Timer`/`InterruptController`/`Smp`/`Console`/`SystemReset`）加 `riscv`/`fake`/`nommu` 实现；多个 ISA 骨架；`CpuId`（逻辑）与 `HardwareCpuId`（硬件）分离（定义在 arch，Core re-export）；中断回调统一为 `LocalInterruptHandler = fn(CpuId)`；`InterruptController` 已原地改为 `Config`/`Claim` + `init_cpu`；`ComponentRelocationImpl` 按 ISA 选择。RISC-V backend 验证充分：host 直驱生产实现测编解码与 walk、`RiscvRelocator`；QEMU ArchTest 覆盖 trap、页表权限、context switch、timer、PLIC。

缺口：跨架构抽象**已有骨架但未被第二个 ISA 验证**——`os/arch/src/{x86_64,aarch64,loongarch64}` 与 `os/boot/<isa>` 已建（同形，实现体 `todo!()`），能编译、未启动、未验证；没有真机；机器差异欠验证。SMP 接口已收敛（`Smp` trait / `LocalInterruptHandler` / `InterruptController` 的 `Config`+`Claim` / `CpuArch::init_cpu`+`enable_irq`），但实现与 CPU-local 存储（`sscratch` 入口记录、per-CPU trap 栈）都是 `todo!()`，属协调 trap bring-up 的工作。RISC-V bring-up 进展可观，但层本身还在动。

下一步：RISC-V 协作式 SMP 已接线；第二 ISA 暂不扩展。新 ISA 的 ArchTest 入口已就绪但 opt-in（`make test-arch-{x86_64,aarch64,loongarch64}`，boot 未实现前会失败）；`.kcomp` 组件目前仍是 RISC-V 重定位专用，新 ISA 先用 Core-only 镜像。M-mode 补 boot harness 之后才有意义。

### 组件与执行域

#### 3.13 组件工件与 loader `▰▰▰▰▱` IMPLEMENTED

现状：`.kcomp`（ELF32/64 ET_REL）经 cpio `.initpkg` 内嵌，store 解析，loader 放段加重定位加入口校验，registry 生命周期。C 和 Rust 两个语言前端都能编。host 覆盖 ELF 解析、重定位、未导出符号拒绝、`same_artifact_loads_produce_independent_components`；QEMU RV64/RV32 机器级跑 `load`/`unload kcomp_c_smoke`。

缺口：每次 instantiate 重新放段与重定位，没有 text 去重；没有运行期依赖解析；manifest 只有名字列表，没有能力或支持范围字段。

下一步：manifest 加能力字段。依赖解析按 depmod 模式后置。运行期热插拔与 Runtime Graph 明确不做，除非出现需求。

#### 3.14 组件实例与生命周期 `▰▰▰▱▱` EXPERIMENTAL（模型是 ACTIVE DESIGN）

现状：一个 `ComponentId` 等于一个完整组件，`ComponentRecord` 直接持有 `loaded`；状态机是 `Declared→Resolved→Starting→Ready→Stopping→Stopped/Failed`。host 覆盖生命周期、停止与销毁、失败撤销、endpoint 永久失效；QEMU CoreTest `component-lifecycle`，ArchTest `isolated-lifecycle*` 与 `isolated-restart`。

"一个 `.kcomp` 到多个独立实例"是真实支持，不只是 ID 类型上可表达：host 断言两次 instantiate 的 backing 独立且镜像区间不重叠，CoreTest `driver-multi-device`，ArchTest `isolated-restart` 接受同 artifact 的并发第二实例（各自 AS、backing、窗口），`ram_blk_rw` 每实例独立 buffer。

Stop 在 registry 准入锁内检查 live Task 与 Gate/policy/IRQ 在途执行，再提交 Stopping；Native 发布过 Direct 表即拒绝 destroy（EBUSY），保留表与 ctx。公开 `kcore_component_stop` 复用相同编排。Checksum CoreTest 验证零 Task 的 Passive、多实例、Active/Hybrid Worker 与 RV64 跨 CPU Gate/Stop；普通 `kcore_task_create(entry,arg)` 已提供零到多个 owned Task，不需要另造 `kcomp_task` 生命周期。

缺口：记录与镜像留 tombstone，Direct 无 release 协议；没有 drain、Task join、自动物理回收与完整跨域任务矩阵。

下一步：出现真实物理回收需求后验证 drain、Direct 引用释放与 DMA 静默；私有域 owned Task 另按部署依赖接通，不增加第二套实例生命周期。

#### 3.15 Endpoint / Contract / Binding `▰▰▰▱▱` EXPERIMENTAL

现状：`ContractId` 加 exact ABI fingerprint 加 opaque `EndpointId`，绝不重定向；staged publish/lookup/discover/bind；bind 是 Core 选定机制的唯一选择点。host 覆盖 staged publication、abort 原子性、no-redirect、domain 矩阵、Gate/Direct 选择；CoreTest 有 scheduler/filesystem/driver 链；ArchTest 有 `isolated-service*`。

ABI 校验的落点要说清楚。`kcore_endpoint_lookup` 的发现路径不带 abi，只比 contract 与存活，交付 opaque capability。exact contract 加 abi 加存活的校验在 `kcore_endpoint_validate`（对已持有的 id，C ABI 可达，SDK `Endpoint<C>::from_id` 已接）和 `kcore_endpoint_bind`（第一步就走同一个 `EndpointRegistry::lookup`）。`deployment.md` §7.4/§11 已同步这一落点，裸 lookup 只发现 id。

现有 K/I 矩阵均已派发：K/K Direct、K→I / I→K / I→I Gate。SDK `block.device` 同一工件验证；Sandbox 仍显式拒绝。

下一步：新接口（NetDevice、Clock、RNG 等）继续走同一条 bind 路径；补齐各接口的扁平 wire adapter。

#### 3.16 执行域 `▰▰▰▱▱` EXPERIMENTAL

现状：`ExecutionDomain` 三个变体。KernelNative 完整并验证。IsolatedNative（S 加私有 AS）有真实的 create/destroy/service：私有 AS、共享 Core 映射（same VA→PA）、最小跨 AS trampoline（per-invocation context、satp 切换、全量 `sfence.vma`）、按域放段（页级权限）、Core 预置窗口、K/I 双向 service Gate、失败与重启清理；SDK 在 create 前选择 K 共享堆 / I 私有堆，同一 `kcomp_heap.kcomp` 验证 Rust/C 分配、增长、精确 release、多实例与重启。按验证层看是 PARTIALLY VALIDATED，能力止于 QEMU RV64+RV32 的 29 个 ArchTest case，不能再往上抬。

缺口：无 ASID（恒 0 加全量 flush）；无 U-mode/`ecall`，是协作式 S-mode 边界，不是对抗隔离；import 面支持诊断、只读查询、panic、私有 backing 与 endpoint 发布/发现/校验/绑定/调用；不能 claim 设备、注册 IRQ/DMA、建任务；没有压力或对抗测试；私有堆 backing 停止后驻留，自动回收仍需 drain / DMA 静默与 AS teardown。

下一步：两条路选一条。继续补 ASID、U-mode、任务/设备 import 面；或者明确冻结成教学实验，把资源投到 SandboxedNative 与真机。

#### 3.17 SandboxedNative（U-mode + syscall 边界） `▱▱▱▱▱` NOT IMPLEMENTED

现状：Core `component/sandbox.rs` 已有 prepare_task / enter / 用户范围 copy 的声明与 Unsupported 占位，尚无执行机制。`load.rs` 的创建分派装载前返回 `SandboxUnsupported`（ABI `-ENOTSUP`），不创建实例、不回退 native；销毁入口仍未实现。没有 sandbox.kcomp。普通用户 task 已有单独的 kcore_user_* C ABI / U-mode 路径，不等于组件部署后端已实现。

缺口：组件按域装载 / import、Core mechanism ecall SDK 与 destroy；私有 allocator / runtime 选择可复用 Isolated 后端，但未接通 Sandbox 执行路径；普通程序的低特权执行 / trap / U 页表访问已有独立路径。

下一步：复用已接通的普通用户 task / AS / trap / copy 机制，补组件按域装载与 Core ecall SDK。阶段依赖与验收见 `docs/development/userspace.md`。

#### 3.18 驱动组件 `▰▰▰▱▱` EXPERIMENTAL

现状：`drivers/virtio_blk` 是 VirtIO-MMIO 块驱动，发布 `block.device` 与 `probe.result`，内部用第三方 crate `virtio-drivers 0.13.0`，Hal 适配器是组件私有（`hal.rs`），不 fork 上游；`driver_prober` 做协议无关总线角色；测试 fixture 有 `ram_blk`/`ram_blk_rw`。QEMU RV64/RV32 CoreTest 覆盖 `driver-candidates`、`driver-prober-load`、`driver-prober-dispatch`、`driver-attach`、`driver-no-match`、`driver-multi-device`，含 `no-block` 场景。

缺口：只有 virtio，还是 QEMU virt 的 MMIO 变体；没有 PCIe/USB/NVMe/网卡；没有 UART driver component（`driver-model.md` 明说未实现）；没有真机；没有热插拔或驱动更换事务。

网络骨架：`os/components/network/netstack/` 是独立 Rust `.kcomp`，以 `third_party/smoltcp` submodule（v0.14.0，0BSD）作为私有 no_std / no-alloc 后端。已按 TCP 客户端 / 服务端 / UDP 查询用例声明 `abi/network.toml`、SDK NetworkBinding / TcpSocket / UdpSocket 和 NetworkProvider / NetworkInstance 分发；内部 connection / listener 分开，bind → listen 保留同一 ID / 端口，存储由服务分配。服务与 worker 经 Engine 共享状态，worker 独占协议推进，网卡调用在锁外；Busy 与网络 Pending 分开。业务 / 同步 / C-Gate adapters 仍待手写；SDK bind 和组件 create / destroy 返回 `-ENOTSUP`，无服务 endpoint。NetDevice / 网卡、跨组件 unpark 与 timer 尚未接线；契约见 `docs/interfaces/network.md`，代码见 `docs/modules/netstack.md`。

下一步：定稿 `NetDevice` 契约与网卡 provider，手写 netstack adapter / 原语；事件驱动先补跨组件 unpark 与显式 timer 登记 / 取消，普通 park 不新增隐式 deadline。USB 走 TinyUSB，前提是同步原语。TLS 走 Mbed TLS，前提是 RNG/Clock/Socket。真机 bring-up 时补 SD/eMMC 与以太网，顺序见第 7 节。

#### 3.19 文件系统服务 `▰▰▰▰▱` IMPLEMENTED（只读；通信收敛未完成）

FatFs、littlefs 是两个独立 C `.kcomp`，显式消费 Block。littlefs mount 失败才 format，
随后自检；legacy open/read/close 与双实例介质隔离回归保留，节点接口仍 ENOTSUP。
FatFs 已有 root/lookup/node_info/node_details/open_node/read_at；64 个挂载期 borrowed
Node、8 个独立 FIL open；默认 init 使用 IPC-only FatFs，legacy 测试仍保留表/Gate。
Provider 状态已有 try-enter 串行纪律，不能继续称 files 表无同步。

VFS create、Rust SDK、LocalFs/RemoteFs、统一 Namespace/Path/OpenFile 与 owned Server
Task 已接线；ksh cat 和 ELF 文件读取都走 VFS。CoreTest 真两个 C FatFs+RAM Block
验证身份、独立游标/EOF、wrong Task、取消/退出回收、失效/重启不重绑与 drain/stop。
init 真实 virtio FAT/双盘/Local/Remote 路径及 RV64 exec 本轮通过。
模块事实见 [VFS](docs/modules/vfs.md)，完整当前门禁与剩余迁移见 §3.29。

缺口：littlefs Remote Node、目录枚举/写/share/delete/ACL、两级缓存与通用 POSIX fd；
C/Rust FS legacy frontend 仍两 Backend，Block 三 Backend，method codec 尚未生成。
下一步先恢复基线失败与 KABI/SDK 收敛，不重复实现已接通 VFS。lwext4 仍候选，许可另审。

### SDK、测试与观测

#### 3.20 SDK / C ABI / Rust ABI `▰▰▰▰▱` IMPLEMENTED

现状：`abi/*.toml` 是单一来源，生成 C 头与 Rust 镜像；72 个 `kcore_*` 导出（含九项 IPC 与 current principal 查询）；组件 ABI 是窄 C ABI，Rust ABI 永远是私有实现；SDK 私有携带 C runtime。可选 `kcomp_runtime_init` 在业务 create 前选择 K 共享堆 / I 私有堆，Rust `Vec/Box` 与 C `malloc/free` 共用部署 adapter。`make abi-check` 重生成后逐文件比对，`kcomp_abi_drift.rs` 冻结入口面与绝对数值；QEMU CoreTest `c-frontend` 加机器级 `load kcomp_c_smoke`，ArchTest `isolated-heap` 验证同工件的分配后端。

缺口：没有 ABI 版本兼容，靠 exact fingerprint 加协调替换；KABI 尚不生成业务 method codec/client/dispatch，C/Rust 仍需人工同步。组件外链只允许 `kcore_*`；SDK 不朝 libc 或共享 runtime 扩张。

下一步：为 embedded 系列提供 `kcomp-embedded-*` 伴生 crate 或 SDK feature；把 `porting.md` §8 的 host 接口（Net/RNG/Clock/Log/Thread/Sync）逐个成文并落到 SDK。

#### 3.21 测试体系 `▰▰▰▰▱` IMPLEMENTED

现状：host 单测含 proptest；CoreTest 是板内集成，本轮 RV64 双 CPU 为 79 项 KTAP 检查、RV32 为 57 项，两个 block topology 均运行；ArchTest 43 个 case，每 case 独立 QEMU，其中 29 个 `isolated-*`。入口是 `make check/test/test-host/test-qemu/test-arch`；另有 opt-in 的 `test-arch-smp-rv64` 与 `test-arch-{x86_64,aarch64,loongarch64}` / `test-arch-new`（SMP 已实现并纳入 ArchTest CI；新 ISA 仍独立 opt-in）；CI 三个 job（check/qemu/archtest）。

兼容性应用：`tests/compat/` 从 pinned `third_party/libc-test` 直接选择上游 source，
13 项 ISO C 加 1 项纯计算在 Linux / Windows 共用，另有 2 项 Linux POSIX 文件测试。
宿主 runner 构建普通 ELF / PE、原生参考运行、生成组合 manifest 与带许可证的 tar 包；
`make compat-*` / `test-compat-*` 不消费或创建内核 `.config`。独立的
`.github/workflows/compat.yml` 配置 Linux / Windows 原生参考与双平台打包 job。
工具与具体用例见 `docs/development/compat-testing.md`。

验证（2026-10-04）：Linux x86_64、系统 GCC 8.4 / 静态 glibc，16/16 PASS；
MinGW-w64 GCC 9.3 交叉构建 14/14 Windows x86_64 PE，另外两项显式 UNSUPPORTED。
全部 Linux 程序无 PT_INTERP；Windows PE imports 为 KERNEL32.dll / msvcrt.dll，
不能宣称无 DLL 依赖。双平台包包含 30 个程序、可执行位、manifest、COPYRIGHT / AUTHORS。
host runner 11 项 failure / crash / timeout / stale result / 平台拒绝 / config-free 检查通过；
Kconfig 胶水原 12 项通过。报告与日志在 `build/compat/`，不提交生成物。

RV64 普通用户执行（2026-10-04）：14 个普通 ELF 夹具位于
`os/components/tests/exec_probe/`。CoreTest exec 分组验证真实 U-mode、退出码、argv /
auxv / BSS、坏指针、Core / text / stack 权限、mprotect、fork 深拷贝与 FP 独立性、
exec 成功和失败回滚、wait 的 EFAULT / ECHILD，以及无 ecall 长循环的 timer 返回。
系统集成编排留在 CoreTest；没有另建 exec_test 组件。

默认 init 的 RV64 FAT / 无盘 / 坏盘三场景通过；FAT 串口流程使用实际文件
open/read/close → ksh exec → POSIX → U-mode → exit/signal，故障后 cat / echo /
shutdown 仍正常。RV32 普通用户执行返回 ENOTSUP，既有 CoreTest / init 流程回归保留。

原 16 项上游用例另已交叉构建为 RV64 静态 ELF，QEMU Linux 用户态参考均退出 0；
不是 KaleidOS PASS。实际从 FAT 装载 glibc `compiler/udiv` 后启动终止，观察到
uname / openat / writev / mmap / signal 等缺口，没有伪造成功。
当前无原生 Windows 环境，Windows reference 未验证；新增 CI 尚未远端运行。
操作与边界见 `docs/development/userspace.md`。

缺口：NoMMU 尚未进入默认门禁 / CI，本轮 S-mode 有私有 profile 实跑证据；M-mode 启动失败；runner 校验完整 KTAP 计划、连续编号、失败与 Skip；host ring 是线程本地替身，不覆盖并发语义。

下一步：NoMMU 进 `_test-build` 与 CI；Isolated 补压力与失败注入；并发用 Loom，形式化用 Kani/Miri/Verus，另有 Test Scheduler / Hunt Mode，这些是登记的方向，不是排期。

#### 3.22 观测 / monitor / trace `▰▰▰▰▱` IMPLEMENTED

现状：结构化 trace ring（seq 单调、固定容量、无分配），只读 Inspector，`core>` monitor（行编辑、历史、Tab，help/machine/memory/tasks/load/unload/components/catalog/trace/shutdown/reboot）。QEMU runner 依赖 `core>` 与 load/unload；CoreTest 用 trace 断言操作到事件；ArchTest 断言精确 scause。

缺口：`TaskBlock`/`TaskWake`/`Fault` 事件刻意未定义；host ring 不覆盖生产锁；没有动态订阅或落盘。

下一步：block/wake 落地时加对应事件；Fault 事件等 Core 接管 fault 记录再加。

### 平台与生态

#### 3.23 平台 profile 与真机 `▰▱▱▱▱` PLANNED（按最弱一环取）

现状：仓库零真机支持。RV32 NoMMU 有 backend / defconfig，本轮 S-mode 私有 profile 已构建并在 QEMU 通过 CoreTest；默认入口与 CI 仍未覆盖，不等于 M-mode 或 MCU 验证。M-mode 代码可编译；本轮私有 profile 编译并尝试 QEMU，默认 OpenSBI 在 S-mode 交接、裸 M-mode console 仍走 SBI，未启动成功；不计为通过。

缺口：没有板卡、没有板级 quirk、没有非 FDT 发现路径。

下一步：VisionFive 2 串口出 `core>` 作为第一步验收；RV32 NoMMU 先做成 QEMU 里的第二个启动目标，再谈 MCU 真机。顺序见第 7 节。

#### 3.24 第二 ISA（AArch64 / x86_64 / LoongArch） `▰▱▱▱▱` PLANNED（骨架已立，未实现）

现状：`os/arch/src/{x86_64,aarch64,loongarch64}` 与 `os/boot/{x86_64,aarch64,loongarch64}` 已有**同形骨架**（`encoding` / `elf` / `console` / `cpu` / `smp` / `trap` / `context` / `mmu`，实现体全部 `todo!()`），能编译、未启动、未验证；纯编码模块可在 host `test` 下编译（当前以 `#[ignore]` 保留）。Kconfig 有 `ARCH_X86_64` / `ARCH_AARCH64` / `ARCH_LOONGARCH64` 与对应 defconfig；`ComponentRelocationImpl` 按 ISA 选择（`ELF_MACHINE` 62 / 183 / 258，未实现时 `Unsupported`）；`AddressSpaceImpl` 用显式占位 `stub_vm::StubAddressSpace`（`PRIVATE_ADDRESS_SPACE = false`）。新 ISA ArchTest 入口为 opt-in（`make test-arch-{x86_64,aarch64,loongarch64}`）。

缺口：没有真正启动过的第二 ISA，"arch 是抽象"仍未被证明；`.kcomp` 组件目前仍是 RISC-V 重定位专用，新 ISA 只能用 Core-only 镜像；boot 的 `os/boot/<isa>` 与 `os/arch/src/<isa>` 的硬件机制（trap / context / mmu / AP 启动）全部待手写。

下一步：按 x86_64 → aarch64 → loongarch64 顺序做真实 bring-up（loongarch 参考 DragonOS 与 Linux `arch/loongarch`）。

#### 3.25 embedded 生态与调包 `▰▱▱▱▱` PLANNED

现状：`embedded-hal` / `embedded-io` / `embedded-storage` / `embedded-nal` / `embedded-graphics` 在仓库里完全不存在，SDK 依赖为空。调包先例只有 FatFs 与 littlefs，都是组件内 adapter。

缺口：没有伴生 crate，没有统一 host 接口，没有网络与 USB。

下一步：整节计划见第 8 节。

#### 3.26 POSIX personality `▰▰▰▱▱` EXPERIMENTAL

现状：静态 RV64 ELF / 启动栈、进程族、独立 task / AS、fork / execve / wait4、console
write / EOF read / close、brk / mprotect、退出 / fault 状态已经接入真实运行。
PID / Linux syscall 语义在组件；Core 只管理实际执行真相。create 配置原地替换成显式
镜像快照 profile，最多 8 个命名镜像；只读 `posix.process` endpoint 提供退出状态。
一个 POSIX instance 管进程族，不把 PID 登记为 ComponentId。详见 `docs/modules/posix.md`。

缺口：一般 VFS / 文件 fd、mmap、signal delivery / handler、线程、vfork、pipe / terminal、
动态链接、静态 PIE；上游 glibc startup 尚未通过。fork 深拷贝，无 COW；wait 与成功 exec
旧 backing 尚无物理回收。SandboxedNative 组件部署仍未实现。

下一步：先接只读 VFS / fd，验证普通应用运行期文件 I/O；libc startup 按实际二进制缺口独立推进，不能替代纵向文件链路验收。
现有 FAT/littlefs/block provider 可复用；Core 不收 POSIX 或文件系统语义。

#### 3.27 ksh `▰▰▰▰▱` IMPLEMENTED（KernelNative 交互 shell）

当前cat和ELF文件读取走VFS；以下早期交互阶段计数/fingerprint/PASS保留为历史记录，最新门禁见§3.29。

现状：独立 `ksh.kcomp`，普通 profile 由 init 自动启动；monitor 也可 `load scheduler_rr` → `load ksh` 启动普通任务。
支持 help [command] / history / echo / clear / components / endpoints / devices / load native 或 isolated /
inspect 已加载实例 / cat / exec 静态 ELF / exit。业务代码只走 SDK。shell 的 Core 观察导出包括 console read 与
component / endpoint 的值枚举；设备发现复用 `device_nth`，`device_info` 复制已有描述与 owner / quarantine，装载在既有 `component_load`
上增加 domain 请求，协调替换 exact ABI fingerprint。命令与文件组合边界见 `docs/modules/ksh.md`。

本次交互改进：512 字节有界行、引号 / 转义 / 空参数 / 注释、历史与草稿恢复、光标插入 / 删除、
编辑控制键、命令名补全与单命令帮助。基础解析和历史不分配堆；会话 / 解码缓冲驻留各实例
自己的可写 image，参数使用有界短偏移，避免在任务栈上存放大数组（当前栈预算 16 KiB）。
未支持的管道 / 重定向 / 命令列表、超长或非 ASCII 输入整行拒绝，不执行截断前缀。

交互改进阶段验证：ksh 14 个 host 用例 PASS；host tests 与 RV64/RV32 的 ksh Clippy `-D warnings`、
`make fmt-check` PASS；`make test-qemu` 的 13 条流程 PASS：四条 CoreTest→ksh 串口链
（当前 RV64 81 项、RV32 59 项 CoreTest）以及 RV64 五条、RV32 四条 init 流程。
串口覆盖引号 echo、编辑 / 补全 / 历史、取消、错参与语法拒绝、超长恢复；init 覆盖真实
FAT 路径的引号 / 转义读取和 RV64 引号 exec 参数。RV64 OOM 后引号 echo、历史查询 / 回忆、
退出继续响应。该阶段没有变更 Core / SDK ABI，也没有重跑 ArchTest。

设备显示阶段：显示主 compatible、MMIO / PIO 窗口、首个 IRQ、资源总数与认领者
`artifact#id`；空 virtio transport 仍按固件记录显示，不探测协议、不认领设备。
新增只读 `kcore_device_info`，exact ABI 原地协调为 `0xD58F_B296_4E73_A10C`，全部组件重建。
验证：`make check` PASS（Core host 564 PASS、6 ignored；SDK 88 PASS；ksh 14 PASS；
fmt / Clippy / Kconfig 12 项 / 16 个 ABI 生成物一致性 / RV64 构建 / RV32 check）。
`make test-qemu` 13 条流程全部 PASS：RV64 CoreTest 84 项、RV32 62 项，四条 CoreTest→ksh
链及九条普通 init 流程覆盖真实设备表、块驱动 owner、双盘与 RV64 OOM 后继续查询。
CoreTest 新增认领后值查询、短缓冲 / 不存在设备、释放后状态三个检查；host 另覆盖
quarantine、未映射 IRQ、多资源、零 ID / 零 IRQ 与失败时不写输出。
未重跑 ArchTest。串口证据在 `build/tests/{coretest,init}-rv{64,32}/logs/`。

此前验证：ksh 8 个 host 用例、SDK 82 个、Core 536 个通过（Core 6 个既有 ignored）；
`make check` 包括 fmt / Clippy / Kconfig / ABI 生成一致性 / 全部 host / RV64 构建 / RV32 check。
RV64 与 RV32、default 与 no-block 四条 QEMU 串口链均 PASS：CoreTest → ksh 输入与查询 →
native / isolated 装载 → 故障或 panic 加载返回 EIO 且 shell 存活 → cat → 超长行恢复 →
exit → monitor unload → shutdown。回归修复：Isolated create / destroy 设置被调实例边界并
挂起 caller Core ABI depth；monitor 在串口读取前恢复 Runnable 任务，避免 yield 后抢读。
RV64 / RV32 各六项 ArchTest PASS：isolated-lifecycle / lifecycle-fault / destroy-fault /
service-fault / direct-imports / panic-escape；覆盖此次边界复用的正常与失败出口。
两种架构另验证不经过 CoreTest 的独立启动：先加载 ksh 而无策略时 monitor 仍可交互，
随后加载 scheduler_rr 启动 shell；空闲输入、无 FS 的 cat 拒绝、exit / unload / shutdown 均 PASS。

缺口：inspect 仅加载后元数据，无未加载 artifact / 任意文件格式检查；FS 无目录 / 工作目录
契约，因此 ls / cd / pwd unsupported；多 provider 的 cat 无 namespace 选择；尚无 console
session 仲裁、通用 VFS / execve 路径、文件 fd、管道或 Win32。`exec` 已有显式静态 ELF
入口；`./hello` 的隐式查找仍未实现。

#### 3.28 init `▰▰▰▰▱` IMPLEMENTED（最小启动编排）

现状：普通KernelNative init选择RR并运行driver_prober，明确选择IPC-only Block，
创建FatFs（LE config、control、flags=1）和VFS，显式安装Block→Fat→VFS→ksh grants。
真实Task检查根挂载，ksh消费VFS Endpoint及选定根路径。无块盘仍有Local namespace；
坏FAT使init Failed回monitor。没有全图回滚/级联卸载；实现见docs/modules/init.md。

本轮make test-init覆盖RV64五/RV32四场景并通过，含cat/Local/Remote绝对路径、
双盘选择、坏盘/无盘与RV64 OOM/ELF exec。早期FS直连验证不作为当前链路证据。

缺口：无常驻 supervisor / watchdog、依赖解析或热插拔；已有只读混合 VFS namespace 与请求驱动对象 reaper（§3.29）。
当前 prober / mount 是有限任务；`sched_run` 不提供 join，异步启动需要组件侧完成契约。

#### 3.29 Endpoint Request/Reply 与混合 VFS `▰▰▰▱▱` EXPERIMENTAL

2026-10-09 Cleanup 核对：起始 HEAD 与 fetch 后 origin/develop 均为
`2e10304c389fb6ab1b5815a97575199b62aa0c4b`，起始工作树干净。
现行传输见 [IPC](docs/architecture/ipc.md)；事实/维护点/性能/门禁见
[专项审计](docs/development/component-communication-audit.md)，待实施小 patch 见
[KABI 设计](docs/development/component-communication-cleanup-design.md) 与
[文件级迁移](docs/development/component-communication-migration.md)。

已实现：有界 Exchange、verified Component/Task、grant、独立 Echo Server、真实 wait/
wake/退出/取消；Local+Remote FatFs、VFS runtime/SDK、ksh cat/ELF；virtio Block IPC-only。
实际链路是 VFS → Fat Server → Block Server，设备由 driver 自己的 Task 身份操作。
Node 是 borrowed mount-lifetime 身份，open 才 owning；不是早期每-node lease 草案。

最新实施门禁：make abi-gen/abi-check（23生成文件）、make check（含test-host）通过；
make test-qemu：RV64 default/no-block各112 checks、RV32各90 checks与shell通过；
init RV64五场景/RV32四场景全部通过。make test-arch：RV64/RV32各43/43、SMP3/3通过。
4项C/Rust envelope测试及5项generated方法测试通过，无expectedFailure。
私有 RV32 S-mode NoMMU：default/no-block 各90 checks与shell通过；I域装载明确ENOTSUP。
生产修补包括policy拒绝优先级与decoder容量校验；测试修补把driver I/O放真实Task，
独立legacy fixture adapter解除未支持的IPC import依赖；没有扩展I白名单。

收敛状态：Phase A专项审计完成；B已实现Echo/Block/Filesystem method AST、整数LE、bounded buffer、
C/Rust client/validator/dispatch，接入VirtIO/Fat Server与RemoteFs；命名结构、VFS自身待做。
详见 [方法生成](docs/development/kabi-methods.md)。C仍有Block三Backend、FS双入口、
RAM/little/probe旧通道；D私有Task/import/copy尚缺；E普通业务Direct/Gate未删。
F文档/测试随实施更新，不等于整体Cleanup完成。Core新增账本/锁为零。

限制：KernelNative同特权可信；Isolated新IPC/持久Task/跨AS copy、Sandbox、deadline/
强制终止/通知与物理回收未实现。旧Gate必须保留到真实I替代门禁满足。
Echo fixture确定性验证远端Task先于create完成的启动窗口，等待Endpoint提交后listen。
下一步：VFS固定结构及client/dispatch，逐组迁移普通服务，补私有域IPC后删除旧通道。

## 4. 结构热点（按对 Core 冻结的威胁排序）

1. block/wake 已接通固定 CPU 的协作式 SMP；组件持有条件与等待者集合，Core 只提供 permit / park / unpark。继续检验真实驱动与服务负载。
2. task 与地址空间没有绑定。`TaskRecord` 和 `AddressSpaceId` 之间没有关系，`Running(CpuId)` 没有 AS 概念。将来一个 POSIX 进程等于一个 AS 加 N 个 task，这个语义必须由 Core 先提供，否则进程语义会漏进 Core。
3. 抢占没接线。`timer::on_trap` 不调用 `sched::on_timer_tick`，后者是 `todo!()`。纯协作模型下，不协作或死循环的执行域拿不回控制权。
4. 组件多实例与生命周期的完整矩阵。多实例在加载、状态、设备、FS 维度已经证明，但 endpoint、task、crosstalk、卸载后的语义没有逐个证明。unload 是 tombstone，backing 驻留到 reboot，没有 drain。发布 Direct 的 Native provider 不能进入 Stopped/destroy；Failure 后旧表仍可能调用，必须保留 ctx，代码页驻留本身不能保证其安全。Gate 的 stop/admission 竞争已由 host 与 RV64 双 CPU CoreTest 验证，完整 DMA/drain 回收仍未证明。
5. IsolatedNative 的缺口。机制真实，覆盖很窄，详见 3.16。
6. 真机验证缺失。设备、中断、DMA、timer 都只在 QEMU virt 证明过。
7. 组件模型仍在演进。`ExecutionDomain` 与生命周期接口是 ACTIVE DESIGN，SandboxedNative 参与的所有组合都被 `todo!()` 或显式拒绝。

## 5. 验证矩阵（哪些组合真的跑过）

| 组合 | 跑过没有 | 入口 / 证据 |
|---|---|---|
| Host 单测（Core truth / parser / property） | 是 | `make test-host`（`cargo test --workspace` 加 SDK/prober/kbench/ram_blk）；`kcomp_abi_drift.rs` |
| CoreTest / QEMU RV64 / MMU / `default`+`no-block` | 是 | `make test-qemu` → `tests/qemu/runner.py` |
| CoreTest / QEMU RV32 / MMU / `default`+`no-block` | 是 | 同上（`qemu_rv32_defconfig`） |
| ArchTest / QEMU RV64 / MMU（43 case） | 是 | `make test-arch` → `tests/qemu/arch_runner.py` |
| ArchTest / QEMU RV32 / MMU（43 case） | 是 | 同上 |
| ArchTest / 新 ISA 骨架（opt-in） | 否 | `make test-arch-{x86_64,aarch64,loongarch64}` / `test-arch-new`：镜像可构建（Core-only），boot 为 `todo!()`，用例现在会失败 |
| ArchTest / SMP | 是 | `make test-arch-smp-rv64`：启动 / IPI / per-CPU；组件调度、远端 wake、双向 panic 在 CoreTest 的 `make test-qemu` 中验证 |
| KernelNative（执行域） | 是 | CoreTest 全链加 ArchTest `panic-*` |
| IsolatedNative（S 加私有 AS） | 是，仅 QEMU | ArchTest `isolated-*`（29 case，RV64+RV32） |
| SandboxedNative（U-mode） | 否 | Core 骨架；create `-ENOTSUP`，U-mode / destroy 未实现，调用组合显式拒绝 |
| RV32 / NoMMU | S-mode 私有 profile 实跑 | 本轮 QEMU CoreTest 通过；默认 `_test-build` / CI 未覆盖。M-mode 编译成功、启动失败，证据见 [审计](docs/development/core-convergence.md) |
| RV32 / M-mode（`PRIVILEGE_MACHINE`） | 编译通过，启动失败 | 本轮 NoMMU 私有 profile 通过 Cargo 构建；默认 BIOS 与裸启动分别尝试，均未到 monitor，见收敛审计 |
| 真机（任何板卡） | 否 | 仓库没有任何真机代码或配置；`os/`、`configs/` 无 VisionFive/JH7110/ESP32 之类 |
| CI | 是 | `.github/workflows/ci.yml` 三个 job：`check` / `qemu` / `archtest`；只覆盖 RV64+RV32 MMU。SMP 已进入 archtest job，新 ISA 的 opt-in 目标仍不在 CI |

提醒：`make test` = `test-host + test-qemu + test-arch`；`make check` = fmt + clippy + `_test-kconfig` + `abi-check` + `test-host` + 交叉构建。两者都不含 NoMMU 启动，不含真机。

此前复核抽样：在 `0d189e1` 上复跑 `make check`、`make test-qemu`、`make test-arch`，均通过；`tests/qemu/logs/` 留有 2026-09-28 的 RV64/RV32 `default`+`no-block` 与 ArchTest（RV64/RV32 各 41/41）日志。`python3 tests/kconfig/test_glue.py` 复跑为 11/11 PASS。

2026-10-01 SMP 工作树验证：`make check` 通过（Core host 531 PASS、6 ignored；scheduler_rr 6 PASS；SDK 82 PASS；Kconfig 11/11；13 个 ABI 生成物一致；RV64 构建 / RV32 检查）。`make test-qemu` 的 default / no-block 均通过：RV64 CoreTest 56/56、RV32 49/49；RV64 两场景另各复跑一次通过。`make test-arch` 的 RV64 / RV32 各 41/41；`make test-arch-smp-rv64` 的基线与三个 SMP 硬件契约用例全通过。串口证据在 `tests/qemu/logs/*20261001-20*.log`。测试编排在 CoreTest，独立 `kcomp_smp` 只充当 panic 被测对象。

## 6. 近期依赖链

原路线图文档的依赖链并入本节（文件已删除）。已完成的地基：

```text
P0 地基（启动地址去硬编码、Sv39/Sv32 启动）                 —— 已完成
P1 任务系统（context switch、调度执行链）                    —— 已完成
P2 中断/驱动（timer 抢占 C5、设备·IRQ·DMA C6、第一个 driver）—— 部分：driver 已落地，抢占未接线
P3 组件化进阶（区域分配 C7、域视图交付 C8、任务化组件 C9）    —— 部分：身份与生命周期已落地，普通 owned Task 可实现 Passive / Active / Hybrid；缺私有域任务与回收协议
P4 执行域/隔离（C10）                                        —— 部分：受限 IsolatedNative 已落地，ASID / U-mode / ecall / Isolated 任务与设备 / SandboxedNative 未完成
```

### 6.1 主线：由真实文件负载验证组件组合

职责见 [服务执行](docs/architecture/service-execution.md)，源码问题与实验条件见
[服务研究](docs/development/service-runtime-study.md)。本次经用户明确授权修改生产实现，
按阶段交付；create 的窄 ABI 协调增加 domain，未引入注册中心或 Runtime Graph 框架。

| 顺序 | 交付 | 前置与验收 | 当前状态 |
|---|---|---|---|
| 0：架构基线 | Service / Binding / Transport / Execution / Session / Recovery 分开；问题矩阵带证据等级 | 现行契约、代码事实和提案分别登记 | 文档已整理；未新增硬件验证 |
| 1：双设备与显式组合 | raw block 可 attach，prober 遍历全部设备并关联实例；组合方显式选择两条 Block→FS 连接；init 配置根序号和 shell FS endpoint | 真实两盘/两驱动有效读写；错指纹、过期选择、创建失败；不以改 Core ABI 为起点 | DONE：raw attach、全枚举/去重、显式选择；真实两盘双 FS 直连通过，见下文 |
| 2：只读 VFS | 补 provider 节点/lookup/read_at 所需语义、同步纪律与 SDK 数据缓冲前端；/fat、/little 并存 | FS ABI 演进与 adapter 前置；两个 FS 的路径、独立 open 与共享引用分别验证；ksh 消费选定 VFS | Local+Remote Fat、VFS服务、ksh/ELF已接；littlefs节点/read_at/Remote仍待迁移，当前失败与证据见§3.29 |
| 3：POSIX 文件 I/O | fd 引用 VFS 打开对象，用户内存 copy 与 openat/read/close 接通 | 普通 ELF 运行期访问两个挂载；坏指针、短读、fork/close 引用语义 | BLOCKED：阶段 2，通用 fd 表未接 |
| 4：动态逻辑故障 | 依赖失效、旧对象错误、新实例显式重新挂载 | 一项 FS 失败不误伤另一项；旧 handle 不改指向；不要求现有 Direct FS 热卸载 | 部分已验证：混合VFS测试覆盖Fat失效、旧句柄、新实例不重绑及另一挂载可用；完整回收仍缺 |
| 5：Queued 实验 | Runtime 队列/Worker 原型；有需求再设计最小授权通知 | 先证提交/取消，最终阻塞完成无忙轮询；完成早于等待与失败收尾；不放宽 unpark owner | PLANNED：跨 owner notification 未实现 |
| 6：部署/保护评估 | 固定业务负载比较实际可行 K/I 组合 | 先验证 import/等待/同步能力，再测性能；硬件保护另由 ArchTest/QEMU 证明 | PLANNED：真实驱动/FS 支持面受限 |

### 6.2 历史 FS 前置实施与验收（2026-10-09，混合 VFS 接线前）

以下保留旧阶段记录；其“没有 VFS”和 PASS 是当时事实，当前实现/失败以 §3.29 为准。

本次明确授权下完成阶段 1，以及阶段 2 的同步/SDK 前置；没有实现 VFS 草案、POSIX
通用文件 fd、Queued 通知或 Native Direct release。初审问题编号见服务研究矩阵。

- #1/#2：virtio_blk 只检查 sector 0 传输，不解释格式；prober 完成全部候选，同驱动
  设备去重，已 Match 的设备不交后续候选。CoreTest 自动发现两份独立驱动、验证不同
  sector 0、无签名盘、重复 claim 拒绝及拒绝后原服务仍可用。
- #3/#4（绑定部分）：init 按配置根盘序号选择（默认 profile 为第 0 个），把确切 FS
  endpoint 交给 ksh；业务不再随全局 FS 数量重选。无配置 monitor shell 仍拒绝歧义。
- #5：FatFs/littlefs 的 mount/open/read/close/unmount/destroy 共用实例级 try-lock；
  竞争返回 EBUSY。文件 token 单调增长、耗尽拒绝，关闭后的旧 token 永不指向新 open；
  零长度 read 也验证 token。
- #8/#11：create 同时消费 domain/config，指纹协调替换为
  `0x71A9_CE34_8D62_F0B5`，全部组件重建；Rust/C read 都接受普通数据缓冲区。
  Direct 直接写入；Gate 使用 8+512 字节栈 frame，单次最多 512 字节，校验后复制。
- 新发现修复：RV32 的 u64 LBA 收窄返回 EOVERFLOW，不能读写低位扇区；packer 的
  readelf 管道完整消费输出，避免 pipefail/grep -q 的 SIGPIPE 误报和漏拒绝。

验收结果：

| 层次 / 命令 | 本次结果与边界 |
|---|---|
| `make check` | PASS：fmt/clippy、ABI 工件一致性、host 单测与构建；Core 561 项、SDK 86 项、prober 8 项、init 2 项等；RV64 构建/RV32 check |
| C provider host | 生产 adapter + 上游 FatFs/littlefs + fake 块介质；强制读 I/O 与 close/mount 交错、旧 token/零长/耗尽通过；不作为硬件隔离证据 |
| C SDK / packer host | Direct/Gate 普通小缓冲/零长/512 上限/非法回复；大量 readelf 输出下接受合法 ELF、拒绝 ALIGN，通过 |
| `make test-qemu` | PASS：CoreTest RV64 每拓扑 81 项、RV32 每拓扑 59 项（default/no-block）；storage-real-chain 在 default 真实两盘上分别运行 FatFs/littlefs，验证内容/错误路径/旧 token；no-block 检查缺失存储 |
| init 串口流程 | RV64/RV32 的 FAT、dual-fat（FAT 根 + raw 盘）、no-block、bad-fat 均 PASS；RV64 OOM 也 PASS。内存压力下 destroy 可成功或返回 ENOMEM，不把特定剩余块布局当作契约 |
| `make test-arch` | PASS：RV64/RV32 基础硬件套件与 RV64 三项 SMP；不扩大既有 Isolated 支持面或回收保证 |

原始串口日志在 `build/tests/{qemu,init,archtest}-rv{64,32}/logs/`，SMP 日志在
`build/tests/archtest-smp-rv64/logs/`；结果来自本次运行。下一阶段依赖仍按 §6.1 推进。

阶段 1 的 FS 直连可先独立验收，不以 VFS 完成为前置；阶段 2 中同步纪律先于并发负载。
阶段 4 的「故障」是受控逻辑失败，不宣称 KernelNative 可容纳任意内存破坏。
每次新增 Core 机制先提供现有公开 API 无法正确完成的真实反例，优先 SDK/Runtime 方案。

### 6.2 独立底座工作与已完成机制

- RV64 普通用户 Task↔AS 与 trap 路径已接线，不再列为未开始的 POSIX 前置；私有域组件
  Worker/Sandbox 是另一项未完成能力。CPU/调度事实见 §3.3–§3.5、§3.26。
- park/unpark permit 与远端 CPU 唤醒已有；事件 trace、跨 owner 通知仍后置，等待者/条件
  留在组件。已修资源 grant/IRQ/Init-Exit 门禁见 [归属审计 §12](docs/development/execution-ownership-review.md#12-授权后的修复与验证2026-10-08)。
- Timer 抢占另行接线并证明 IRQ 状态保存，不作为同步双盘/VFS 主线的硬前置。
- drain、Direct 引用释放、私有域 Task 与完整回收按真实消费者分别论证，不为双盘实验
  提前承诺热卸载。Isolated 继续受限教学实验，不靠补 ASID 推导不可信代码隔离。
- `validate`/`bind` exact ABI 与 liveness、SDK 消费路径已有；lookup 保持 contract-only。
- RV64 协作式 SMP 已接真实组件 Task；迁移/work stealing/抢占与第二 ISA 后置。

三条贯穿约束（合并自原路线图 §3，仍在生效）：

- 物理帧分配是 Core 内部机制，不是策略流；未来 `MemoryPolicy` 只能提议偏好（NUMA、配额），选择、验证、提交留在 Core。
- 执行域定位（D2=A）：`KernelNative`（S 加共享 AS）是常态、长期模式，就是可信代码，撤销是协作式的；`IsolatedNative`（S 加私有 AS）只做条件性故障隔离；`SandboxedNative`（U 加私有 AS）才是未来硬件强制的边界。细节见 `driver-model.md`。
- `Registry::unload()` 绝不能直接变成物理释放原语：缺少"停止新工作 → 排空 IRQ/回调 → 确认零活跃执行"的 drain 协调协议时不得释放内存（phase 1 不承诺物理回收），见 3.14。

"第一个 Driver Component" 在代码上已经落地（`virtio_blk`、`driver_prober`、`block.device`，CoreTest 已验证），不是当前的技术瓶颈；下一批驱动的前置是 `NetDevice` 等新契约，见 3.18 与第 8 节。

## 7. 硬件路线图

现状：仓库零真机支持。下面全部是计划，不是当前能力。

第一台真机候选是 VisionFive 2（StarFive JH7110，RV64，Linux-class）。顺序是 OpenSBI/U-Boot → KaleidOS entry → DTB → `MachineInfo` → UART console → `core>` 出现在串口。第一步验收就是串口出 `core>`。之后依次做 timer、interrupt controller、GPIO，再做 SD/eMMC，然后 Ethernet，最后 PCIe/USB。GPU、VPU、camera、multimedia 不要阻塞第一次 bring-up。

第二条线是 MCU：RV32 + NoMMU + 小内存，候选 ESP32-C3。不承诺支持，只作为验证目标。它检验 NoMMU 内存模型、GPIO、SPI、I2C、flash 和 embedded 生态。在真机之前，先把 RV32 NoMMU 做成 QEMU 的第二个启动目标。

两条线合起来才算验证了"同一套 Core 适配不同机器类别"。VisionFive 2 代表 Linux-class RV64，ESP32-C3 型代表 MCU-class RV32 NoMMU。

依赖序补记（并入自原路线图）：设备链（`DeviceTable` / claim / IRQ / DMA）→ 第一个 Driver Component 已完成；下一段是 QEMU RV32 NoMMU 的第二个启动目标 → QEMU M-mode（`PRIVILEGE_MACHINE`，目前只可编译，无 defconfig 与 boot harness）→ 真实 MCU。这三段不互相阻塞，但都排在真机 bring-up 之后才有意义。

多架构（AArch64/x86_64）重要但不是近端阻塞。等 Core 词汇、组件模型、设备模型、task-mm 缝稳定后，用一个真正不同的 ISA 验证 arch 是抽象，而不是包了一层 trait 的 RISC-V。顺序是 x86_64 → aarch64 → loongarch64。

## 8. 上层能力地图、可复用组件目录与调包计划

总原则：复用 no_std、embedded、portable-C 生态，不重写一切；兼容层绝不进 Core。第三方库不得直接调 Core，链路固定为：库的原生 API → 组件内 adapter → native semantic interface → provider → Core。库源码里出现 `kcore_*` 就是设计错误。

### 8.1 上层能力地图

> ground truth 以第 3 节的模块状态与第 5 节验证矩阵为准（2026-09-28 快照）。**下列能力全部属于组件（Component），不是 Core**：Core 不增加文件系统、TCP/IP、POSIX 代码（判断标准见 `AGENTS.md` 与 `docs/philosophy/core-philosophy.md`）。唯一例外是"安全 / 隔离"一行，它是 Core/Arch 的机制，组件只消费加密原语。

| 能力 | KaleidOS 现状 | 计划复用 | 前置 |
|---|---|---|---|
| 存储 / 文件系统 | 部分 IMPLEMENTED（只读）：`fatfs`（只读 FAT）与 `littlefs`（v2.9.3）两个 C `.kcomp`，已有只读 Local+Remote Fat VFS/namespace；little Remote 与写支持未有 | C 路线：lwext4（ext2/3/4，许可待定）；Rust 路线：Hadris（MIT，FAT/exFAT）、ext4-view（MIT/Apache，只读 ext4）。写支持与 VFS 自研（`docs/interfaces/filesystem.md`） | lwext4 的 GPLv2 许可策略先定；FS 契约定稿；块设备路径已就绪 |
| 网络（TCP/IP） | SKELETON：服务 ABI / SDK 代理 / 用例与私有 smoltcp 后端占位；bind / create 拒绝，无运行服务 / 网卡驱动 / NetDevice ABI | smoltcp（0BSD，已引入）；lwIP（Modified BSD，后备） | `NetDevice` + 网卡驱动、原语 / 同步 / 服务 adapters、跨组件通知 / 显式 timer；lwIP 还需 Thread/Sync |
| WiFi | NOT IMPLEMENTED | 按芯片：ESP32 系列 → esp-radio；Pico W → cyw43；参考 supplicant：wpa_supplicant/hostapd（BSD-3，Zephyr 移植）。多数芯片固件自带 802.11/WPA，主机侧只需驱动 + HCI/帧交换 | 对应硬件；SDIO/SPI 总线；固件加载机制（WiFi blob） |
| 蓝牙 | NOT IMPLEMENTED | TrouBLE（trouble-host，MIT/Apache）+ bt-hci（Controller 缝）；C 后备 NimBLE（Apache-2.0） | HCI 传输（UART/USB/SDIO）+ 同步原语；本期不做 BR/EDR |
| USB | NOT IMPLEMENTED | 设备侧：usb-device（MIT）+ usbd-*，或 embassy-usb（MIT/Apache）；C 后备 TinyUSB（MIT）。主机侧：xhci（MIT/Apache，只给寄存器/context/ring 原语）+ 自写驱动 | 主机侧先要 PCIe 枚举（DMA 机制已有，PCIe 没有）；设备侧要同步原语 |
| 图形 / 显示 | NOT IMPLEMENTED（monitor 是串口文本，不是图形栈） | MCU 级：embedded-graphics（MIT/Apache）+ mipidsi（MIT）；UI 级：LVGL（MIT）；Slint 嵌入式用途不在免费许可内 | 显示端点契约（`DrawTarget` 形状）+ panel/framebuffer 组件 |
| 音频 | NOT IMPLEMENTED | 无单一栈，需组合：embedded-i2s（传输）+ wm8960/es7210（codec/ADC）+ biquad/dasp（DSP）+ lc3-codec/opuscule（编解码）；流水线参考 daisy-embassy | I2S + DMA 搬运 + 同步原语；音频契约自定 |
| 加密 / TLS | NOT IMPLEMENTED：Core 无 crypto，SDK 不携带 TLS | embedded-tls（Apache-2.0，无分配器）或 Mbed TLS（取 Apache-2.0 分支，C FFI）+ RustCrypto 原语（MIT/Apache） | RNG（熵源在 Arch/Core 侧）、Clock、Socket/传输 |
| 电源管理 | NOT IMPLEMENTED：没有 cpufreq/cpuidle、runtime PM、suspend/resume | 本轮 4 份调研没有覆盖到可复用候选；需要单独调研或自研 | 先有设备模型与真机；QEMU-only 阶段不做 |
| 安全 / 隔离 | IsolatedNative（S + 私有 AS）EXPERIMENTAL；SandboxedNative（U-mode + `ecall`）NOT IMPLEMENTED。这是 Core/Arch 机制，不是组件能力 | 组件侧能复用的是加密原语（RustCrypto）与 TLS 栈；隔离本体不靠第三方库 | ASID、U-mode/`ecall`、syscall wire ABI（见 3.16 / 3.17） |

### 8.2 可复用组件目录

四份调研的合并结果（数据时点 2026-09-27）。每行的**分组标题即该行的能力**；列：项目 / 仓库 / 语言 / 许可 / no_std / 成熟度 / 前置与备注。同一项目在多个能力下出现时只保留一行最完整的记录，并在相关处注明。`⚠️` 表示许可或状态待核实，与调研原文一致。

#### 8.2.1 存储 / 文件系统 / NVMe

**文件系统**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| littlefs | github.com/littlefs-project/littlefs | C | BSD-3-Clause | Yes（裸机 C） | Production（v2.x） | 掉电安全、磨损均衡、RAM 有界；Zephyr/Mbed/ESP-IDF/RT-Thread 在用。**已在用**（v2.9.3，组件内 adapter） |
| FatFs / ff | elm-chan.org/fsw/ff（镜像 github.com/abbrev/fatfs） | C | ChaN（BSD 风格 / 1-clause，GPL 兼容） | Yes | Production | FAT12/16/32 + exFAT，经 `diskio.c` 硬件无关。**已在用**（只读）。R0.16 需 patch-2 修 CVE-2026-6682..6688 |
| Petit FatFs | elm-chan.org/fsw/ff/00index_p.html | C | ChaN | Yes | Production（极小） | 最小 MCU 的只读 FAT 子集；无官方 GitHub，只有镜像 |
| lwext4 | github.com/gkostka/lwext4 | C | GPL-2.0 整体（仅 `ext4_extent.c` / `ext4_xattr.c` 是 GPL，其余 BSD-3 可移除） | Yes | Mature | ext2/3/4 读写、journaling、extents；**静态链接有 copyleft 风险，纳入前先定许可策略** |
| littlefs2（crate） | github.com/trussed-dev/littlefs2 | Rust + C FFI | MIT/Apache（API）+ BSD-3（核心） | Yes（需 `c-stubs` 与 C 工具链） | Production（0.8.1） | littlefs 的惯用 Rust API；每个目标要 C 工具链；Trussed/SoloKeys 在用 |
| littlefs-rust | github.com/duanjr/littlefs-rust | Rust（纯） | ⚠️ 待核实（可能 MIT/Apache） | Yes | Experimental（2026） | 逐函数纯 Rust 移植、磁盘格式兼容；免 C 工具链；`littlefs-rust-core` 是不安全核心 |
| littlefs2-rust | crates.io/crates/littlefs2-rust | Rust（纯） | ⚠️ 待核实 | Yes | Experimental（2026） | 纯 Rust littlefs v2 挂载/读改；记录极少 |
| rust-fatfs（fatfs） | github.com/rafalh/rust-fatfs | Rust | MIT | Partial（`default-features=false`） | Mature but slow（0.3.6） | FAT12/16/32 + LFN；需要缓冲 `ReadWriteSeek` 适配，配 fscommon |
| Hadris（hadris-fat） | github.com/hxyulin/hadris | Rust | MIT | Yes（可选 `alloc`） | Emerging（v2.4，2026） | FAT12/16/32 + VFAT/LFN + exFAT；同 workspace 带分区表与磁盘镜像；`hadris-io` sync+async |
| embedded-sdmmc | github.com/rust-embedded-community/embedded-sdmmc-rs | Rust | MIT/Apache | Yes，无 alloc | Production（0.10） | SPI 模式 SD/MMC + FAT16/32 + `BlockDevice`；无原生 SDHCI |
| unifat | github.com/inex-rs/unifat | Rust | MPL-2.0 | Yes（+alloc） | Experimental（v0.1，2026） | 统一 FAT16/32 + exFAT，`embedded_io`；无 FAT12，仅一个版本 |
| simple-fatfs | github.com/alexkazik/forked-simple-fatfs | Rust | MIT | Yes | Emerging（2025） | FAT12/16/32，`embedded_io`；修 rust-fatfs 的嵌入式易用性，fork 血统 |
| embedded-fatfs | github.com/coliasgroup/rust-embedded-fat | Rust | MIT | Yes | 低活跃 / fork | 小；优先用维护中的 embedded-sdmmc |
| embedded-exfat | github.com/qiuchengxuan/exfat | Rust | MIT | Yes（+alloc，async） | Experimental | 仅 exFAT；no_std 用 `spin`（注意死锁） |
| ext4-view | github.com/nicholasbishop/ext4-view-rs | Rust | MIT/Apache | Yes（+alloc） | Mature（1.0.0） | **只读** ext2/3/4，fuzz 硬化、无 unsafe；适合只读 rootfs/initrd |
| ext4（crate） | crates.io/crates/ext4 | Rust | ⚠️ 待核实 | No（std） | Legacy（0.5，无维护） | 被 ext4-view 取代 |
| ext4-rw | github.com/suhteevah/ext4-rw | Rust | MIT/Apache | Yes（+alloc） | ⚠️ 未证明（2026 单人） | ext4 读写、extents、位图；无 journal/htree/校验和，只对干净挂载；不要放关键路径 |
| ext4-lwext4 / ext4-rs | github.com/arcbox-labs/ext4-rs | Rust FFI over C lwext4 | 默认 MIT/Apache；开 `gpl*` feature 变 GPL-2.0 | Partial（FFI；`FileBlockDevice` 是 std） | Experimental（2025） | 安全封装，默认排除 GPL 的 extents/xattr 文件 |
| lwext4-sys | crates.io/crates/lwext4-sys | Rust FFI | GPL-2.0 | Partial（FFI） | Legacy（2020） | 被 ext4-lwext4 取代 |
| RedoxFS | github.com/redox-os/redoxfs | Rust | MIT | No（userspace） | Production（Redox 内） | CoW、校验和、透明加密、resize；原生 CoW FS 的设计参考，需要 std/FUSE 工具链 |
| backhand | github.com/wcampbell0x2a/backhand | Rust | MIT/Apache（LZO 路径 GPL） | No（std） | Active | SquashFS 读/建/改；只读 rootfs 载荷的主机工具 |
| SPIFFS | github.com/pellepl/spiffs | C | MIT | Yes | Legacy / 已弃用 | 被 littlefs 取代；ESP-IDF 兼容保留，掉电语义有坑 |
| UFFS | github.com/earlephilhower/uffs | C | GPL-2.0/LGPL | Yes（NAND） | 无维护 | 仅 NAND，含坏块与磨损均衡；小众 |

**闪存转换 / 磨损均衡 / 坏块**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| embedded-storage（+async） | github.com/rust-embedded-community/embedded-storage | Rust | MIT/Apache | Yes | Production 标准 | `ReadStorage`/`Storage`/`NorFlash`/`NandFlash` traits，生态互操作脊柱；块设备与 FS 都向它靠 |
| sequential-storage | github.com/tweedegolf/sequential-storage | Rust | MIT/Apache | Yes | Production | 日志结构 KV/队列 + 磨损均衡 + 掉电安全；flash 内格式 semver 未稳 |
| lean-ftl | github.com/sebastien-riou/lean-ftl | C | Apache-2.0 | Yes | Experimental（2025） | 最小 FTL：磨损均衡 + 防撕裂 + 事务；防撕裂经仿真器穷测 |
| esftl | github.com/thearistotlemethod/esftl | C | ⚠️ 待核实 | Yes | Experimental | ≤128 MB flash 的 FTL，磨损均衡/坏块；小众、低活跃 |
| SPIFTL | github.com/earlephilhower/SPIFTL | C++ | ⚠️ 待核实 | Yes | Niche | MCU 静态磨损均衡；RP2040/Arduino 生态 |
| ekv / ekv-fs | crates.io/crates/ekv-fs | Rust | ⚠️ 待核实（Embassy，可能 MIT/Apache） | Yes（async） | Emerging | Embassy KV + 分块 VFS；VFS 层加路径/大 blob/流式 |

NAND 坏块管理通常属于驱动/控制器（MTD），不是可移植 crate；littlefs/UFFS/lwext4 在 FS 层处理各自目标的磨损/坏块。

**块 / 页缓存**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| fscommon | github.com/rafalh/rust-fatfs/tree/master/fscommon | Rust | MIT | Yes | Mature | `BufStream` 块/扇区缓冲，配 rust-fatfs |
| foyer | github.com/foyer-rs/foyer | Rust | Apache-2.0 | No（std/tokio） | Production | OS 级用户态缓存（S3-FIFO/LRU/LFU + 块引擎）；不是 no_std |
| quick_cache | crates.io/crates/quick_cache | Rust | MIT | Partial（需 alloc） | Production | 轻量并发 S3-FIFO/LRU；不是 no_std 核心 |
| caches-rs | github.com/al8n/caches-rs | Rust | MIT/Apache | Yes（`libm`,`hashbrown`） | Emerging | LRU/Segmented/2Q/TinyLFU 等，feature 开 no_std |
| cache-rs | github.com/sigsegved/cache-rs | Rust | MIT | Yes（+alloc, hashbrown） | Pre-1.0（2025） | LRU/SLRU/LFU/LFUDA/GDSF；次版本 API 可能变 |
| lru | github.com/jeromefroe/lru-rs | Rust | MIT | Yes | Mature | 经典 O(1) LRU；MSRV 1.85 |
| const-lru | crates.io/crates/const-lru | Rust | ⚠️ 待核实 | Yes | Niche | const 泛型、非哈希、固定容量 LRU，无堆分配 |

没有现成的 no_std page cache（脏页跟踪、回写、预读）：页缓存策略属于 storage 组件自研，用上面的 LRU 原语 + fscommon 式扇区缓冲拼。

**NVMe / AHCI 块驱动**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| OpenVMM `nvme-driver` | openvmm.dev（github.com/microsoft/openvmm） | Rust | MIT | Yes | OS-class，Production | 可移植 NVMe 块驱动，IRQ 驱动、`rdif-block` capability 边界；最成熟的 Rust NVMe 候选 |
| nvme-oxide | crates.io/crates/nvme-oxide | Rust | ⚠️ 待核实 | Yes | Experimental，极小 | 内核/bootloader 用轻量 NVMe；与 Oxide Computer 无关 |
| nvme-nostd | github.com/suhteevah/nvme-nostd | Rust | MIT/Apache | Yes（+alloc，identity 映射 DMA） | ⚠️ 未证明（2026，1 commit） | NVMe 1.4、admin/IO 队列、PRP；需全局分配器与 identity 映射 |
| ahci-nostd | github.com/suhteevah/ahci-nostd | Rust | ⚠️ 待核实（可能 MIT/Apache） | Yes | ⚠️ 未证明（2026 单人） | no_std AHCI/SATA；来源同 nvme-nostd，谨慎 |
| nvme（crate） | github.com/lihanrui2913/nvme | Rust | MIT/Apache | Yes | 已 yank / 陈旧 | 简单 no_std NVMe；crate 已 yank（0.2.2），仅参考 |
| SPDK | github.com/spdk/spdk | C | BSD-3-Clause | No（Linux/FreeBSD 用户态 + DPDK） | OS-class Production | 轮询用户态 NVMe 全家桶；太重，不能塞进组件，做主机侧参考 |
| libnvme | github.com/linux-nvme/libnvme | C | LGPL-2.1+ | No（Linux 用户态，需 `/sys`） | Production | 类型/命令/解码/管理，不是块驱动；copyleft |
| Rust-for-Linux NVMe | rust-for-linux.com/nvme-driver | Rust | GPL-2.0 | No（内核模块） | Experimental | 不能搬出 Linux，仅参考 |

**VirtIO**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| virtio-drivers | github.com/rcore-os/virtio-drivers | Rust | MIT | Yes（`alloc` 可选） | Production（0.13） | blk/net/console/gpu/input/rng/rtc/vsock；legacy+MMIO+PCI(ECAM)；实现 `Hal`（phys↔virt + DMA）。**已在用**（`virtio_blk` 组件私有，不 fork）。组织是 `rcore-os`，不是 `r3-os` |
| rust-vmm/vm-virtio | github.com/rust-vmm/vm-virtio | Rust | Apache-2.0/BSD-3 | No（std） | Production | VMM/设备侧的 virtio-blk 请求解析与 virtqueue；不是 guest 驱动 |
| virtio-bindings | crates.io/crates/virtio-bindings | Rust | BSD-3/MIT | Yes | Active | virtio 规格常量/bindings，配合 virtio-drivers |

**SD / eMMC**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| sdmmc-protocol + sdhci-host + dwmmc-host | github.com/rcore-os/tgoskits | Rust | Apache-2.0 | Yes | Emerging，活跃（2026） | 协议核心 + SDHCI/DesignWare 后端；RK3568/3588 验证过；SDHCI 对很多 RISC-V SoC 通用 |
| sdmmc | crates.io/crates/sdmmc | Rust | ⚠️ 待核实 | Yes | Experimental（0.1，2026） | 仅 Rockchip RK3568/3588（DWCMSHC） |
| stm32h7xx-hal `sdmmc` | github.com/stm32-rs/stm32h7xx-hal | Rust | MIT/Apache | Yes | Production（STM32） | 仅 STM32H7 |
| esp-hal `sdmmc` | docs.espressif.com/projects/rust/esp-hal | Rust | MIT/Apache | Yes（async） | Production（ESP32） | 仅 ESP32 |
| Zephyr SDMMC / SDHC / disk | docs.zephyrproject.org（storage/disk） | C | Apache-2.0 | Yes | Production | 可移植 SD/MMC/eMMC 栈 + 大量树内驱动；新 SoC 的最佳参考移植 |
| Linux `drivers/mmc` | github.com/torvalds/linux/tree/master/drivers/mmc | C | GPL-2.0 | No | Production | 仅参考（copyleft、内核耦合） |

**分区表 / initramfs**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| gpt / mbr / hadris-partition | github.com/hxyulin/hadris | Rust | permissive（逐 crate 核实） | Yes/partial | Emerging | 任何 FS 组件底下都需要；hadris workspace 是 MIT |
| cpio / tar-no-std | crates.io/crates/cpio、crates.io/crates/tar-no-std | Rust | permissive（待核实） | Yes | Emerging | `.kcomp` / initramfs 载荷解包 |

#### 8.2.2 网络 / WiFi / 蓝牙

**嵌入式 TCP/IP 栈**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| smoltcp | github.com/smoltcp-rs/smoltcp | Rust | 0BSD | ✅ no_std、无堆 | High（调研 v0.13.1；骨架固定 v0.14.0） | TCP/UDP/ICMP/raw、IPv4/IPv6、DHCP、DNS、ARP、802.15.4；`phy::Device` trait 就是 MAC 驱动缝；已引入 netstack 骨架，RV64 / RV32 交叉编译与打包通过；没有网络实测 |
| embassy-net | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | ✅ async no_std / no-alloc | High（v0.9.1） | smoltcp 之上的 async 封装（TCP/UDP/DNS/DHCPv4、`embedded-io`）；要接受 embassy 执行器 |
| embassy-net-driver / -driver-channel | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | ✅ | High | 驱动 trait + 包队列 channel；组件 NIC ABI 可照抄 |
| embassy-net-ppp / -nrf91 | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | ✅ | Medium | PPP over UART；nRF91 蜂窝网卡 |
| lwIP | git.savannah.gnu.org/cgit/lwip.git（镜像 github.com/lwip-tcpip/lwip） | C | Modified BSD（BSD-3-Clause 式） | ✅ 裸机或任意 RTOS | Very high（v2.2.1，20+ 年） | 全栈 + sockets；厂商 SDK 通用；需 Thread/Sync，~40–80 KB flash |
| picoTCP / PicoTCP-NG | github.com/tass-belgium/picotcp · github.com/virtualsquare/picotcp | C | GPL-2.0/3.0 或商业 | ✅ | Medium，停更（2023） | 模块化 IPv4/6；**许可红旗** |
| uIP | github.com/adamdunkels/uip（contiki-os/contiki 树内） | C | BSD-3-Clause | ✅ | Legacy 但稳定（1.0，2013） | 单缓冲、proto-thread、~4–5 KB；除最受限目标外不如 smoltcp |
| Contiki-NG | github.com/contiki-ng/contiki-ng | C | BSD-3-Clause | ✅ | High | OS + uIP/6LoWPAN/RPL/6TiSCH/CoAP |
| RIOT GNRC | github.com/RIOT-OS/RIOT | C | LGPL-2.1 | ✅ | High | 模块化 IPv6/6LoWPAN/RPL/UDP + netdev API；LGPL 注意 |
| Zephyr net stack | github.com/zephyrproject-rtos/zephyr | C | Apache-2.0 | ❌ 需 Zephyr | Very high | OS 耦合，不能当组件库 |
| FreeRTOS+TCP | github.com/FreeRTOS/FreeRTOS-Plus-TCP | C | MIT | ❌ FreeRTOS-only | High | OS 耦合；无 TLS/应用层 |
| Eclipse NetX Duo | github.com/eclipse-threadx/netxduo | C | MIT | ❌ ThreadX 耦合 | High | OS 耦合 |
| CycloneTCP | oryx-embedded.com（商业） | C | GPL-2.0 或商业 | ✅ | High | TCP/IP + TLS/SSH/IPsec/Modbus/SNMP + 多驱动；商业许可 |
| Mongoose | github.com/cesanta/mongoose | C | GPL-2.0 或商业 | ✅ | High | ~3.3k LOC 内建栈 + HTTP/WS/MQTT/TLS；许可红旗 |
| µC/TCP-IP | github.com/weston-embedded/uc-tcp-ip | C | Apache-2.0 + 商业 | ✅ | High | 双 IPv4/v6、TLS socket、Eth/WiFi/PHY；双许可 |
| microps | github.com/pandax381/microps | C | MIT | ✅（tun/tap） | Low（教学） | 学习级 TCP/IP，不适合生产 |
| tinytcp | github.com/rkimball/tinytcp | C/C++ | BSD-3-Clause | ✅ 静态分配 | Low-Medium | IP/TCP/ARP + 微型 HTTP，内存确定 |
| nano-ip | github.com/nanosoft-net/nano-ip | C | LGPL-3.0 | ✅ 16/32 位 | Low / 停更（2018） | 许可注意，niche |
| nanonet | AMBIGUOUS（无权威嵌入式 C 实现） | — | — | — | — | 唯一命中是 Arduino C++ 栈与无关 Rust 网络模拟器；**纳入前先找到来源** |
| embedded-nal / embedded-nal-async | github.com/rust-embedded-community/embedded-nal | Rust | MIT/Apache | ✅ | High | 网络抽象 traits（socket 形状），不是栈；组件 ABI 的形状参考 |
| embedded-svc | docs.rs/embedded-svc | Rust | MIT OR Apache-2.0 | ✅ | Stable-ish | `Wifi`/`Dns`/`Wifi` 供应商中立 traits（不是驱动） |

**WiFi**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| esp-radio（原 esp-wifi） | github.com/esp-rs/esp-hal（esp-radio/） | Rust | MIT OR Apache-2.0 | ✅ no_std | High，v1.0.0-beta（2026） | Wi-Fi STA/AP/scan、sniffer、ESP-NOW、BLE、802.15.4、coex；`embassy-net-driver` + `bt-hci` 集成；依赖 esp-hal（见 8.2.4），API 仍在变 |
| esp-hosted | github.com/espressif/esp-hosted | C | Apache-2.0 | ✅（ESP 侧固件） | Medium | 把 ESP32 变成 SDIO/SPI/UART 的 WiFi/BT NIC；配 embassy-net-esp-hosted |
| embassy-net-esp-hosted | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | ✅ | Medium | 非 ESP 主机 + ESP32 射频 |
| cyw43 | github.com/embassy-rs/embassy（cyw43/） | Rust | MIT OR Apache-2.0 | ✅ async | Medium-High | CYW43439 WiFi STA/AP/scan + BLE HCI；RP2040/2350（Pico W/2W）；实现 `embassy-net-driver` 与 `bt-hci` controller traits |
| cyw43-pio | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | ✅ | Medium | RP2040 PIO 半双工 SPI，释放硬件 SPI |
| Infineon WHD | github.com/Infineon/wifi-host-driver | C | Apache-2.0 | ✅ portable，RTOS hooks | High（v4.x；v5.x 掉 CYW43） | CYW43xx/CYW55500 等主机驱动；需固件 blob；CYW43 家族钉 v4.x，Rust 侧优先 cyw43 |
| Zephyr Wi-Fi | github.com/zephyrproject-rtos/zephyr（drivers/wifi） | C | Apache-2.0 | ❌ Zephyr | High | 原生 L2 + offload 驱动；OS 耦合 |
| wpa_supplicant / hostapd | github.com/zephyrproject-rtos/hostap（镜像） | C | BSD-3-Clause | ✅（Zephyr 移植） | Very high | WPA2/3、EAP、802.1X、PMF 的唯一实战级 supplicant；芯片固件已卸载 802.11/WPA 时不需要它 |
| supplicant-rs | github.com/structured-world/supplicant-rs | Rust | Apache-2.0 | ❌ Linux/nl80211 | 非功能（v0.1.0，2026-03） | 面向 Linux，不可嵌入 |
| shuli | github.com/cathay4t/shuli | Rust | （早期） | ❌ Linux | 早期，不可用 | 纯 Rust nl80211 daemon |
| NXP MXM / mwifiex（IW612 等） | github.com/nxp-imx/mwifiex-iw612 | C | GPL-2.0 或专有（MCUXpresso 的 RTOS 移植是 BSD-3） | MOAL 可移植到 FreeRTOS/Zephyr | High | NXP 无线 SoC；许可混合 |
| Realtek Ameba（WHC） | gitee.com/ameba-aiot/ameba-rtos（NuttX 移植） | C | Realtek SDK（Apache-2.0） | RTOS SDK | High | RTL8720/8721；固件在 NP 核 |

**蓝牙**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| TrouBLE（trouble-host） | github.com/embassy-rs/trouble | Rust | MIT OR Apache-2.0 | ✅ async no_std | Medium-High（v0.8+） | BLE Host：central/peripheral、GATT client+server、L2CAP CoC、可选 security/SMP；MSRV 1.80 |
| bt-hci | github.com/embassy-rs/bt-hci | Rust | MIT OR Apache-2.0 | ✅ | Medium | HCI 包类型 + `Controller` trait（controller/host 缝，TrouBLE 的基础） |
| nrf-sdc / nrf-mpsl | github.com/alexmoon/nrf-sdc | Rust 绑定 | 绑定 MIT/Apache；SoftDevice Controller 二进制专有 | ✅ no_std | High | 预认证 BLE Controller + MPSL；仅 nRF52/54 |
| nrf-softdevice | github.com/embassy-rs/nrf-softdevice | Rust（+C blob） | 绑定 MIT/Apache；SoftDevice 二进制/头文件专有 | ✅ no_std | High，legacy | nRF51/52 |
| NimBLE | github.com/apache/mynewt-nimble | C | Apache-2.0 | ✅ 裸机/任意 RTOS | High，SIG 认证 | BT 5.4 Host+Controller（L2CAP/ATT/GAP/GATT/SM）+ Mesh；最成熟的宽松 C 选项 |
| Zephyr Bluetooth | github.com/zephyrproject-rtos/zephyr（subsys/bluetooth） | C | Apache-2.0 | ❌ Zephyr | Very high | 全 Host+Controller、HCI over UART/SPI/USB、Mesh、LE Audio；OS 耦合 |
| BTstack | github.com/bluekitchen/btstack | C | 非商业免费；商业收费（源码可见） | ✅ 无需 RTOS | High（活跃 2026） | BR/EDR + LE 双模、SIG 认证；**商业许可红旗** |
| BlueZ | github.com/bluez/bluez | C | GPL-2.0-or-later | ❌ Linux | Very high | Linux 参考 Host，不可嵌入 |
| bleps | github.com/bjoernQ/bleps | Rust | MIT | ✅ no_std | Toy / 停更 | 极简 BLE host，已被 TrouBLE 取代 |
| embassy-stm32-wpan | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | ✅ | Medium | STM32WB 的 `bt-hci` controller（IPCC mailbox；需 ST FUS 固件） |
| bt-hci-linux | github.com/embassy-rs/bt-hci | Rust | MIT OR Apache-2.0 | ❌ Linux | Medium | HCI socket controller 适配；开发/测试 |

**以太网 MAC/PHY 与 USB 以太网**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| embassy-net-wiznet | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | ✅ async | High | W5100S/W5500/W6100/W6300 SPI 硬件 TCP 芯片 MACRAW；硬件卸载 TCP/UDP |
| embassy-net-w5500 | github.com/embassy-rs/embassy（独立仓库已归档） | Rust | MIT OR Apache-2.0 | ✅ | High | 仅 W5500；legacy |
| embassy-net-enc28j60 | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | ✅ | Medium | Microchip ENC28J60 SPI MAC+PHY |
| embassy-net-adin1110 | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | ✅ | Medium | Analog ADIN1110 10BASE-T1x 单对以太网 SPI MAC-PHY |
| eth-mdio-phy + eth-phy-lan87xx + eth-phy-lan867x | github.com/jethub-iot/eth-phy-rs | Rust | MIT（仓库确认） | ✅ no alloc | Medium（v0.3） | MDIO Clause-22 bus/PHY traits + LAN8710/8720/8740/8742、LAN867x；配 ESP32 EMAC/STM32 ETH |
| esp-emac | crates.io/crates/esp-emac | Rust | MIT OR Apache-2.0 | ✅ | Medium | ESP32 EMAC MAC + `EspMdio`；LAN8720 板 |
| STM32 ETH MAC | github.com/embassy-rs/embassy（embassy-stm32） | Rust | MIT OR Apache-2.0 | ✅ | High | STM32 内建以太网 MAC（F1/F2/F4/F7/H5/H7） |
| embassy-usb CDC-NCM | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | ✅ | Medium | 设备侧以太网-over-USB（CDC-NCM）→ embassy-net 网卡；也有 CDC-ECM 路径 |
| usbd-ethernet | github.com/dlkj/usbd-ethernet | Rust | MIT（确认） | ✅ | Medium | 走 `usb-device` 栈的 CDC-NCM |
| USB 以太网主机 dongle（AX88772/RTL8152/CDC-ECM/RNDIS） | Linux C：`asix`/`r8152`/`cdc_ether`/`rndis_host`（kernel.org）；Rust PoC：github.com/pdh11/cotton、fobnail/fobnail、Chitti `usb_eth` | C（Linux）/ Rust PoC | GPL-2.0（Linux C）；PoC 各自宽松 | Linux C ✅；Rust PoC 裸机 | 主机侧 Rust 不成熟（仅 PoC） | CDC-ECM（裸帧）、RNDIS（44 字节头）、ASIX/RTL 寄存器协议；真 dongle 要么 shim Linux C（GPL），要么自己写 |

#### 8.2.3 USB / 图形 / 音频

**USB 设备栈**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| TinyUSB | github.com/hathach/tinyusb | C | MIT | C（"No OS" OSAL 支持） | Very high（~7.1k★，10+ 年） | 全设备栈：CDC/HID/MSC/MIDI/UAC2/DFU/WebUSB/BTH HCI；无 malloc、静态缓冲、ISR 工作延后到任务上下文；`lib/` 与 `hw/mcu/` 逐文件许可。**最佳 C 选项**，需 port + OSAL shim |
| embassy-usb | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | yes + async | High（v0.6.0） | 异步设备栈（control/bulk/interrupt + 类管线）；需 embassy-executor/embassy-time；async 映射 Core 的栈切换 |
| usb-device | github.com/rust-embedded-community/usb-device | Rust | MIT | yes | Very high（v0.3.2） | 阻塞、`embedded-hal` 基础；类驱动最多，**最低风险的 Rust 路线** |
| usbd-hid | github.com/twitchyliquid64/usbd-hid | Rust | MIT OR Apache-2.0 | yes | High | HID 描述符 + 报告处理；配 usb-device |
| usbd-serial | github.com/mvirkkunen/usbd-serial | Rust | MIT | yes | High | USB CDC-ACM 串口；配 usb-device |
| usbd-midi | github.com/rust-embedded-community/usbd-midi | Rust | MIT | yes | Medium | USB MIDI 1.0；配 usb-device |
| usbd-audio | github.com/kiffie/usbd-audio | Rust | MIT OR Apache-2.0 | yes | Low-medium | USB Audio Class 1.0 流；配 usb-device |
| usbd-uac2 | github.com/ktims/usbd-uac2 | Rust | MIT | yes | Low | USB Audio Class 2.0；配 usb-device |
| usbd-dfu | github.com/vitalyvb/usbd-dfu | Rust | MIT | yes | Medium | USB DFU runtime 类；配 usb-device |
| usbd-human-interface-device | github.com/dlkj/usbd-human-interface-device | Rust | MIT | yes | Medium | 键盘（boot/NKRO）、鼠标、摇杆、Consumer Control；配 usb-device |

**USB 主机控制器 / 主机栈**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| xhci | github.com/rust-osdev/xhci | Rust | MIT OR Apache-2.0 | yes | Medium（v0.9.2） | xHCI 寄存器、context、extended caps、TRB ring——**不是完整驱动**；MMIO 映射与驱动自己写。最佳 xHCI 原语层 |
| xhci-nostd | github.com/suhteevah/xhci-nostd | Rust | Apache-2.0 | yes | Low（2026） | 可用 xHCI 3.0 host + HID 键盘；未证明，做参考 |
| crab-usb | github.com/rcore-os/tgoskits | Rust | Apache-2.0 | yes（rCore 裸机） | Medium（v0.12.1） | rCore 内嵌 USB host；确认能否干净抽出 |
| cotton-usb-host | github.com/pdh11/cotton | Rust | CC0-1.0 | yes | Low（v0.3.0） | 小型生态；公共领域许可 |
| embassy-usb-host | github.com/embassy-rs/embassy | Rust | MIT OR Apache-2.0 | yes + async | Very low（v0.1.0） | async USB host；新，观察成长 |
| usb-host | git.spork.org/usb-host.git | Rust | **crates.io LGPL-3.0+ / 仓库自称 MIT（冲突）** | yes 候选 | Low（v0.1.3） | ⚠️ 许可冲突未解决前不要用 |
| nusb | github.com/kevinmehall/nusb | Rust | MIT | std only | High | 主机 OS 用户态；不能进内核，仅参考 |
| rusb | github.com/a1ien/rusb | Rust | MIT | std only | High | libusb 包装；仅参考 |
| EHCI | 无打包 crate | — | — | — | — | 没有 no_std EHCI；Linux `ehci-hcd.c` 是 GPL-2.0，不能搬进宽松组件 |

**图形 / 显示**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| embedded-graphics | github.com/embedded-graphics/embedded-graphics | Rust | MIT OR Apache-2.0 | yes | Very high（v0.8.2） | 2D 原语、`DrawTarget`、字体；事实标准 no_std 绘制 API，驱动生态大 |
| mipidsi | github.com/almindor/mipidsi | Rust | MIT | yes | High（v0.10.0） | 通用 MIPI DCS 显示器驱动（ST7789/ILI9341/GC9A01…）；`embedded-hal`；有 async fork |
| ssd1306 | github.com/rust-embedded-community/ssd1306 | Rust | MIT OR Apache-2.0 | yes | High（v0.10.0） | SSD1306/SH1106 单色 OLED（I2C/SPI） |
| ili9341 | github.com/yuri91/ili9341-rs | Rust | MIT OR Apache-2.0 | yes | Medium | SPI ILI9341 TFT；mipidsi 的按面板替代 |
| st7789 | github.com/almindor/st7789 | Rust | MIT | yes | Medium | ST7789 SPI TFT + embedded-graphics |
| epd-waveshare / epdsi | github.com/caemor/epd-waveshare | Rust | MIT | yes | Medium | 墨水屏驱动 |
| LVGL | github.com/lvgl/lvgl | C | MIT | C（无需 OS/RTOS，无外部依赖） | Very high（v9.x） | 30+ 控件、样式、布局、CJK 排版；~32 kB RAM 起 + framebuffer + 1/10 屏缓冲；提供 display/input/tick shim |
| lvgl-rs | github.com/rafaelcaricio/lvgl-rs | Rust | MIT | Rust 绑定（C 核心） | Low-medium | 便利绑定，可能落后上游 |
| u8g2 | github.com/olikraus/u8g2 | C | BSD-2-Clause | C（裸机无依赖） | Very high | 单色 OLED/LCD + 大字体集；RAM 极小时用；需 SPI/I2C + delay 回调 |
| Slint | github.com/slint-ui/slint | Rust | GPL-3.0-only 或商业 | yes（`i-slint-core` + software-renderer） | High（v1.18） | ⚠️ **嵌入式用途不在免费 Royalty-free 许可内**，要 GPLv3 或付费商业许可 |
| slint-mipidsi-adapter | github.com/antonsterkhov/slint-mipidsi-adapter | Rust | MIT | yes（Slint no_std） | Low（v0.2.2） | 社区适配，年轻 |
| tiny-skia | github.com/linebender/tiny-skia | Rust | BSD-3-Clause | ❌ 需要 std + alloc | High | Skia CPU 子集；只适合有堆的 IsolatedNative 组件，不适合最小 sandbox |
| embedded-graphics-simulator | github.com/embedded-graphics/simulator | Rust | MIT OR Apache-2.0 | std（host） | Medium | 主机侧显示模拟器，开发/CI 工具 |

**音频**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| embedded-i2s | github.com/eldruin/embedded-i2s-rs | Rust | MIT OR Apache-2.0 | yes | Low（v0.1.0） | 通用 I2S trait（`embedded-hal`）；配 SoC 驱动 |
| stm32_i2s_v12x | github.com/samcrow/stm32_i2s | Rust | MIT OR Apache-2.0 | yes | Medium | STM32 SPI 外设当 I2S；仅 STM32F4 等 |
| rp2040-i2s | crates.io/crates/rp2040-i2s | Rust | MIT/Apache | yes | Low（v0.1.0） | RP2040 PIO 的 I2S 读写；MEMS 麦/DAC |
| pdm | github.com/lsartory/pdm | Rust | MIT | yes | Low（v1.0.0） | PDM 麦克风采集/抽取 |
| es7210 | github.com/QuackHack-McBlindy/es7210 | Rust | MIT | yes | Low（v0.1.0） | 4 通道 ES7210 音频 ADC；`embedded-hal` |
| wm8960 | github.com/imxrt-rs/wm8960-rs | Rust | MIT OR Apache-2.0 | yes | Low-medium | WM8960 寄存器图 + codec 驱动 |
| usbd-audio / usbd-uac2 | 见 8.2.3 USB 设备栈 | Rust | MIT / MIT-Apache | yes | Low | USB 音频设备功能 |
| biquad | github.com/korken89/biquad-rs | Rust | MIT OR Apache-2.0 | yes | Medium（v0.6.0） | 二阶 IIR（biquad）滤波器；混音/EQ |
| microdsp | github.com/stuffmatic/microdsp | Rust | MIT | yes | Low（v0.1.3） | 嵌入式 DSP 算法 |
| fundsp | github.com/SamiPerttu/fundsp | Rust | MIT OR Apache-2.0 | yes（查 feature） | Medium-high（v0.23.0） | 音频合成、滤波、DSP 图 |
| dasp | github.com/rustaudio/dasp | Rust | MIT OR Apache-2.0 | yes（查 feature） | High（v0.11.0） | 采样类型、转换、插值；缓冲/采样管道 |
| lc3-codec | github.com/ninjasource/lc3-codec | Rust | Apache-2.0 | yes | Low（v0.2.0） | BLE LE Audio LC3 编解码 |
| opuscule | codeberg.org/jojo-laplace/opuscule | Rust | MPL-2.0 | yes（有能力） | Low（v0.2.1） | 纯 Rust Opus 解码；⚠️ MPL-2.0 文件级 copyleft |
| daisy-embassy / daisy | github.com/daisy-embassy/daisy-embassy · github.com/zlosynth/daisy | Rust | MIT | yes + async | Low-medium | Daisy Seed/Patch SM 音频 BSP；完整 async I2S + DMA 流水线参考 |
| awedio | github.com/boppofun/awedio | Rust | MIT OR Apache-2.0 | alloc（非裸 no_std） | Medium（v0.8.0） | 低开销音频播放（ESP32 后端）；不是 no_std 核心候选 |
| esp-adf | github.com/espressif/esp-adf | C | Apache-2.0（repo 标注 NOASSERTION） | C，仅 ESP-IDF/FreeRTOS | High | ESP32 音频管线/codec/HAL；参考架构，不可移植 |
| embedded-audio | github.com/decaday/embedded-audio | Rust | Apache-2.0 | ? | Immature（v0.0.0） | 占位版本，不要依赖 |

#### 8.2.4 加密 / TLS / 驱动生态

**TLS / DTLS**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| Mbed TLS | github.com/Mbed-TLS/mbedtls | C | Apache-2.0 OR GPL-2.0-or-later（双许可） | 为裸机设计，可不需 OS/alloc | Very high（PSA 认证） | 默认嵌入式 TLS 栈；取 Apache-2.0 分支；已拆分，TLS + `TF-PSA-Crypto` 子树；作 `.kcomp` 私有 C FFI |
| TF-PSA-Crypto | github.com/Mbed-TLS/TF-PSA-Crypto | C | Apache-2.0 OR GPL-2.0-or-later | 裸机可用 | High（新） | PSA Cryptography API 参考实现（仅 crypto，不含 TLS）；配 mbedtls |
| embedded-tls | github.com/embassy-rs/embedded-tls | Rust | Apache-2.0 | yes：no_std、**无分配器** | Medium（0.19，WIP） | TLS 1.3 客户端；逐帧、client-only；其 webpki 验证器目前可能需要 std（纳入前核实） |
| rustls | github.com/rustls/rustls | Rust | Apache-2.0 OR ISC OR MIT | Partial：no_std + alloc（需自定义 `CryptoProvider`） | Very high | 默认 provider 需要 std/cmake；裸机走纯 Rust provider |
| rustls/webpki | github.com/rustls/webpki | Rust | ISC | yes（+alloc） | High | X.509 路径校验；rustls 现在用它 |
| briansmith/webpki | github.com/briansmith/webpki | Rust | ISC 风格（非 SPDX） | yes | Dormant（v0.22，2023） | 被 rustls/webpki 取代，新工作不要用 |
| BearSSL | bearssl.org（镜像 github.com/status-im/BearSSL） | C | MIT | yes：完全无 OS，只需 `mem*`/`strlen` | Medium / 停更（0.6，2018，上游自称 alpha） | TLS 1.0–1.2、常量时间；20 KB 代码 / 25 KB RAM 起；无 TLS 1.3 |
| wolfSSL | github.com/wolfSSL/wolfssl | C | GPL-3.0-or-later 仓库 + 商业双许可 | yes 裸机 | Very high，CVE 活跃 | 能力全，但 GPL，除非买商业许可；宽松 OS 避免 |
| Intel tinycrypt | github.com/intel/tinycrypt | C | Intel BSD-3 风格 | yes 裸机 | 归档（2024），Zephyr 内维护 | 只有原语（AES/SHA/HMAC/ECC），无 TLS |
| MatrixSSL | github.com/gitcollect/matrixssl | C | GPL/商业 | yes | Low / 冷门 | 除非接受许可，否则优先 Mbed TLS |

**RustCrypto 原语与绑定（大多纯 Rust、no_std）**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| aes | github.com/RustCrypto/block-ciphers | Rust | MIT OR Apache-2.0 | Yes | High | AES/分组密码；常量时间，可选硬件 AES |
| sha2 | github.com/RustCrypto/hashes | Rust | MIT OR Apache-2.0 | Yes | Very high | SHA-2 家族 |
| p256 | github.com/RustCrypto/elliptic-curves | Rust | Apache-2.0 OR MIT | Yes | High | P-256 ECDH/ECDSA + 曲线运算 |
| ed25519-dalek | github.com/dalek-cryptography/curve25519-dalek | Rust | BSD-3-Clause | Yes | Very high（v3） | Ed25519 签名/验签；许可与其余 RustCrypto 不同 |
| rsa | github.com/RustCrypto/RSA | Rust | MIT OR Apache-2.0 | Yes | Medium（0.10 rc） | 纯 Rust RSA；验签优先，签名慢 |
| aes-gcm / chacha20poly1305 | github.com/RustCrypto/AEADs | Rust | Apache-2.0 OR MIT | Yes | High | AEAD；可选硬件加速 |
| hkdf | github.com/RustCrypto/KDFs | Rust | MIT OR Apache-2.0 | Yes | High | HKDF 密钥派生 |
| x509-cert / pkcs8 / der | github.com/RustCrypto/formats | Rust | Apache-2.0 OR MIT | Yes | High | X.509 / PKCS / DER-PEM 编解码；只解析，无路径校验 |
| signatures | github.com/RustCrypto/signatures | Rust | MIT OR Apache-2.0 | Yes | High | ECDSA/Ed25519 签名 traits |
| rustls-rustcrypto | github.com/RustCrypto/rustls-rustcrypto | Rust | Apache-2.0 | Experimental，no_std 扩展 | Low | rustls 的 RustCrypto `CryptoProvider`；纯 Rust no_std rustls 的关键，风险自担 |
| rustls-mbedtls-provider | github.com/fortanix/rustls-mbedtls-provider | Rust | Apache-2.0 | 经 mbedtls | Low | rustls → C mbedtls 的桥 |
| mbedtls（Rust wrapper） | github.com/fortanix/rust-mbedtls | Rust | Apache-2.0 OR GPL-2.0+ | Yes no_std capable | Medium | Mbed TLS 的安全 Rust FFI |
| wolfssl-rs | github.com/expressvpn/wolfssl-rs | Rust | GPL-2.0+ | Yes | Low | 带 wolfSSL 的 GPL 传染 |

**embedded-hal 可移植层与代表性驱动 crate**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| embedded-hal v1.0 | github.com/rust-embedded/embedded-hal | Rust | MIT OR Apache-2.0 | Yes | Very high | 阻塞 HAL traits：i2c/spi/gpio/pwm/delay/adc/serial/can/rng |
| embedded-hal-async | github.com/rust-embedded/embedded-hal | Rust | MIT OR Apache-2.0 | Yes | High | 异步孪生 traits |
| embedded-hal-bus | github.com/rust-embedded/embedded-hal | Rust | MIT OR Apache-2.0 | Yes | Medium | `RefCellDevice`/`CriticalSectionDevice`/`MutexDevice` 共享 SPI/I2C 总线 |
| embedded-io / embedded-io-async | github.com/rust-embedded/embedded-hal | Rust | MIT OR Apache-2.0 | Yes | High | IO `Read`/`Write`（+async）traits；virtio-drivers console 在用 |
| bme280 | github.com/VersBinarii/bme280-rs | Rust | MIT OR Apache-2.0 | Yes | Medium（0.5，2024） | BME280/BMP280 温湿压 |
| sh1106 | github.com/rust-embedded-community/sh1106 | Rust | Apache-2.0 | Yes | Medium | SH1106 OLED |
| mpu6050 | github.com/juliangaal/mpu6050 | Rust | MIT | Yes | Low/稳定（2022） | MPU6050 六轴 IMU |
| ina219 | github.com/scttnlsn/ina219 | Rust | MIT/Apache-2.0 | Yes | Medium | INA219 电流/功率监测 |
| lora-phy / lorawan-device | github.com/lora-rs/lora-rs | Rust | MIT | Yes（async） | High | LoRa PHY + LoRaWAN end-device |
| spi-memory | github.com/jonas-schievink/spi-memory | Rust | 0BSD | Yes | Medium | SPI flash/EEPROM 驱动 |
| device-driver | github.com/diondokter/device-driver | Rust | MIT OR Apache-2.0 | Yes | Medium | 寄存器映射 DSL/codegen，快速写新驱动 |
| embedded-hal-mock | github.com/rust-embedded/embedded-hal-mock | Rust | Apache-2.0 | Yes | High | 无硬件 mock；适配 CoreTest / host test |
| eldruin driver family | github.com/eldruin（ads1x1x-rs、ds323x-rs、lsm303agr-rs、max3010x-rs、lm75-rs、mcp49xx-rs…） | Rust | MIT/Apache-2.0 | Yes | 各 crate High | 最丰富的单人驱动家族（传感器/执行器） |
| ssd1306 | 见 8.2.3 图形 | Rust | MIT OR Apache-2.0 | Yes | High | OLED 驱动，同时是 embedded-hal 驱动代表 |

**HAL 实现与厂商驱动目录（移植来源）**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| stm32-rs | github.com/stm32-rs/stm32-rs | Rust/Python | MIT/Apache-2.0 | Yes | Very high | STM32 PAC + HAL |
| rp-hal | github.com/rp-rs/rp-hal | Rust | MIT/Apache-2.0 | Yes | High | RP2040/RP235x HAL |
| esp-hal | github.com/esp-rs/esp-hal | Rust | MIT/Apache-2.0 | Yes | High | ESP32 no_std HAL（esp-radio/sdmmc 依赖它）；仅 ESP32 系列 |
| atsamd | github.com/atsamd-rs/atsamd | Rust | MIT/Apache-2.0 | Yes | High | ATSAMD PAC + HAL |
| embassy | github.com/embassy-rs/embassy | Rust | MIT/Apache-2.0 | Yes | Very high | async 框架 + 多家 HAL + net/usb/crypto 组件 |
| Bosch BME280_SensorAPI（+BMP/BNO/BME68x） | github.com/boschsensortec/BME280_SensorAPI | C | BSD-3-Clause | Yes | High（厂商） | 可移植 C，易 FFI 进 `.kcomp` |
| Sensirion embedded-common | github.com/Sensirion/embedded-common（+各传感器仓库） | C | BSD-3-Clause | Yes | High（厂商） | SHT/SGP/SEN5x 驱动 |
| Semtech SX126x | github.com/Lora-net/sx126x_driver | C | BSD-3-Clause-Clear | Yes | High（厂商） | LoRa 射频规范驱动 |
| nrf-hal | github.com/nrf-rs/nrf-hal | Rust | MIT/Apache-2.0 | Yes | High | Nordic nRF HAL |
| avr-hal | github.com/Rahix/avr-hal | Rust | MIT/Apache-2.0 | Yes | High | AVR HAL |

**驱动目录 / 聚合器（driver as a library）**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| awesome-embedded-rust | github.com/rust-embedded/awesome-embedded-rust | Markdown | CC0 风格 | n/a | High | 驱动 + HAL 索引，生态路标 |
| Zephyr | github.com/zephyrproject-rtos/zephyr | C | Apache-2.0 | Yes（架构矩阵巨大） | Very high | `drivers/` 约 3,487 个 C 文件、104 个驱动类；**最佳宽松目录**（另有 net/BT/WiFi 栈，OS 耦合） |
| Apache NuttX | github.com/apache/nuttx（+ nuttx-apps） | C | Apache-2.0 | Yes | Very high | `drivers/` 约 62 个驱动类；有 pci/virtio/usbhost |
| RT-Thread + packages | github.com/RT-Thread/rt-thread（+ packages） | C | Apache-2.0 | Yes | Very high | RTOS + 包索引（外设/iot/security/system/tools） |
| libdriver | github.com/libdriver | C | MIT | Yes（MCU + Linux） | High，活跃 | 185 个 C 驱动库，一 chip 一 repo；最丰富的宽松 C 目录（mpu6050/bme280/ssd1306/sht3x…） |
| Tock | github.com/tock/tock | Rust | MIT/Apache-2.0 | Yes | High | 安全嵌入式 OS，driver "capsules"；driver-as-library 设计参考 |

**OS-class 可复用驱动（不包含前几节已列的存储/网络/USB 行）**

| 项目 | 仓库 | 语言 | 许可 | no_std | 成熟度 | 前置 / 备注 |
|---|---|---|---|---|---|---|
| pci_types | github.com/rust-osdev/pci_types | Rust | MIT/Apache-2.0 | Yes | Medium | PCI 类型 / config space / BAR 解析；枚举起点，配 virtio-drivers 的 PCI transport |
| CherryUSB | github.com/cherry-embedded/CherryUSB | C | Apache-2.0 | Yes | High | USB host + device C 栈；可移植、高性能；做 TinyUSB 之外的 C 后备 |
| rust-osdev/acpi | github.com/rust-osdev/acpi | Rust | Apache-2.0 | Yes | Medium | ACPI 表 + AML 解析；PCIe/ACPI 枚举面 |
| rust-vmm（vm-virtio、vm-memory、acpi_tables、vmm-sys-util） | github.com/rust-vmm/rust-vmm | Rust | Apache-2.0/BSD-3 | Yes（多数） | Very high（Firecracker/Cloud Hypervisor） | 主机/VMM 侧；`vm-memory` 已并入 monorepo（旧仓库归档） |
| pciids | github.com/pciutils/pciids | 数据 | 构建期依赖 | n/a | Very high | PCI vendor/device ID 数据库；配枚举驱动 |
| virtio-drivers / nvme 系 / smoltcp / embedded-sdmmc / embedded-nal / xhci / embassy-usb-host / usb-device / TinyUSB / nusb / rusb | 见 8.2.1 / 8.2.2 / 8.2.3 | — | — | — | — | 调研原文把它们也归在 OS-class；本文按能力分组，已在对应小节保留完整行，此处不重复 |

**embedded-hal 适配链（组件侧的统一入口）**

```text
第三方 embedded-hal 驱动 crate（不改）
  → KaleidOS adapter（`kcomp-embedded-hal` 或 SDK feature，实现 embedded-hal traits）
  → Endpoint → controller component → Device / MMIO / IRQ / DMA → Core
```

明确警告：embedded-hal 覆盖 sensor、display、SPI/I2C、GPIO、radio 等嵌入式外设（crates.io 上 keyword `embedded-hal-driver` 约 397 个、匹配 `embedded-hal` 约 1,057 个，2026-09，下界），**不覆盖** NVMe、PCIe、xHCI、GPU、现代 PC 网卡这类 OS-class 驱动。后者必须从 8.2.1 / 8.2.3 / 8.2.4 的 OS-class 行里另找候选，或者自己写。

**许可约束（属于硬前置，纳入前必须先定）**

- 宽松、可直接进组件：smoltcp（0BSD）；TinyUSB（MIT；`lib/` 与 `hw/mcu/` 逐文件复核）；lwext4 的 BSD-3 子集；FatFs/littlefs（BSD 风格）；virtio-drivers、Hadris、ext4-view、embedded-sdmmc、embedded-storage、embedded-hal 系列（MIT/Apache/0BSD）；Mbed TLS 取 Apache-2.0 分支；embedded-tls 与 RustCrypto（Apache-2.0/MIT）；Zephyr、NuttX、RT-Thread（Apache-2.0）；libdriver（MIT）。
- copyleft 或商业风险，先定策略再谈代码：lwext4（GPL-2.0；BSD-3 子集可剥离）；wolfSSL 与 wolfssl-rs（GPL-3.0/商业）；libnvme（LGPL-2.1+）；RIOT GNRC（LGPL-2.1）；nano-ip（LGPL-3.0）；picoTCP、CycloneTCP、Mongoose（GPL/商业）；BTstack（非商业免费、商业收费）；BlueZ（GPL-2.0）；nrf-sdc/nrf-softdevice 的 SoftDevice 二进制（专有）；NXP MXM（GPL/专有）；opuscule（MPL-2.0 文件级）；Slint（嵌入式不在免费许可内）；`usb-host`（crate 与仓库许可冲突）。
- 结论：**许可先定，再进目录**。同一能力优先选宽松候选；GPL/商业候选只有在明确接受其许可、或换实现之后才进入组件。

### 8.3 embedded 系列（全部是计划，仓库里现在为零）

- `kcomp-embedded-hal`（或 SDK feature）：GPIO/SPI/I2C/Delay/PWM 适配层。调用链是：第三方 embedded-hal 驱动 → KaleidOS adapter → Endpoint → controller component → Device/MMIO/IRQ/DMA → Core。它覆盖 sensor、display、SPI-I2C、GPIO、radio、各类嵌入式外设；它不会带来 NVMe、PCIe、xHCI、GPU、现代 PC 网卡这类 OS-class 驱动。
- `kcomp-embedded-io`：Read/Write，覆盖 UART、USB-CDC、TCP、console 之类的流。
- `kcomp-embedded-storage`：NOR/SPI flash、EEPROM、MCU flash，落到 littlefs、KV、config、firmware 存储，也是 MCU profile 的基础。
- `kcomp-embedded-nal`：TCP/UDP/DNS，绑定到未来的 `NetDevice`。
- `kcomp-embedded-graphics`：`DrawTarget`，写往 display endpoint 或 framebuffer。
- 可能还要 `kcomp-embedded-can`（CAN 总线）。
- 落地方式二选一：独立伴生 crate，或 SDK feature。先做 embedded-hal 与 embedded-io 两个，它们能最快检验 adapter 链路是否站得住。

### 8.4 已点名的调包候选（来自 `docs/architecture/porting.md`）

已经落地的只有两个：FatFs（只读 FAT）与 littlefs（v2.9.3），都是 C `.kcomp` 加组件内 adapter。

候选与前置条件：

- `lwext4`（ext2/3/4，二档）：GPLv2，纳入前必须先定许可策略（独立 profile、只用 BSD 子集，或替换实现）。
- `smoltcp`（TCP/IP，Rust，一档，0BSD）：已以 submodule 引入 `network/netstack` 骨架；设备 adapter / TCP / UDP 操作仍为 `todo!()`，前置 `NetDevice` 契约仍不存在。见 `docs/modules/netstack.md`。
- `lwIP`（TCP/IP，C，二档，BSD-3-Clause）：前置是 `NetDevice` 加 Thread/Sync。
- `Mbed TLS`（二档，Apache-2.0）：前置是 RNG、Clock、Socket。
- `TinyUSB`（一档偏二档，MIT）：前置是同步原语。
- `WAMR`（WASI/Wasm runtime，二档偏三档，Apache-2.0）：前置是 File 与 Namespace。Wasm 是组件的一种执行后端，与执行域正交，不是第四个执行域。
- `picolibc`（C libc，一档，BSD-3-Clause）：要写 `_write`/`sbrk`/`_exit` host-glue 加交叉编译配方。
- `virtio-drivers 0.13.0` 已经在用，落在 `virtio_blk` 组件内部，Hal 适配器私有，不 fork 上游。

统一 host 接口是这条线的关键设计，状态要分清：Block 已落地；Files/Namespace 是设计；Net、RNG、Clock、Log、Thread、Sync 还是提案（`porting.md` §8）。不要把这些提案当已实现能力用。

打包链已经落地：`.kcomp` 加 cpio `.initpkg` 加文本 manifest，就是 Linux insmod/initramfs 模式的简化版。运行期依赖解析（depmod 模式）与 Runtime Graph 后置。

## 9. POSIX 边界

当前已有最小 RV64 personality 的用户执行、fork/exec/wait 与 console；完整 POSIX 仍未实现。
下一步用 VFS 文件 I/O 验证 Task/AddressSpace、等待、文件对象与进程语义边界，详见 §3.26、§6。

模型是 POSIX Process → `AddressSpaceId` + N × (POSIX Thread → `TaskId`)。PCB、TCB、fd、cwd、session、credentials、signal handler 与 mask、pending signal、fork、exec、waitpid、mmap、brk 全部留在 personality。Core 不得变成 POSIX 内核。顺序是先做通用 block/wake/event/notification/wait/cancel，不要提前造 `core::signal`。

## 10. Core 冻结判据（A 到 H）

freeze 的判据不是数功能，而是多个差异很大的上层负载能只用现有 Core 原语构造出来，并且不再持续索要新的基础对象类型。逐条对当前仓库：

| # | 场景 | 现状 | 依据 / 缺口 |
|---|---|---|---|
| A | Driver：真设备 → claim → MMIO/IRQ/DMA → driver component → Endpoint，最好在真 SoC 上 | PARTIAL | 机制齐全，`virtio_blk` 加 `driver_prober` 在 QEMU 跑通；无真 SoC；IRQ 单线；无 PCIe/USB/NVMe |
| B | 多实例：一个 artifact 到实例 A/B，各自 state/resources/endpoints/tasks，无串扰 | PARTIAL | 已证：host `same_artifact_loads_produce_independent_components`、CoreTest `driver-multi-device`、ArchTest `isolated-restart`（并发同 artifact）、`ram_blk_rw` 每实例 buffer；endpoint/task 全维度无串扰与卸载后语义未系统证明 |
| C | 服务组合：BlockDevice → Filesystem → 更高消费者，Core 不理解 FS 语义 | PARTIAL | `fatfs`/`littlefs` 已绑 `block.device`，CoreTest `block-chain`/`littlefs-multi-instance`/`littlefs-isolation`；只读混合VFS/namespace/OpenFile已接，通用POSIX fd与两级缓存未有，通信收敛见§3.29 |
| D | Task 运行时：Runnable→Running→Blocked→(wake)Runnable→Exited 加 timer/preemption | NOT-SATISFIED | TaskTable permit、host 调度集成与 RV64 远端 wake 已通过；RR 三常驻任务的下标饥饿已修复；preemption（`on_timer_tick` 是 `todo!()` 且未接线）仍缺失 |
| E | POSIX 原型：process semantic state → AddressSpace → 多个 Core Task，PCB/fd/signal 留在 personality | PARTIAL | RV64 普通用户 task/AS/trap、fork/exec/wait 与 console 已接；通用 VFS fd、线程/signal 与文件完成协议仍缺 |
| F | 执行域：同一 service contract 至少在 KernelNative + IsolatedNative 上验证，Sandbox 后加 | PARTIAL | 同一 `kcomp_domain_service.kcomp` 的真实 SDK `block.device` provider/consumer 覆盖 K/K、K/I、I/K、I/I，组件自行发布；嵌套/故障/stale/重入已验证。Sandbox 与硬件设备/任务能力尚缺 |
| G | 真机：至少一块 QEMU RISC-V virt 之外的真 Linux-class RISC-V 板 | NOT-SATISFIED | 零真机代码与配置 |
| H | 不同机器类别：RV64 Linux-class 加 RV32 NoMMU embedded/MCU 共用同一套小 Core | NOT-SATISFIED | RV32 S-mode/NoMMU 私有 profile 已通过 CoreTest；M-mode 启动失败，无 MCU 真机 |

结论：A/B/C/E/F 是 PARTIAL，D/G/H 未满足。Core 还没到 freeze-candidate。
下一步缺口是组合/文件对象/完成语义、组件私有域任务，以及真机与异构机器验证；
RV64 普通用户 task 与 AS 已关联，不能继续当作零实现缺口。

## 11. 明确未做

- SandboxedNative（U-mode 加私有 AS 加 `ecall`）：Core `component/sandbox.rs` 骨架，组件 create `-ENOTSUP`；组件 import / Core ecall / destroy 未实现；私有 allocator / runtime 选择已有测试，尚未接到 Sandbox 执行路径。普通用户 task 的 U-mode / trap 单独已接通。NOT IMPLEMENTED / PLANNED。
- 抢占：`sched::on_timer_tick` 是 `todo!()`，`timer::on_trap` 不调用它。NOT IMPLEMENTED。
- block/wake：任务 permit、owner、最终检查与 commit 已落地；CoreTest 覆盖本地与跨 CPU 唤醒。独立 TaskBlock / TaskWake trace、event / waitqueue、join / stop 未做。
- SMP 基础已落地（RV64）：AP 启动、BootGate、Online、IPI、per-CPU scheduler / timer / containment、固定 CPU 的真实组件调度、跨 CPU wake 与 panic containment。复杂 SMP 调度（迁移 / work stealing / 抢占 / hotplug）与私有 AS 任务调度未做。SMP 不是构建开关，无 `smp` Cargo feature。
- 本阶段明确不做（原路线图的"明确不做"清单并入本文，与 `AGENTS.md` 一致）：真正动态加载、运行期组件热插拔 / Runtime Graph / 依赖解析器、热迁移、复杂 IPC、微内核模式、Wasm runtime、WIT/IDL、完整 capability 系统、完整 POSIX、Linux syscall 兼容、复杂 VFS、复杂 SMP 调度、形式化证明、完整 driver framework、完整依赖解析器。NOT IMPLEMENTED / PLANNED。
- 尚未完成方向（方向，不是承诺的里程碑，没有排期）：多 profile（`game` / `unix`(POSIX personality) / `micro` / `debug`）与 UserAddressSpace 执行域；Wasm 执行后端（`scheduler.wasm` 等，是组件的一种执行方式，与执行域正交，Core/Arch 保持 native Rust）；热替换（在 drain 协调协议之后向无感替换演进：quiesce → stop → unbind → reset → replace → bind → start；不做 live state migration）；内存物理回收（完整 buddy、通用 Core heap、完整 panic recovery；phase 1 只做资源归属撤销与 quarantine，不承诺共享堆字节回收，也不承诺对抗隔离）；验证工具链（Kani / Loom / Miri / Verus 与 Test Scheduler / Hunt Mode）；第三方库调包（见第 8 节）。
- 内存物理回收与 instance 退役回收：不承诺，逻辑死亡、物理驻留。NOT IMPLEMENTED。
- NoMMU：RV32 S-mode私有profile已实际boot；本轮IPC/hybrid分组通过但整套driver失败，默认CI仍未纳入。M-mode不由此推导通过。
- M-mode（`PRIVILEGE_MACHINE`）启动：Kconfig 可选、代码可编译，但没有 defconfig、没有 boot harness、不在任何测试或 CI 里构建。PLANNED / 未验证。
- AArch64 / x86_64 / LoongArch：`os/arch/src/<isa>` 与 `os/boot/<isa>` 同形骨架已落地（`encoding`/`elf`/`console`/`cpu`/`smp`/`trap`/`context`/`mmu`，实现体 `todo!()`），能编译、未启动、未验证。NOT IMPLEMENTED（骨架）。
- 真机支持（VisionFive 2、ESP32-C3 等）：零代码与配置，纯路线图。PLANNED。
- `embedded-hal` / `embedded-io` / `embedded-storage` / `embedded-nal` / `embedded-graphics`：仓库里完全不存在。PLANNED。

## 12. 与文档的已发现漂移

历史登记保留；本轮通信漂移已同步，清单与当前证据见专项审计§9。

- `docs/architecture/deployment.md` §7.4/§11 的消费路径 ABI 陈旧描述已修正：裸 lookup 只发现 id，SDK 随后 validate，bind 再核对 exact ABI 与存活。
- `docs/modules/components.md` 漏登记两个已在 `KCOMP_SRCS` 里的 fixture：`tests/kcomp_isolated_direct`、`tests/kcomp_isolated_unsupported`，全 `docs/` 没有引用。
- `README.md` 目录说明列了 `components/drivers/ uart/ virtio_blk/ …`，但 `uart/` 不存在，`driver-model.md` §4 明说 uart 未实现；README 的 monitor 命令列表漏了 `unload` 和 `trace`，`docs/modules/core/monitor.md` 有。
- 执行域注释已同步 K/I 双向 Gate；另有历史注释待处理：`os/core/src/memory/address_space.rs:2030` 的"对应 roadmap 的 NoMMU 验收点"失去所指（路线图文档已删除，NoMMU 现状见 3.23 与第 7 节）。
- 测试计数以 §3.21 的当前 KTAP 计划为准，2026-09-27 的 `41/41` 和旧 bitmap 计数是历史快照。
