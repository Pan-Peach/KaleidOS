# KaleidOS 状态与计划

快照：2026-09-28（UTC）。分支 `develop`，`git rev-parse HEAD` = `0d189e1029d6bf62eb48a76ff32decbb73f7d388`。

工作树干净。本次快照纳入 **SMP 骨架 + 多 ISA 骨架 + HAL 接口收敛**（`feat(smp)`，见 `docs/modules/arch.md`）：接口已立，`Smp` / 新 ISA 的实现体仍是 `todo!()`；RISC-V 单核行为不变，全部测试保持绿。上一快照提到的并发卫生改动（inspector 移除、dma 测试 flake 等）已提交（`bb215b5`），不再有"工作树与 HEAD 冲突"的情况。

本文是审计快照加计划，不是设计文档。事实以代码、测试、构建配置为准。README 和设计文档里写了但代码没有的，按未实现记。

本文同时是状态与路线图的唯一入口：原 `docs/development/` 下的路线图文档已并入本文并删除（依赖链见第 6 节，硬件见第 7 节，生态与调包见第 8 节，明确未做见第 11 节）。

## 0. 一句话

KaleidOS 是一台能在 QEMU 启动、能交互观察、能加载 `.kcomp` 组件的 RISC-V 单核内核。Core 的基本词汇（`TaskId` / `PhysicalRange` / `ComponentId` / `DeviceId` / `EndpointId` / `ExecutionDomain`）已经立住，`propose → validate → commit` 路径可用。现在还没有多个差异足够大的上层负载来把 Core 逼到定型，所以离 freeze-candidate 还有距离。最大的洞是 block/wake 语义和 task 与地址空间的关系；它们决定未来 POSIX personality、驱动、服务会不会继续向 Core 要新的对象类型。

底座侧有新进展：`arch` 的 CPU 身份（逻辑 `CpuId` / 硬件 `HardwareCpuId`）、中断回调（`LocalInterruptHandler = fn(CpuId)`）与中断控制器（关联 `Config` / `Claim`）契约已收敛成可多核、可多 ISA 的稳定面，并落下 SMP 与 x86_64 / aarch64 / loongarch64 的**骨架**——接口已定，实现体待手写。详见 3.12、3.24 与 `docs/modules/arch.md`。

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
Applications / System Personality        未实现（POSIX/Win32/WASI 都是未来）
        │
Services / Devices（组件图组合的产物）     最小 FS 服务已有（fatfs/littlefs）；无 VFS/namespace
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

缺口：没有 user AS，没有 COW/lazy，没有 mmap/brk（这些属于 personality）；NoMMU 的 protection（PMP/MPU）没落地；`adopt`（接管 boot root）有代码但没接线。

下一步：user AS 留给 SandboxedNative；PMP/MPU 跟 NoMMU 启动一起做；`adopt` 等 boot `vm/runtime.rs` 重构时接入。

#### 3.3 任务对象 `▰▰▰▰▱` IMPLEMENTED

现状：`TaskId`、`TaskTable`、owner、kernel stack、状态机（`Created`/`Runnable`/`Running(CpuId)`/`Blocked`/`Exited`，合法边 6 条）。host 覆盖非法迁移、owner、`EntryOutOfImage`、property `random_transition_sequence_preserves_task_truth`；QEMU 有 `task-create`/`task-switch`/`task-exit`，ArchTest 有 `task-panic`。

缺口：`TaskRecord` 与 `AddressSpaceId` 没有绑定，`Running(CpuId)` 没有 AS 概念。

下一步：完成通用 park/unpark（见 3.4），再做 task 与 AS 的绑定语义。

#### 3.4 park/unpark 原语 `▱▱▱▱▱` IMPLEMENTATION IN PROGRESS

现状：ABI、每任务 pending permit、owner 校验与 `Blocked→Runnable` 已实现；host `proptest!` 验证 permit 状态模型，CoreTest 新增组件侧提前 unpark 与 64 轮 park/unpark 调度；host 集成测试当前在“schedule_next 前任务表锁已释放”的断言失败。每任务最多一个 pending permit；Core 不定义 EventId / waitqueue，等待队列和条件由组件持有。

缺口：当前 host 调度集成测试发现 `park_current` 持有 TaskTable 锁调用 `schedule_next`，触发 fail-fast 不变量；外层 irq-save guard 也不能跨 context switch。RV64 CoreTest 已构建并启动，报告到 `park-early-unpark-accepted` 后没有完成后续阻塞/唤醒场景，runner 判失败。`TaskBlock`/`TaskWake` trace 事件尚未定义（`os/core/src/trace/event.rs`）。

下一步：把 permit 检查和 Blocked 提交纳入调度临界区、释放 TaskTable 锁后再调度，并确保 context switch 前 IRQ 恢复；随后运行 `cargo test -p kernel --lib park -- --test-threads=1` 与 `make test-qemu`。

#### 3.5 调度机制 `▰▰▰▰▱` IMPLEMENTED（协作式）

现状：每 CPU `CpuState`，`PolicySlot` 存选中的 `EndpointId` 与 Core 栈，`run`/`yield_current`/`exit_current` 走 `propose → validate → commit`。host 22 项测试覆盖非法提议拒绝、策略失败回退、IRQ 与 service-call 门禁、trace；QEMU 有 `scheduler-load`/`scheduler-select`/`scheduler-rr`；`kbench` 量 `sched.yield_roundtrip`。

缺口：没有抢占（`on_timer_tick` 是 `todo!()`）；SMP 只有骨架（`CONFIG_SMP` 与 `os/core/src/smp/` 已落地，per-CPU 调度状态、AP 启动、IPI 投递、`sscratch` 入口记录是 `todo!()`，未接线）；没有 priority/CFS，这属于策略组件。

下一步：接抢占，见 3.6；实现 SMP（per-CPU 状态 + AP 启动 + IPI，先 RISC-V）。priority/CFS 留在组件。

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

现状：`IrqTable` 按 device 锚定 route，存 owner/handler/ctx；`register`/`enable`/`disable`/`release`/`revoke_owner`；投递走 trap→route→锁外 callback，回调带 IRQ 归属作用域。QEMU ArchTest `external-irq` 证明 PLIC 恰好投递一次 UART THRE 线；host 覆盖 route、owner 与 revoke。

缺口：单线模型（一台设备一条 IRQ）；polled、计数、掩蔽、ack 已删并推迟；回调内 panic 会致命；只在 QEMU/PLIC 验证。

下一步：共享线与 MSI-X 跟 PCIe 一起；ack/mask 等真机需求出现再补；回调 panic 并入 containment 的后续工作。

#### 3.10 DMA `▰▰▰▱▱` EXPERIMENTAL

现状：allocation（与设备无关）和 mapping（与设备相关）分离，`alloc`/`free`/`map`/`unmap` 齐备；失败 backing 进 Core 私有 `QUARANTINE`，不归还 buddy；mapping id 单调不复用。host 覆盖单调、quarantine、revoke；QEMU 有 `dma-ring`、`dma-invalid-size`，virtio_blk 真实走 `dma_alloc` 加 `map`。

缺口：没有 IOMMU，设备地址是 identity，DMA 隔离只有偶然故障隔离；CPU 隔离不等于 DMA 隔离；没有 bounce buffer 或多 pool；`free` 后 backing 不归还，这是 correctness 决定，不是安全结论。

下一步：IOMMU 与 bounce/pool 等真机或安全需求出现再做；回收前提是设备静默。

### Arch 与启动

#### 3.11 Boot / 机器发现 `▰▰▰▰▱` IMPLEMENTED

现状：FDT → `MachineInfo` → `core::init` → monitor 全链打通。FDT 解析用 third_party 的 `fdt` crate。RV64 用 identity 加高半区双映射（Sv39），RV32 用 identity（Sv32）。`make test-qemu` 在 RV64/RV32 双 profile 有 boot smoke。

缺口：只认 FDT 和 QEMU virt；没有真机、板级 quirk、ACPI；M-mode 没有启动路径（无 defconfig，未构建）；NoMMU 启动没有任何 runner 跑过。

下一步：VisionFive 2 的 bring-up 从这里开始，串口出 `core>` 是第一步验收。板级差异集中在 boot，不进 Core。

#### 3.12 Arch 层 `▰▰▰▱▱` EXPERIMENTAL（层是 ACTIVE，不是 stable）

现状：backend trait（`CpuArch`/`Timer`/`InterruptController`/`Smp`/`Console`/`SystemReset`）加 `riscv`/`fake`/`nommu` 实现；多个 ISA 骨架；`CpuId`（逻辑）与 `HardwareCpuId`（硬件）分离（定义在 arch，Core re-export）；中断回调统一为 `LocalInterruptHandler = fn(CpuId)`；`InterruptController` 已原地改为 `Config`/`Claim` + `init_cpu`；`ComponentRelocationImpl` 按 ISA 选择。RISC-V backend 验证充分：host 直驱生产实现测编解码与 walk、`RiscvRelocator`；QEMU ArchTest 覆盖 trap、页表权限、context switch、timer、PLIC。

缺口：跨架构抽象**已有骨架但未被第二个 ISA 验证**——`os/arch/src/{x86_64,aarch64,loongarch64}` 与 `os/boot/<isa>` 已建（同形，实现体 `todo!()`），能编译、未启动、未验证；没有真机；机器差异欠验证。SMP 接口已收敛（`Smp` trait / `LocalInterruptHandler` / `InterruptController` 的 `Config`+`Claim` / `CpuArch::init_cpu`+`enable_irq`），但实现与 CPU-local 存储（`sscratch` 入口记录、per-CPU trap 栈）都是 `todo!()`，属协调 trap bring-up 的工作。RISC-V bring-up 进展可观，但层本身还在动。

下一步：实现 SMP（per-CPU 状态 + AP 启动 + IPI，先 RISC-V）；按 x86_64 → aarch64 → loongarch64 顺序做第二 ISA 的真实 bring-up。新 ISA 的 ArchTest 入口已就绪但 opt-in（`make test-arch-{x86_64,aarch64,loongarch64}`，boot 未实现前会失败）；`.kcomp` 组件目前仍是 RISC-V 重定位专用，新 ISA 先用 Core-only 镜像。M-mode 补 boot harness 之后才有意义。

### 组件与执行域

#### 3.13 组件工件与 loader `▰▰▰▰▱` IMPLEMENTED

现状：`.kcomp`（ELF32/64 ET_REL）经 cpio `.initpkg` 内嵌，store 解析，loader 放段加重定位加入口校验，registry 生命周期。C 和 Rust 两个语言前端都能编。host 覆盖 ELF 解析、重定位、未导出符号拒绝、`same_artifact_loads_produce_independent_components`；QEMU RV64/RV32 机器级跑 `load`/`unload kcomp_c_smoke`。

缺口：每次 instantiate 重新放段与重定位，没有 text 去重；没有运行期依赖解析；manifest 只有名字列表，没有能力或支持范围字段。

下一步：manifest 加能力字段。依赖解析按 depmod 模式后置。运行期热插拔与 Runtime Graph 明确不做，除非出现需求。

#### 3.14 组件实例与生命周期 `▰▰▰▱▱` EXPERIMENTAL（模型是 ACTIVE DESIGN）

现状：一个 `ComponentId` 等于一个完整组件，`ComponentRecord` 直接持有 `loaded`；状态机是 `Declared→Resolved→Starting→Ready→Stopping→Stopped/Failed`。host 覆盖生命周期、停止与销毁、失败撤销、endpoint 永久失效；QEMU CoreTest `component-lifecycle`，ArchTest `isolated-lifecycle*` 与 `isolated-restart`。

"一个 `.kcomp` 到多个独立实例"是真实支持，不只是 ID 类型上可表达：host 断言两次 instantiate 的 backing 独立且镜像区间不重叠，CoreTest `driver-multi-device`，ArchTest `isolated-restart` 接受同 artifact 的并发第二实例（各自 AS、backing、窗口、slot），`ram_blk_rw` 每实例独立 buffer。

缺口：unload 是 tombstone，不回收 backing；没有 drain 变体；没有意外退出的独立终态；endpoint、task、crosstalk、卸载后语义的完整矩阵没有逐个证明；组件自有任务入口 `kcomp_task` 不存在。

下一步：先补 drain 协调协议（停止新工作、排空 IRQ 与回调、确认零活跃执行），它是物理回收的前提；再补 `kcomp_task` 与卸载语义矩阵。

#### 3.15 Endpoint / Contract / Binding `▰▰▰▱▱` EXPERIMENTAL

现状：`ContractId` 加 exact ABI fingerprint 加 opaque `EndpointId`，绝不重定向；staged publish/lookup/discover/bind；bind 是 Core 选定机制的唯一选择点。host 覆盖 staged publication、abort 原子性、no-redirect、domain 矩阵、Gate/Direct 选择；CoreTest 有 scheduler/filesystem/driver 链；ArchTest 有 `isolated-service*`。

ABI 校验的落点要说清楚。`kcore_endpoint_lookup` 的发现路径不带 abi，只比 contract 与存活，交付 opaque capability。exact contract 加 abi 加存活的校验在 `kcore_endpoint_validate`（对已持有的 id，C ABI 可达，SDK `Endpoint<C>::from_id` 已接）和 `kcore_endpoint_bind`（第一步就走同一个 `EndpointRegistry::lookup`）。`deployment.md` §7.4/§11 仍写"consumer exact ABI 未做"，和 HEAD 代码冲突，见第 12 节。

缺口：跨域只有 KernelNative→IsolatedNative Gate 真正派发；出站 Isolated 与 Sandbox 组合显式拒绝。

下一步：把 `deployment.md` 的措辞收敛到代码，这不是代码缺口。新接口（NetDevice、Clock、RNG 等）都走同一条 bind 路径。

#### 3.16 执行域 `▰▰▰▱▱` EXPERIMENTAL

现状：`ExecutionDomain` 三个变体。KernelNative 完整并验证。IsolatedNative（S 加私有 AS）有真实的 create/destroy/service：私有 AS、共享 Core 映射（same VA→PA）、最小跨 AS trampoline（per-invocation context、satp 切换、全量 `sfence.vma`）、按域放段（页级权限）、Core 预置窗口、K→I service Gate、失败与重启清理。按验证层看是 PARTIALLY VALIDATED，能力止于 QEMU RV64+RV32 的 27 个 ArchTest case，不能再往上抬。

缺口：无 ASID（恒 0 加全量 flush）；无 U-mode/`ecall`，是协作式 S-mode 边界，不是对抗隔离；无出站 Isolated 调用；import 面只有诊断、只读查询加 `kcore_panic_escape`；不能 claim 设备、注册 IRQ/DMA、建任务、发布 endpoint；没有 I→I transport；没有压力或对抗测试。

下一步：两条路选一条。继续补 ASID、U-mode、出站调用、更宽 import 面；或者明确冻结成教学实验，把资源投到 SandboxedNative 与真机。

#### 3.17 SandboxedNative（U-mode + syscall 边界） `▱▱▱▱▱` NOT IMPLEMENTED

现状：`todo!()` 占位（`load.rs:183`、`exit.rs:129`），所有组合显式拒绝；没有 syscall wire ABI。

缺口：低特权执行、`ecall` 入口、syscall 编解码、页表强制的访问边界，全部没有。

下一步：等 IsolatedNative 定位定下来再做。它是"部署形态即安全策略"里真正的硬件强制边界。

#### 3.18 驱动组件 `▰▰▰▱▱` EXPERIMENTAL

现状：`drivers/virtio_blk` 是 VirtIO-MMIO 块驱动，发布 `block.device` 与 `probe.result`，内部用第三方 crate `virtio-drivers 0.13.0`，Hal 适配器是组件私有（`hal.rs`），不 fork 上游；`driver_prober` 做协议无关总线角色；测试 fixture 有 `ram_blk`/`ram_blk_rw`。QEMU RV64/RV32 CoreTest 覆盖 `driver-candidates`、`driver-prober-load`、`driver-prober-dispatch`、`driver-attach`、`driver-no-match`、`driver-multi-device`，含 `no-block` 场景。

缺口：只有 virtio，还是 QEMU virt 的 MMIO 变体；没有 PCIe/USB/NVMe/网卡；没有 UART driver component（`driver-model.md` 明说未实现）；没有真机；没有热插拔或驱动更换事务。

下一步：网络组件优先考虑 smoltcp（0BSD）或 lwIP，前置是 `NetDevice` 契约。USB 走 TinyUSB，前提是同步原语。TLS 走 Mbed TLS，前提是 RNG/Clock/Socket。真机 bring-up 时补 SD/eMMC 与以太网，顺序见第 7 节。

#### 3.19 文件系统服务 `▰▰▰▰▱` IMPLEMENTED

现状：`fatfs`（只读 FAT）与 `littlefs`（v2.9.3，mount 内 format 加自检）两个 C `.kcomp`，都绑 `block.device`；对外只有只读 filesystem 契约（mount/unmount/open/close/read，不透明 handle）。CoreTest `block-chain`、`block-chain-direct`、`littlefs-multi-instance`、`littlefs-isolation`、`littlefs-direct` 在 RV64/RV32 端到端验证，多实例存储互不影响。

缺口：只读；没有 VFS、namespace、File service；没有两级缓存；`lwext4` 还是候选，GPLv2 许可策略要先定。

下一步：写支持；VFS/namespace 按 `docs/interfaces/filesystem.md` 的设计走；`lwext4` 许可先行；C 库用 picolibc 时补 `_write`/`sbrk`/`_exit` host-glue。

### SDK、测试与观测

#### 3.20 SDK / C ABI / Rust ABI `▰▰▰▰▱` IMPLEMENTED

现状：`abi/*.toml` 是单一来源，生成 C 头与 Rust 镜像；42 个 `kcore_*` 导出；组件 ABI 是窄 C ABI，Rust ABI 永远是私有实现；SDK 私有携带 C runtime。`make abi-check` 重生成后逐文件比对，`kcomp_abi_drift.rs` 冻结入口面与绝对数值；QEMU CoreTest `c-frontend` 加机器级 `load kcomp_c_smoke`。

缺口：没有 ABI 版本兼容，靠 exact fingerprint 加原地替换；组件外链只允许 `kcore_*`；SDK 刻意不朝 libc 或共享 runtime 扩张。

下一步：为 embedded 系列提供 `kcomp-embedded-*` 伴生 crate 或 SDK feature；把 `porting.md` §8 的 host 接口（Net/RNG/Clock/Log/Thread/Sync）逐个成文并落到 SDK。

#### 3.21 测试体系 `▰▰▰▰▱` IMPLEMENTED

现状：host 单测含 proptest；CoreTest 是板内集成，49 项检查（bit 0 到 49，bit 23 未用）；ArchTest 41 个 case，每 case 独立 QEMU，其中 27 个 `isolated-*`。入口是 `make check/test/test-host/test-qemu/test-arch`；另有 opt-in 的 `test-arch-smp-rv64` 与 `test-arch-{x86_64,aarch64,loongarch64}` / `test-arch-new`（SMP / 新 ISA 未实现前会失败，刻意不进默认聚合与 CI）；CI 三个 job（check/qemu/archtest）。

缺口：NoMMU 不在任何测试入口或 CI 里构建与启动，只有 Kconfig 解析用例；M-mode 无验证；runner 只显式断言 12 条，其余靠 `all: PASS`；host ring 是线程本地替身，不覆盖并发语义。

下一步：NoMMU 进 `_test-build` 与 CI；Isolated 补压力与失败注入；并发用 Loom，形式化用 Kani/Miri/Verus，另有 Test Scheduler / Hunt Mode，这些是登记的方向，不是排期。

#### 3.22 观测 / monitor / trace `▰▰▰▰▱` IMPLEMENTED

现状：结构化 trace ring（seq 单调、固定容量、无分配），只读 Inspector，`core>` monitor（行编辑、历史、Tab，help/machine/memory/tasks/load/unload/components/catalog/trace/shutdown/reboot）。QEMU runner 依赖 `core>` 与 load/unload；CoreTest 用 trace 断言操作到事件；ArchTest 断言精确 scause。

缺口：`TaskBlock`/`TaskWake`/`Fault` 事件刻意未定义；host ring 不覆盖生产锁；没有动态订阅或落盘。

下一步：block/wake 落地时加对应事件；Fault 事件等 Core 接管 fault 记录再加。

### 平台与生态

#### 3.23 平台 profile 与真机 `▰▱▱▱▱` PLANNED（按最弱一环取）

现状：仓库零真机支持。RV32 NoMMU 有 backend 代码和 defconfig（`nommu.rs`、`entry32-nommu.S`、`qemu_rv32_nommu_defconfig`），但当前没有任何测试入口或 CI 构建、启动它，所以不给 EXPERIMENTAL。M-mode 代码可编译（Kconfig 自述 compile-verified only），没有 defconfig 和 boot harness，不在任何测试入口。

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

#### 3.26 POSIX personality `▰▱▱▱▱` PLANNED

现状：没有。连前置条件（task 与 AS 绑定、block/wake）都不满足。

缺口：PCB、fd、signal、fork/exec/waitpid、mmap/brk 全部没有，而且都应该留在 personality。

下一步：先做最小原型验证边界，模型是 Process = AddressSpaceId + N 个 Task。Core 不做成 POSIX 内核。详见第 9 节。

## 4. 结构热点（按对 Core 冻结的威胁排序）

1. block/wake 尚未通过集成验证，最高优先。TaskTable permit / owner 路径已实现；调度测试当前发现 park 持表锁进入 schedule_next，且 irq-save guard 跨 context switch。组件持有条件和等待者列表，Core 只提供按任务 park/unpark 机制。
2. task 与地址空间没有绑定。`TaskRecord` 和 `AddressSpaceId` 之间没有关系，`Running(CpuId)` 没有 AS 概念。将来一个 POSIX 进程等于一个 AS 加 N 个 task，这个语义必须由 Core 先提供，否则进程语义会漏进 Core。
3. 抢占没接线。`timer::on_trap` 不调用 `sched::on_timer_tick`，后者是 `todo!()`。纯协作模型下，不协作或死循环的执行域拿不回控制权。
4. 组件多实例与生命周期的完整矩阵。多实例在加载、状态、设备、FS 维度已经证明，但 endpoint、task、crosstalk、卸载后的语义没有逐个证明。unload 是 tombstone，backing 驻留到 reboot，没有 drain。consumer 缓存的裸 function table 在 provider `Stopped`/`Failed` 后仍能调用成功，这是物理驻留的直接后果，不是 bug，但 teardown 正确性不能建立在它上面。
5. IsolatedNative 的缺口。机制真实，覆盖很窄，详见 3.16。
6. 真机验证缺失。设备、中断、DMA、timer 都只在 QEMU virt 证明过。
7. 组件模型仍在演进。`ExecutionDomain` 与生命周期接口是 ACTIVE DESIGN，SandboxedNative 参与的所有组合都被 `todo!()` 或显式拒绝。

## 5. 验证矩阵（哪些组合真的跑过）

| 组合 | 跑过没有 | 入口 / 证据 |
|---|---|---|
| Host 单测（Core truth / parser / property） | 是 | `make test-host`（`cargo test --workspace` 加 SDK/prober/kbench/ram_blk）；`kcomp_abi_drift.rs` |
| CoreTest / QEMU RV64 / MMU / `default`+`no-block` | 是 | `make test-qemu` → `tests/qemu/runner.py` |
| CoreTest / QEMU RV32 / MMU / `default`+`no-block` | 是 | 同上（`qemu_rv32_defconfig`） |
| ArchTest / QEMU RV64 / MMU（41 case） | 是 | `make test-arch` → `tests/qemu/arch_runner.py` |
| ArchTest / QEMU RV32 / MMU（41 case） | 是 | 同上 |
| ArchTest / 新 ISA 骨架（opt-in） | 否 | `make test-arch-{x86_64,aarch64,loongarch64}` / `test-arch-new`：镜像可构建（Core-only），boot 为 `todo!()`，用例现在会失败 |
| ArchTest / SMP（opt-in） | 否 | `make test-arch-smp-rv64`：`smp-*` 用例为 `todo!()`，现在会失败 |
| KernelNative（执行域） | 是 | CoreTest 全链加 ArchTest `panic-*` |
| IsolatedNative（S 加私有 AS） | 是，仅 QEMU | ArchTest `isolated-*`（27 case，RV64+RV32） |
| SandboxedNative（U-mode） | 否 | `todo!()`（`load.rs:183`、`exit.rs:129`），全部组合显式拒绝 |
| RV32 / NoMMU | 仅配置解析 | `configs/qemu_rv32_nommu_defconfig` 只被 `tests/kconfig/test_glue.py` 解析；`make check` 的 `_test-build` 与所有 runner/CI 都不构建、不启动它 |
| RV32 / M-mode（`PRIVILEGE_MACHINE`） | 否 | 仅可编译；无 defconfig；无 boot harness |
| 真机（任何板卡） | 否 | 仓库没有任何真机代码或配置；`os/`、`configs/` 无 VisionFive/JH7110/ESP32 之类 |
| CI | 是 | `.github/workflows/ci.yml` 三个 job：`check` / `qemu` / `archtest`；只覆盖 RV64+RV32 MMU。SMP / 新 ISA 的 opt-in 目标不在 CI（未实现前会红） |

提醒：`make test` = `test-host + test-qemu + test-arch`；`make check` = fmt + clippy + `_test-kconfig` + `abi-check` + `test-host` + 交叉构建。两者都不含 NoMMU 启动，不含真机。

本次复核抽样：在 `0d189e1` 上复跑 `make check`、`make test-qemu`、`make test-arch`，均通过；`tests/qemu/logs/` 留有 2026-09-28 的 RV64/RV32 `default`+`no-block` 与 ArchTest（RV64/RV32 各 41/41）日志。`python3 tests/kconfig/test_glue.py` 复跑为 11/11 PASS。

## 6. 近期依赖链

原路线图文档的依赖链并入本节（文件已删除）。已完成的地基：

```text
P0 地基（启动地址去硬编码、Sv39/Sv32 启动）                 —— 已完成
P1 任务系统（context switch、调度执行链）                    —— 已完成
P2 中断/驱动（timer 抢占 C5、设备·IRQ·DMA C6、第一个 driver）—— 部分：driver 已落地，抢占未接线
P3 组件化进阶（区域分配 C7、域视图交付 C8、任务化组件 C9）    —— 部分：加载与生命周期已落地，只缺 `kcomp_task` 等
P4 执行域/隔离（C10）                                        —— 部分：受限 IsolatedNative 已落地，ASID / U-mode / ecall / 出站 / SandboxedNative 未完成
```

当前未完成项按依赖顺序排，前一项不成立，后一项无从谈起。

1. Timer 与抢占接线：让 `timer::on_trap` 真正到达调度。先决定用延迟重调度标志还是 trap 内直接切换，并正面回答 `sstatus.SIE` 的保存恢复。
2. Task block/wake：修复当前失败的调度集成测试，验证 permit 快速路径、阻塞后唤醒及 IRQ 恢复，再决定 trace 事件。等待条件与等待者列表留在组件。
3. Task 与 AddressSpace：定义绑定语义，为将来的 POSIX "Process = AS + N Task" 铺路。
4. 组件生命周期收敛：drain 协调协议，`kcomp_task`，卸载与重载的语义矩阵。
5. IsolatedNative 收敛或冻结：补 ASID、U-mode、出站调用、更宽 import 面，或者明确冻结成教学实验。
6. 消费路径 ABI 的文档收敛：代码侧没有缺口，`validate` 与 `bind` 都做 exact contract 加 abi 加存活，SDK 已接。要做的是把 `deployment.md` §7.4/§11 的"consumer exact ABI 未做"改到和代码一致，或明确声明 lookup 永远 contract-only。
7. SMP 与第二 ISA：`arch` 接口已收敛（`Smp` trait；逻辑 `CpuId` / 硬件 `HardwareCpuId`；回调 `fn(CpuId)`；`InterruptController` 的 `Config`/`Claim`；per-ISA 重定位；`CONFIG_SMP` 与 per-CPU seams），实现与 CPU-local 存储（RISC-V `sscratch` 入口记录、per-CPU trap 栈、containment 本地状态）待做，属与 trap bring-up 协调的工作。见 3.12 / 3.24 与 `docs/modules/arch.md`。

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
| 存储 / 文件系统 | 部分 IMPLEMENTED（只读）：`fatfs`（只读 FAT）与 `littlefs`（v2.9.3）两个 C `.kcomp`，只读契约，无 VFS / namespace / 写支持 | C 路线：lwext4（ext2/3/4，许可待定）；Rust 路线：Hadris（MIT，FAT/exFAT）、ext4-view（MIT/Apache，只读 ext4）。写支持与 VFS 自研（`docs/interfaces/filesystem.md`） | lwext4 的 GPLv2 许可策略先定；FS 契约定稿；块设备路径已就绪 |
| 网络（TCP/IP） | NOT IMPLEMENTED：没有 `NetDevice` 契约、没有网卡驱动、没有协议栈 | smoltcp（0BSD，首选）；lwIP（Modified BSD，后备） | `NetDevice` 契约 + 网卡驱动（virtio-net 可先用 virtio-drivers）；lwIP 还需 Thread/Sync |
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
| smoltcp | github.com/smoltcp-rs/smoltcp | Rust | 0BSD | ✅ no_std、无堆 | High（v0.13.1） | TCP/UDP/ICMP/raw、IPv4/IPv6、DHCP、DNS、ARP、802.15.4；`phy::Device` trait 就是 MAC 驱动缝；**首选**，已在 RISC-V 目标编译 |
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
- `smoltcp`（TCP/IP，Rust，一档，0BSD）：前置是 `NetDevice` 契约，它今天还不存在。
- `lwIP`（TCP/IP，C，二档，BSD-3-Clause）：前置是 `NetDevice` 加 Thread/Sync。
- `Mbed TLS`（二档，Apache-2.0）：前置是 RNG、Clock、Socket。
- `TinyUSB`（一档偏二档，MIT）：前置是同步原语。
- `WAMR`（WASI/Wasm runtime，二档偏三档，Apache-2.0）：前置是 File 与 Namespace。Wasm 是组件的一种执行后端，与执行域正交，不是第四个执行域。
- `picolibc`（C libc，一档，BSD-3-Clause）：要写 `_write`/`sbrk`/`_exit` host-glue 加交叉编译配方。
- `virtio-drivers 0.13.0` 已经在用，落在 `virtio_blk` 组件内部，Hal 适配器私有，不 fork 上游。

统一 host 接口是这条线的关键设计，状态要分清：Block 已落地；Files/Namespace 是设计；Net、RNG、Clock、Log、Thread、Sync 还是提案（`porting.md` §8）。不要把这些提案当已实现能力用。

打包链已经落地：`.kcomp` 加 cpio `.initpkg` 加文本 manifest，就是 Linux insmod/initramfs 模式的简化版。运行期依赖解析（depmod 模式）与 Runtime Graph 后置。

## 9. POSIX 边界

POSIX 不是当前能力。将来也只先做最小 personality 原型，不是 BusyBox，用途是验证边界：Task/AddressSpace、block-wake、timer、filesystem service、stream-IO、进程语义状态。

模型是 POSIX Process → `AddressSpaceId` + N × (POSIX Thread → `TaskId`)。PCB、TCB、fd、cwd、session、credentials、signal handler 与 mask、pending signal、fork、exec、waitpid、mmap、brk 全部留在 personality。Core 不得变成 POSIX 内核。顺序是先做通用 block/wake/event/notification/wait/cancel，不要提前造 `core::signal`。

## 10. Core 冻结判据（A 到 H）

freeze 的判据不是数功能，而是多个差异很大的上层负载能只用现有 Core 原语构造出来，并且不再持续索要新的基础对象类型。逐条对当前仓库：

| # | 场景 | 现状 | 依据 / 缺口 |
|---|---|---|---|
| A | Driver：真设备 → claim → MMIO/IRQ/DMA → driver component → Endpoint，最好在真 SoC 上 | PARTIAL | 机制齐全，`virtio_blk` 加 `driver_prober` 在 QEMU 跑通；无真 SoC；IRQ 单线；无 PCIe/USB/NVMe |
| B | 多实例：一个 artifact 到实例 A/B，各自 state/resources/endpoints/tasks，无串扰 | PARTIAL | 已证：host `same_artifact_loads_produce_independent_components`、CoreTest `driver-multi-device`、ArchTest `isolated-restart`（并发同 artifact）、`ram_blk_rw` 每实例 buffer；endpoint/task 全维度无串扰与卸载后语义未系统证明 |
| C | 服务组合：BlockDevice → Filesystem → 更高消费者，Core 不理解 FS 语义 | PARTIAL | `fatfs`/`littlefs` 已绑 `block.device`，CoreTest `block-chain`/`littlefs-multi-instance`/`littlefs-isolation`；无 VFS、namespace、File service、两级缓存 |
| D | Task 运行时：Runnable→Running→Blocked→(wake)Runnable→Exited 加 timer/preemption | NOT-SATISFIED | TaskTable permit 路径已实现，但 host 调度集成测试因锁跨 schedule_next 失败；preemption（`on_timer_tick` 是 `todo!()` 且未接线）也缺失 |
| E | POSIX 原型：process semantic state → AddressSpace → 多个 Core Task，PCB/fd/signal 留在 personality | NOT-SATISFIED | 没有 POSIX personality；前置的 task↔AS 与 block/wake 也不满足 |
| F | 执行域：同一 service contract 至少在 KernelNative + IsolatedNative 上验证，Sandbox 后加 | PARTIAL | K→I Gate 已真实验证（ArchTest `isolated-service*`：自定义 contract 加测试 provider，Core 侧代登记 endpoint）；没有任何生产契约（`scheduler_rr`、`block.device`、filesystem）在 Isolated 上运行过；Isolated provider 不能自己 publish endpoint；资源型契约全被 `-ENOTSUP` 拒绝 |
| G | 真机：至少一块 QEMU RISC-V virt 之外的真 Linux-class RISC-V 板 | NOT-SATISFIED | 零真机代码与配置 |
| H | 不同机器类别：RV64 Linux-class 加 RV32 NoMMU embedded/MCU 共用同一套小 Core | NOT-SATISFIED | 两种 profile 都存在，但 NoMMU 从未被构建或启动；无 MCU 真机 |

结论：A/B/C/F 是 PARTIAL，D/E/G/H 未满足。Core 还没到 freeze-candidate。已经满足的机制侧说明词汇表方向是对的，缺的是等待语义、task 与 AS 的关系，以及真机与异构机器验证。

## 11. 明确未做

- SandboxedNative（U-mode 加私有 AS 加 `ecall`）：`todo!()`（`load.rs:183`、`exit.rs:129`），没有 syscall wire ABI。NOT IMPLEMENTED / PLANNED。
- 抢占：`sched::on_timer_tick` 是 `todo!()`，`timer::on_trap` 不调用它。NOT IMPLEMENTED。
- block/wake 任务原语：ABI 与 TaskTable permit/owner 已实现；host property test 已启用，调度集成测试当前失败于 TaskTable 锁跨 `schedule_next`，IRQ 恢复路径仍待修正。trace 事件未定义。IN PROGRESS。
- SMP：`CONFIG_SMP`、`os/core/src/smp/`（`CpuMask` / `PerCpu` / `BootGate` / `CpuBootState` 已实现并有 host 单测）、arch 的 `Smp` trait、per-CPU seams（sched/timer/irq/containment）与显式中断使能生命周期已落地；per-CPU 状态、AP 启动、IPI 投递、`sscratch` 入口记录、跨 CPU `Running(CpuId)` 互斥仍 `todo!()`。IN PROGRESS。
- 本阶段明确不做（原路线图的"明确不做"清单并入本文，与 `AGENTS.md` 一致）：真正动态加载、运行期组件热插拔 / Runtime Graph / 依赖解析器、热迁移、复杂 IPC、微内核模式、Wasm runtime、WIT/IDL、完整 capability 系统、完整 POSIX、Linux syscall 兼容、复杂 VFS、复杂 SMP 调度、形式化证明、完整 driver framework、完整依赖解析器。NOT IMPLEMENTED / PLANNED。
- 尚未完成方向（方向，不是承诺的里程碑，没有排期）：多 profile（`game` / `unix`(POSIX personality) / `micro` / `debug`）与 UserAddressSpace 执行域；Wasm 执行后端（`scheduler.wasm` 等，是组件的一种执行方式，与执行域正交，Core/Arch 保持 native Rust）；热替换（在 drain 协调协议之后向无感替换演进：quiesce → stop → unbind → reset → replace → bind → start；不做 live state migration）；内存物理回收（完整 buddy、通用 Core heap、完整 panic recovery；phase 1 只做资源归属撤销与 quarantine，不承诺共享堆字节回收，也不承诺对抗隔离）；验证工具链（Kani / Loom / Miri / Verus 与 Test Scheduler / Hunt Mode）；第三方库调包（见第 8 节）。
- 内存物理回收与 instance 退役回收：不承诺，逻辑死亡、物理驻留。NOT IMPLEMENTED。
- NoMMU 启动验证：profile 存在但从未 boot。PLANNED / 未验证。
- M-mode（`PRIVILEGE_MACHINE`）启动：Kconfig 可选、代码可编译，但没有 defconfig、没有 boot harness、不在任何测试或 CI 里构建。PLANNED / 未验证。
- AArch64 / x86_64 / LoongArch：`os/arch/src/<isa>` 与 `os/boot/<isa>` 同形骨架已落地（`encoding`/`elf`/`console`/`cpu`/`smp`/`trap`/`context`/`mmu`，实现体 `todo!()`），能编译、未启动、未验证。NOT IMPLEMENTED（骨架）。
- 真机支持（VisionFive 2、ESP32-C3 等）：零代码与配置，纯路线图。PLANNED。
- `embedded-hal` / `embedded-io` / `embedded-storage` / `embedded-nal` / `embedded-graphics`：仓库里完全不存在。PLANNED。

## 12. 与文档的已发现漂移

只登记，不改写。

- `docs/architecture/deployment.md` §7.4/§11 局部陈旧：仍把"consumer exact ABI（组合期）"标为未做。但 HEAD 的 `kcore_endpoint_bind` 第一步就走 contract 加 abi exact-match 的 `EndpointRegistry::lookup`，`kcore_endpoint_validate` 也已从 C ABI 可达（SDK 已接）。真正剩下的只是"`lookup` 发现路径不带 abi 参数"这个设计选择的措辞，`abi/core.toml` 和 `export.rs` 的 lookup 文档已自述不校验 abi，与代码一致。
- `docs/modules/components.md` 漏登记两个已在 `KCOMP_SRCS` 里的 fixture：`tests/kcomp_isolated_direct`、`tests/kcomp_isolated_unsupported`，全 `docs/` 没有引用。
- `README.md` 目录说明列了 `components/drivers/ uart/ virtio_blk/ …`，但 `uart/` 不存在，`driver-model.md` §4 明说 uart 未实现；README 的 monitor 命令列表漏了 `unload` 和 `trace`，`docs/modules/core/monitor.md` 有。
- 源码注释陈旧，不改 Core 代码：`os/core/src/component/endpoint.rs:234` 仍写 IsolatedNative"未实现：无私有 AS / `satp` 切换"，与已落地的私有 AS、trampoline、生命周期代码矛盾；`os/core/src/memory/address_space.rs:2030` 的"对应 roadmap 的 NoMMU 验收点"失去所指（路线图文档已删除，NoMMU 现状见 3.23 与第 7 节）。
- 计数校验：CoreTest 当前有 49 项 distinct 检查（bit 0 到 49，bit 23 未用；2026-09-27 的 `41/41` 日志是旧快照）；ArchTest 有 41 个 case。本快照在 `0d189e1` 复跑，数字未变。
