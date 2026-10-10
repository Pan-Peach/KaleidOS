# Component Runtime：第一轮审计、实施任务与验证

> 审计/实施建议，2026-10-10；本地 develop HEAD
> `7e3a3ed1da2e503d0acb74892dea50c977957b73`，起始工作区干净，未 fetch。
> §1–§6 保留首次审计/方案及当时证据；用户随后授权生产实现，当前代码与最新验证见 §7。
> 一般 Stop drain、S-mode 强杀与完整回收核算仍未完成，不从旧 PASS 推断这些能力。
> 身份不变，普通业务 IPC-only 已有，不重新实现组件系统。

契约分别在[生命周期 §11](../architecture/component-lifecycle.md#11-runtime-完整化当前与目标)、
[内存回收矩阵 §9](../architecture/memory-and-heap.md#9-runtime-回收矩阵目标与基线)、
[调度 §7](../architecture/scheduling.md#7-runtime-停止与私有-as)、
[IPC 私有域目标](../architecture/ipc.md#私有执行域接通)原地扩充。
本页只保存源码事实、工作拆分和证据，不成为第二套规范。

## 1. Phase 0 现状审计

本节是上述 HEAD 的历史基线，不是修改后快照。当前结果见 §7；函数名比易漂移行号更稳定，下列路径相对仓库根。

| 范围 | 真实已有能力 | 缺口 / 卸载阻碍 | 源码位置 |
|---|---|---|---|
| 身份 | ComponentId 单调；ComponentRecord 1:1 拥有 LoadedComponent；creator 不可改写 | tombstone 包含整个 backing，尚无拆除实体但保留身份的路径 | `os/core/src/component/registry.rs:54`，`declare/record_creator` |
| 装载 | K/I 每次独立放段、重定位；I 页级 RX/R/RW | K 无页级段权限；I image 一整份 lease，分段 mapping 不各自 owning | `component/loader.rs:89`，`isolated_load.rs::place_at/into_loaded_component` |
| 生命周期 | staged publication；create 成功记录 state 后 Ready；停止一次 destroy | 只有 Ready 可 stop；无 Task drain、Stop mode/deadline/reclaim 结果 | `component/load.rs`，`component/exit.rs:116` |
| Graceful | live Task/inflight/Native Direct 均 EBUSY，拒绝不改 Ready；成功 Stopped | 没有通知/等待；同步 destroy 无 watchdog；Stopping 不允许运行清理 Task | `exit.rs::stop_component/complete_stop`，`registry.rs::may_run/begin_stop` |
| Failed | revoke IRQ/DMA/device/Endpoint；Exchange owner_failed；跳过 destroy | 已在别 CPU 的任务只在下一安全点停止；Failed 不是物理静止 | `component/failure.rs:38`，`sched.rs:544/645` |
| 回收诊断 | trace 状态、task/page/component 数量、设备 quarantine 可查询 | failure reason 被丢弃；窗口 best-effort 回收失败无结果；无整体 retained reason | `failure.rs::fail_component`，`isolated_lifecycle.rs:634`，`component/export/query.rs` |
| Task | owner、固定 CPU、context、16 KiB kernel stack；park permit、真实 exit | component Task 无 AS 绑定；create 只检 image 整体区间，私有 Task 应检 EXECUTE 段 | `task/record.rs:12`，`task/mod.rs:144`，`task/table.rs::create` |
| Task 回收 | remove 后 TaskRecord Drop 可以 free stack | remove 不检状态；生产 exit 不 remove；Exited 在实际切栈前提交，当前安全性依赖驻留 | `task/table.rs:276`，`task/record.rs::drop`，`sched.rs:575/622/640` |
| Scheduler/SMP | RV64 K 协作调度、双侧 commit、per-CPU guard、远端 wake/IPI | 无 kernel preemption；Reschedule IPI 只 pending；无 stopped CPU/AS 确认 | `sched.rs::schedule_next_with_guard/on_timer_tick`，`smp/ipi.rs`，`timer/mod.rs` |
| 普通 context | ra/sp/s0-s11/tp 和每执行流 IRQ flags | RiscvContext 不保存 satp；普通组件 task trampoline 原生直接调 entry | `os/arch/src/riscv/cpu.rs:45`，`os/core/src/task/mod.rs::task_entry_trampoline` |
| I 同步执行 | private AS、Core 共享映射、跨 AS trampoline、create/destroy/Gate、fault 归因 | 单实例同步栈不可承载多 Task；无 persistent Task/import/新 IPC | `component/{isolated,isolated_lifecycle,isolated_call}.rs`，`os/arch/src/riscv/trampoline/` |
| I 支持面 | log/只读/panic/memory/endpoint imports | task/ipc/device/IRQ/DMA/shared heap imports 不允许；不能只扩白名单 | `component/isolated_load.rs:315`，`load.rs::validate_isolated_load` |
| AS | private/shared ledger 分开；handle/generation/owner；retire 禁止新 prepare | retire 只置状态；Manager 无实体 removal、backend teardown；PreparedActivation 无执行 pin | `memory/address_space.rs:173/573/585` |
| 页表 | Sv39/Sv32 frames 记录自有 root/中间表页；真实 map/unmap | PageAlloc 只有分配；vm_page_alloc forget；无 free/Drop，unmap 不还页表页 | `os/arch/src/vm.rs:43`，`riscv/mmu/{sv39,sv32}.rs`，`os/core/src/memory/mod.rs:291` |
| 私有 backing | mapping_exact + 动态窗口；acquire 清零；release 撤映射/恢复别名再 raw free | heap/image/页表停止后留驻；发布/恢复修改多个 live root，尚无 remote TLB drain | `component/backing.rs:27/77`，`memory/kernel_mappings.rs:359/383` |
| I 窗口 | create/service fault best-effort 归还预置 stack+ABI；destroy 退役 AS | 不能因窗口已释放称整实例回收；image、heap、表页仍在 | `isolated_lifecycle.rs::fail_provider/fail_with_as/release_instance_windows/destroy` |
| K Memory/Heap | lease RAII；显式 view release；SDK 共享 Core heap 窄 ABI | heap 无 owner 账；memory_acquire 的 deny_if_failed 不全面覆盖 Stopping，也缺锁内提交复验 | `component/export.rs:161/898`，`memory/mod.rs:107`，SDK `c/kcomp_heap_runtime.c` |
| Endpoint | owner/port/contract、exact validate/bind、永久 invalid，无静默 redirect | api/ctx 与 Direct stop pin 仍有真实诊断消费者；不能直接删除 | `component/endpoint.rs:480/522/581/598` |
| IPC | 16 slots、每端口4、1024B Core copy；grant、first terminal、cancel、wait/wake、exit | ABI transaction 只允许 K Task；typed bind exact，raw submit 不携 fingerprint | `component/{exchange.rs,export/ipc.rs}`，`endpoint.rs::bind` |
| IPC copy | K 指针 null/overflow/align/length 检查，Exchange 不留 caller 指针 | I/U 逐页校验、标量 marshal、copy 与 teardown 互斥、copy-out/消费原子性未有 | `export/ipc.rs:6/18/59/68/115`，`isolated_call.rs`（仅 Gate copy） |
| 普通 U 程序 | RV64 S/MMU UserDomain、U frame/FP、timer deadline、ecall/fault 回 task stack；copy chunks | owner 是 personality Task；不是 U component；published/retired_user 留驻，staging discard 仍漏表页 | `task/user.rs:110/166/228/384/448/553`，`os/arch/src/riscv/user{.rs,64.S}` |
| Sandbox | 类型声明与显式 ENOTSUP，无实例/native fallback | load/import/runner/Core ecall/destroy 均缺；不能把普通 ELF U 测试当组件证据 | `component/sandbox.rs`，`load.rs::create_sandboxed_native`，`exit.rs` Sandbox todo |
| trap | 统一异常 hook、I 故障和普通 U trap；U timer 可回 user runner | supervisor 的 UserEnvCall 已经走 hook，部署文档“直接 panic/未开始”已过时；不是新 component ecall | `os/arch/src/riscv/trap/supervisor.rs:85`，`component/isolated.rs::on_exception` |
| IRQ/callback | revoke 防新 route；锁外 callback 与 inflight；policy 串行 Core 栈 | 已准入 callback 未排空不能 free；IRQ panic fatal；其他裸 callback 未自动追踪 | `resource/irq.rs`，`irq/mod.rs`，`component/containment.rs`，`sched.rs` |
| DMA | allocations owning lease；mapping owner=device owner；单调 id；free/revoke Quarantine | mapping 不存 ptr/len/backing 引用；借入普通 buffer 未 pin；unmap 不检 ambient；无静默确认 | `resource/dma.rs:116/123/159/179/214/239/298/344` |
| Device/MMIO | 独占、子项 release 门禁、失败 quarantine；native 裸窗口 | 撤 owner 无法撤 K 裸指针；I/U device import 未接；CPU stop 不停止设备 | `resource/device.rs`，`resource/irq.rs` |
| VirtIO teardown | destroy 写 DeviceStatus=0、drop BLK、残留 unmap、release device | live Block Task 阻止 generic stop；reset 无 Core ack；DMA backing 仍 Quarantine；device_id!=0 的哨兵与合法零 ID 待单独审计 | `os/components/drivers/virtio_blk/src/lib.rs:367` |

### 1.1 旧通道消费者与删除条件

普通 Echo/Block/Filesystem/VFS/Posix/Probe 已生成 IPC；SDK 无普通 Block/FS 多 Backend。
继续使用[现有迁移清单](component-communication-migration.md#3-逐文件-cleanup-清单)，
不第二次迁移这些服务。真实保留者包括 `kcomp_domain_service`、`kcomp_checksum`、
CoreTest `runtime/{deployment,convergence}.rs`、ArchTest `isolated-*` 和专用 PolicyCall。
I Gate 的跨 AS、fault、stale、reentrant 回归必须在真实 I Task/IPC 替代后逐项移植。
IRQ、runtime_init、create/destroy、scheduler policy 不属于普通 RPC，不一起删除。

删除 Direct 后只解除该原因的 stop pin；Task/IRQ/policy/Gate 返回地址、私有栈和
DMA 风险仍在。没有新增 image refcount 方案，也没有以 inflight==0 推断全部可回收。

### 1.2 两组成熟系统依据

核对日期 2026-10-10，不复制外部实现。

| 参考与证据等级 | 事实 / 本项目采用范围 | 简化或拒绝 |
|---|---|---|
| Linux 固定 v6.12 [exit.c](https://github.com/torvalds/linux/blob/v6.12/kernel/exit.c)、[sched/core.c](https://github.com/torvalds/linux/blob/v6.12/kernel/sched/core.c)，源码 `exit_mm/do_exit/finish_task_switch` | 退出拆除和切栈后的清理分别处理。KaleidOS 采用离场后回收这一约束；已有 Exited 在切栈前不是证明 | 不引入 PID、signal、RCU/task/mm 全套结构；KernelNative 不等于 Linux 用户进程 |
| Zircon [Process 官方文档](https://fuchsia.dev/fuchsia-src/reference/kernel_objects/process)，在线文档更新 2025-02-28，无固定源码审计 | process 容纳 threads、VMAR、handles；借鉴以实例归属协调生命周期 | 复用 ComponentRecord/Task/AS；拒绝 handle/job/VMAR registry 全套。文档不能替本项目证明 kill 或 DMA 安全 |
| Linux [TLB 官方文档](https://docs.kernel.org/core-api/cachetlb.html)，在线而非 v6.12 固定快照 | 页表变化需完成对应翻译失效；本项目从无活跃 root 与远端确认建立物理复用条件 | 初期固定 CPU、ASID 0 全量 flush；不先实现 ASID allocator/lazy TLB/完整 Linux mmu_gather |

这些是本项目设计推断，不表示机制等价。路由的 Fuchsia/QNX 比较单独见[ADR](ipc-routing-service-discovery-adr.md)。

## 2. 执行域与保证

| 域 | 本 HEAD 真能力 | 下一验收 | 不能承诺 |
|---|---|---|---|
| K | 真 Task/IPC/协作 SMP；无 Task Stop + tombstone | CPU-only 清理任务退出、离场确认、独占 image/stack 回收 | 共享 heap 漏对象自动识别、裸指针追回、任意 S-mode fault 恢复 |
| I | 真 private AS/create/destroy/Gate/private heap；无持久 Task/IPC | 固定 CPU Task/AS、Core bridge/copy、K/I矩阵、AS实体 teardown | 私有 AS 等于特权隔离；任意不 yield/禁IRQ/破坏 Core 的强杀 |
| U | 普通 U 程序机制已有；组件 create ENOTSUP | RV64 CPU-only Echo，native stub/ecall，九格矩阵，忙循环退出、私有回收 | RV32/NoMMU/另一 ISA、设备/DMA、任意 Core-critical bug containment |

卸载与回收矩阵的唯一依据为[内存 §9](../architecture/memory-and-heap.md#9-runtime-回收矩阵目标与基线)。
Graceful/Force/逻辑死亡的状态和竞争表在[生命周期 §11](../architecture/component-lifecycle.md#11-runtime-完整化当前与目标)。

## 3. 文件级实施任务

以下为审计时的拆分与验收；Result 记录首次交付状态。授权后实现及仍缺部分统一映射到 §7，避免把目标当成当时的事实。

下列均为生产实现待办，按依赖分独立补丁；每项必须有真实消费者再扩大 API。
“维护点”是预计新增的手写机制入口数，不是新增 crate/框架。Phase 0 的测试/文档
已落地，其余 Result 不能标 IMPLEMENTED。

### P0 审计与测试基线（本轮）

- **Problem**：文档混用目标/事实，回收链和 U 普通程序能力描述有漂移。
- **Invariant**：源码/测试证据分层；旧 Gate 回归保留；生产逻辑权限按 AGENTS。
- **Minimal Change**：本页、路由 ADR、权威页扩充与 `exchange/tests.rs`；新增生产维护点 0。
- **Reuse**：现有 host Exchange tests、CoreTest、ArchTest、四项 make 门禁。
- **Removed Complexity**：修正旧“prober 第一盘即停止”和“U ecall 全未开始”文档；不删生产通道。
- **Non-goals**：不能把审计完成当 I/U实现；不复制外部代码。
- **Tests**：三个新的纯 Exchange host 用例；四项命令实测记录见 §5。
- **Result**：审计、契约目标、矩阵、任务、ADR、host 测试完成；I/U与回收未接线。

### P1 IPC-only 消费者门禁

- **Problem**：旧同步诊断仍承担真实私有域覆盖，直接删除将失去证据。
- **Invariant**：同 Contract/Handler、旧 endpoint 失效与故障归因不能退化。
- **Minimal Change**：迁移清单及 `core_test/runtime/{deployment,convergence,ipc}.rs`；先添加对照，依赖 I1/I2，生产维护点 0。
- **Reuse**：Echo generated Provider/Client 与现有 domain.test硬件断言。
- **Removed Complexity**：替代通过后删 test-only 业务 Gate adapter；Core Direct/Gate 单独确认所有消费者再删。
- **Non-goals**：不删 PolicyCall/IRQ/lifecycle；不改 KABI 方法生成器。
- **Tests**：K/I四格与原 fault/reentrant/stale证据；完整 check/host/QEMU/Arch。
- **Result**：待 I2；现普通业务 IPC-only 已有，无需再次迁移。

### L1 Stop 准入与结果（Phase 2）

- **Problem**：Stop 拒绝 live Task，may_run 不支持清理；memory acquire 部分入口只拒 Failed。
- **Invariant**：一个 Stop/destroy 认领；新工作全部锁内拒绝，拆除可执行。
- **Minimal Change**：`component/{registry,exit,failure,export}.rs`、`export/ipc.rs`、`sched.rs`、`abi/core.toml` 与生成物；维护点 2（停止谓词/结果）。
- **Reuse**：Stopping/Stopped/Failed、既有锁序、trace/errno、owner 内 unpark。
- **Removed Complexity**：统一停止门禁，消除分散的 Failed-only推断；不加新状态族。
- **Non-goals**：同步 destroy 尚无强制 deadline；不承诺 S-mode忙循环强杀。
- **Tests**：host双CPU stop/create/submit/fail交错、重复Stop/析构失败；CoreTest已有Worker收到stop并退出。
- **Result**：待实现；风险为错误放开Stopping的新资源准入，验收先关闭此路径再接清理调度。

### L2 Task 离场与停止（依赖 L1）

- **Problem**：Exited先于切栈；remove可移走仍被CPU使用的record。
- **Invariant**：离场确认前 context/stack保活；远端不能伪造Running退出。
- **Minimal Change**：`task/{table,record,state}.rs`、`sched.rs::schedule_next_with_guard/finish_switch`、`containment.rs`；维护点 2（离场确认/终态实体回收）。
- **Reuse**：固定CPU、incoming stack、per-CPU状态、IPC task_exited。
- **Removed Complexity**：退出后持续保留整张Task栈可由最小tombstone替代；不改TaskId复用规则。
- **Non-goals**：不实现迁移/work stealing；无抢占时Running只等安全点。
- **Tests**：host错误owner/非法状态/保存中删除拒绝；CoreTest/ArchTest SMP延迟离场，远端看到Exited仍不得free。
- **Result**：待实现；风险是switch raw pointer与panic abort上下文，验收须覆盖fresh/park/yield/exit/abort。

### M1 AS/backend teardown（依赖 L2；先用于 never-entered AS）

- **Problem**：页表frames只有分配，AS retire保留实体，OOM partial map也留表页。
- **Invariant**：表页只free一次；shared/leaf backing不被误free；活动root拒拆。
- **Minimal Change**：`os/arch/src/vm.rs`、`riscv/mmu/{sv39,sv32,address_space}.rs`、fake/stub/nommu合同实现；`memory/{mod,address_space}.rs`；维护点 2（PageFree/teardown）。
- **Reuse**：backend frames、MemoryLease/raw extent free、AS handle/generation、现有map回滚tests。
- **Removed Complexity**：消除永久forget表页；不复制frame表或新建AS registry。
- **Non-goals**：不从teardown释放业务mapping backing；不任意扩trait抽象。
- **Tests**：host create/drop与中间建表OOM物理基线；ArchTest实际root离场、TLB后回收。
- **Result**：待实现；先验收never-entered失败路径，再接已发布AS，风险是旧activation与部分失败。

### M2 CPU-only backing 回收（依赖 L1/L2/M1）

- **Problem**：image/heap/stack实体驻留，窗口best-effort无保留原因。
- **Invariant**：精确extent来自loaded lease/Task lease/AS动态窗口，不逐PTE free。
- **Minimal Change**：`component/{exit,loader,isolated_lifecycle,backing}.rs`、`memory/{address_space,kernel_mappings}.rs`、trace/query；维护点 2（回收推进/保留诊断）。
- **Reuse**：独占lease、AS mapping_exact、image整extent、别名恢复事务与现有资源表。
- **Removed Complexity**：拆除不必永久pin的独占CPU-only资源；保留必要身份tombstone。
- **Non-goals**：K共享heap批量free、DMA/IOMMU/资源容器；不猜丢失mapping的allocation。
- **Tests**：host double/wrong-owner/alias-failure；CoreTest K无裸引用Echo，ArchTest I页表/heap/stack精确基线与另实例存活。
- **Result**：待实现；需先能观察保留原因，不能用record数量作为物理验收。

### I1 持久 Isolated Task（Phase 3；依赖 L2）

- **Problem**：同步trampoline一次返回，component Task无AS/私有业务栈。
- **Invariant**：owner/AS/entry EXECUTE绑定；Core ABI和调度在Core root/栈执行。
- **Minimal Change**：`task/{mod,record,table}.rs`、`sched.rs`、`component/{isolated,isolated_call,isolated_lifecycle,isolated_load,containment}.rs`、arch trampoline；维护点 2（I Task runner/Core bridge）。
- **Reuse**：现trampoline、prepared activation、per-CPU boundary；保留Core kernel stack。
- **Removed Complexity**：成功后业务不再借caller的同步栈；初期保留Gate对照。
- **Non-goals**：首补丁固定一CPU；不声称S-mode安全沙箱，不先扩device面。
- **Tests**：ArchTest多次yield/park/exit、两个实例不同数据、timer/trap/root/tp/IRQ恢复；CoreTest真Task查询。
- **Result**：待实现；验收任务暂停时root回Core、恢复正确，不仅create时能进入。

### I2 私有域 IPC copy/import（依赖 I1/L1）

- **Problem**：K-only transaction、私有buffer/所有标量未marshal。
- **Invariant**：Core身份/grant、不留guest pointer；copy-out失败不消费；copy期间backing保活。
- **Minimal Change**：`component/export/ipc.rs`、`isolated_load.rs`、`isolated_call.rs`、`memory/address_space.rs`、SDK现IPC adapter；维护点 1（有界copy入口），Exchange终态逻辑复用。
- **Reuse**：Gate访问检查思路、UserDomain逐页copy、1024B Core副本、generated Echo。
- **Removed Complexity**：真四格通过后删对应domain.test手写业务codec/adapter。
- **Non-goals**：不重写Exchange/Wire；不原样让I在私有栈park；不加另一SDK。
- **Tests**：host范围/洞/溢出/只读输出/标量bad ptr；QEMU K/K K/I I/K I/I真请求、退出、取消、短输出与并发unmap。
- **Result**：待实现；风险为copy与消费事务及root桥接跨park，必须有copy失败可重试证据。

### I3 SMP AS 与 I 回收（依赖 I2/M1/M2）

- **Problem**：publish/release会修改其他live root，只有本地flush。
- **Invariant**：停止/翻译失效应答后才free；迟到IPI不能解除新请求的保护。
- **Minimal Change**：`sched.rs`、`smp/ipi.rs`、`component/isolated.rs`、`memory/{address_space,kernel_mappings}.rs`；维护点 1（CPU确认），优先复用原IPI。
- **Reuse**：fixed placement、Reschedule、安全点、ASID0全量sfence。
- **Removed Complexity**：以真实离场证据替代无条件驻留；不加active-AS manager。
- **Non-goals**：不yield/禁IRQ的S-mode强制有限停止、不加ASID或migration。
- **Tests**：ArchTest延迟远端确认、旧TLB访问、别实例仍可用；CoreTest跨CPU K/I矩阵与unload。
- **Result**：待实现；无确认就报pending/retained，不能强行通过压力门禁。

### U1 Sandboxed 装载与 lifecycle runner（Phase 4；依赖 I2/L2）

- **Problem**：Sandbox create直接拒绝，普通UserDomain依赖personality Task且私有持AS。
- **Invariant**：Component仍唯一实例；U image/stack为USER且W^X；Core/设备映射不可U访问。
- **Minimal Change**：`component/{load,sandbox,isolated_load,isolated_lifecycle}.rs`、`task/{record,user}.rs`、arch user frame；维护点 2（域参数放段/U runner）。
- **Reuse**：ELF/relocation与页级放段、普通U run/stop整数FP现场、Component既有AS；U机制最小提取不复制UserDomain。
- **Removed Complexity**：接通后移除SandboxUnsupported/todo分支；不继承PID/ELF进程生命周期。
- **Non-goals**：首批RV64/S/MMU Echo；RV32/NoMMU明确ENOTSUP，驱动不在范围。
- **Tests**：ArchTest真实U入口/初始化/返回/fault、Core地址拒访、USER/W^X、失败装载回滚；同Handler K/I/U build。
- **Result**：待实现；风险是普通U程序回归与AS多Task共享，保持personality测试。

### U2 Core ecall adapter（依赖 U1/I2）

- **Problem**：U不能调用native export，现普通ecall交personality解释。
- **Invariant**：按真实deployment路由Core op；返回Core task stack处理wait，逐页验证所有指针。
- **Minimal Change**：`component/sandbox.rs`、`task/user.rs`、`component/export/{ipc,query}.rs`、`abi/core.toml`、SDK私有stub与构建脚本；维护点 2（op解码/stub），只开Echo必需支持面。
- **Reuse**：已有U trap/frame/copy、narrow C ABI facade和generated business IPC；syscall不解业务method。
- **Removed Complexity**：无需每个业务两份dispatcher；普通Linux号解析仍留personality。
- **Non-goals**：不native地址回退、共享runtime、自动生成全部Core ABI复杂marshaller。
- **Tests**：host错误op/宽度/权限/bad scalar输出；QEMU新增五格、伪造owner/grant/receipt、fault仅归U provider。
- **Result**：待实现；stub ISA/import不同允许工件差异，不承诺二进制天然可移植。

### U3 Stop/destroy/force/reclaim（依赖 U2/L1/L2/M1/M2）

- **Problem**：timer可回普通U runner，但无component停止检查或实体回收。
- **Invariant**：trap返回自己kernel stack后确认，不再sret；Force不运行destroy。
- **Minimal Change**：`component/{sandbox,exit,failure}.rs`、`task/user.rs`、`sched.rs`、`timer/mod.rs`；维护点 1（U离场停止推进）。
- **Reuse**：execution_deadline、真实UserContext、Exchange close、backing/AS teardown。
- **Removed Complexity**：移除终态完整UserDomain驻留，保留必要失效身份。
- **Non-goals**：Core-critical区任意异步abort、Sandbox VirtIO、通用内核抢占。
- **Tests**：QEMU U忙循环/不ecall/页故障、destroy挂死有界停止、远端确认、100轮完整页基线，未通过不标Force完成。
- **Result**：待实现；风险为多CPU trap返回与kernel引用排空，先单CPU再SMP。

### P5 故障与压力门禁（依赖 M2/I3/U3）

- **Problem**：现有1000轮host transport不证明load/unload物理回收。
- **Invariant**：预期retained与异常残留可逐项解释，不用误差掩盖增长。
- **Minimal Change**：`core_test/runtime/{ipc,deployment,smp}.rs`、test-only Echo fixture、ArchTest与QEMU runner；生产维护点只复用只读诊断，不增测试后门。
- **Reuse**：真实公开load/create/stop/Task/Endpoint/trace与free_page_count。
- **Removed Complexity**：协议同一事实不在多层重复测，私有表模拟不计验收。
- **Non-goals**：host fake隔离证据、DMA压力、以Stopped代替Reclaimed。
- **Tests**：§4全矩阵；每域100→数百→1000轮，正常/panic/force/OOM；最终四门禁。
- **Result**：待真正回收；风险是identity tombstone/slab cache增长，先定义有界基线再压力。

### P6 DMA 后续研究

- **Problem**：mapping不pin backing，reset未确认，释放仍Quarantine。
- **Invariant**：CPU静止不等设备静止，异常不依赖failed driver清理。
- **Minimal Change**：后续在现device/DMA seam独立设计；本轮仅更新driver契约，生产维护点0。
- **Reuse**：allocation/mapping分离、Quarantine、VirtIO正常退出代码。
- **Removed Complexity**：本轮无删除；保守驻留不能因CPU-only通过而移除。
- **Non-goals**：IOMMU、DMA Pool、通用恢复框架、故障后重新认领实现。
- **Tests**：保持已有DMA/device/真实storage门禁，不用纯CPU测试推导DMA安全。
- **Result**：推迟且不阻塞CPU-only生命周期；需先确认可验证device reset协议。

### P7 IPC 路由（仅设计）

- **Problem**：Contract/Endpoint/授权/组合策略易混为一体。
- **Invariant**：Endpoint不转移权限、旧身份不重定向、目录不逐消息中转。
- **Minimal Change**：[路由ADR](ipc-routing-service-discovery-adr.md)，生产维护点0。
- **Reuse**：create config注入、lookup/validate/bind/grant、现Exchange。
- **Removed Complexity**：拒绝无需求Connection Registry/第二Endpoint库。
- **Non-goals**：router/fastpath/ring/shared buffer实现，不成为I/U前置。
- **Tests**：设计列选择/拒授权/provider重启例；已有grant/stale host作为现机制证据。
- **Result**：设计文档完成，无路由代码。

## 4. Lifecycle Test Matrix

“已有”只指该格列出的层次；目标测试没有写成跳过占位PASS。

| 场景 | 已有证据 | 新增验收 / 层次 |
|---|---|---|
| 正常load/exit | host实例独立、CoreTest lifecycle、I ArchTest lifecycle | CoreTest真Task+IPC+destroy一次；ArchTest表页/extent归还 |
| Create失败 | hostloader拒绝；I lifecycle-fail/fault | 已start Task排空、失败commit/OOM无半活实例；host+真域 |
| Destroy失败 | host complete_stop；I destroy-fault | nonzero/panic/挂死不重试，结果含retained原因；CoreTest/ArchTest |
| 运行panic | K CoreTest SMP panic；I同步fault | I/U持久Server fault；端口失效、Task离场和回收分别断言 |
| Force | 未有 | U无yield忙循环真timer/stop；K/I只证明安全点、Pending与保留，不能伪造强杀 |
| Caller中途exit | Exchange host、CoreTest IPC | 私有Task退出，accepted receipt占用到退役，copy不访问旧页 |
| Server中途exit | Exchange host、CoreTest IPC | I/U端口close，Pending ENOTCONN；成功reply在exit后可collect |
| Stop/Reply并发 | host首终态全部顺序；本轮exit/reply交错 | RV64两个CPU实际reply/stop，各只有一结果；不能早free |
| SMP执行/stop | K跨CPU Gate/Stop EBUSY | Task/AS离场确认、迟到IPI、save in progress、remote TLB后复用 |
| 非法访问 | I ArchTestRX/NX与AS权限；普通U进程fault | I条件性fault保留Core存活；U不能触Core/别实例；真实trapcause |
| OOM | loader/AS host部分失败；普通用户OOM串口场景 | teardown元数据预留；create/task/page-table/copy/destroy-stack失败资源归属明确 |
| 多实例 | host同artifact；I restart；driver multi-device | 相同Echo多个owner/AS，停一个另一个继续IPC，backing不互free |
| Stale Endpoint | Endpoint host与真实Gate旧绑定 | 新实例不接旧request/grant/session，真IPC跨域旧id拒绝 |
| 重复装载卸载 | 本轮1000轮Exchange槽位复用（host） | 真K/I/U各100→数百→1000轮，物理页/表页/Task/Endpoint/backing分别比较 |
| 错误身份/提案 | host owner/Task/sched/grant拒绝 | I/U自报身份、错误receipt/buffer、错误AS/CPU拒绝，状态不半提交 |

| caller → provider | K | I | U |
|---|---|---|---|
| K | 当前真IPC | 待I2 | 待U2 |
| I | 待I2 | 待I2（两个不同AS） | 待U2 |
| U | 待U2 | 待U2 | 待U2（两个不同AS） |

业务使用同一个 Echo schema/generated Handler；adapter/工件允许按ISA与import变化。
K/I四格先RV64验证，I既有RV32硬件回归继续保留；U首阶段RV64，其他组合明确拒绝。

回收实验先热身初始化全局Exchange/slab，记录有效基线；每轮load同artifact、真实
Task/listen/grant/IPC、stop/fault、确认离场、destroy/force、teardown，再读只读资源
投影。CoreTest只用公开API；ArchTest证明真实root/TLB/页表页。终态Task/Component/
Endpoint最小tombstone增长与其实体资源分开；如metadata分配需要扩容，要能给出
确切数量、上界/压力预算，不把所有单调增长都称缓存。Quarantine各保留extent和原因。

## 5. 本轮实际测试记录

运行命令按[测试指南](testing.md)，日志保存在临时目录；均在上述HEAD加本轮修改的
工作区执行。QEMU使用resolved私有profile，不修改用户.config；没有工具/网络环境阻断。

| 命令 | 实际结果 | 层次 / 日志 |
|---|---|---|
| `make check`（修改前基线） | PASS，exit 0 | fmt/clippy/host/RV64 build/RV32 check；`/tmp/kaleidos-runtime-check.log` |
| `make test-host`（新增测试后） | PASS，exit 0 | 新增三个纯Exchange生命周期测试；`/tmp/kaleidos-runtime-host.log` |
| `make check`（最终修改） | PASS，exit 0 | 新增测试格式/clippy/host/RV64 build/RV32 check；`/tmp/kaleidos-runtime-final-check.log` |
| `make test-qemu` | PASS，exit 0 | RV64 default/no-block各112项，RV32各90项，shell及init（RV64五场景/RV32四场景）；`/tmp/kaleidos-runtime-qemu.log` |
| `make test-arch` | PASS，exit 0 | RV64/RV32各43/43，RV64 SMP3/3；`/tmp/kaleidos-runtime-arch.log` |

新增host用例：`successful_reply_survives_server_exit_and_repeated_close`、
`caller_and_server_exit_orders_preserve_another_instances_request`、
`thousand_endpoint_lifetimes_retire_receipts_without_redirecting`。
第一项保留已回复字节，第二项覆盖caller/server/reply六种序列及另实例存活，第三项
验证取消receipt、已完成结果、双向退出与stale请求不耗尽槽位。它们驱动生产Exchange，
不执行组件入口、不建立私有AS、不证明1000轮物理回收。本轮不新增虚构Force API测试。
kernel host为577 passed/6 ignored（既有基准等），新增三项全部PASS；保持已有测试
选择，未把ignored当功能验证。新增文档链接41个文件目标/22个heading anchors均存在，
`git diff --check`通过。本轮没有新增NoMMU运行，不沿用历史NoMMU PASS当本轮证据。

## 6. 文档同步归属与本轮结果

| 页面 | 同步内容 | 权威边界 |
|---|---|---|
| component-lifecycle | §11停止/析构/竞争/结果目标，修正prober历史事实 | 身份/生命周期唯一契约；当前Stop行为保留 |
| memory-and-heap | §9资源矩阵、extent/CPU/TLB/别名/页表回收不变量 | 不新建内存账本，当前驻留与目标分开 |
| scheduling | §7Task/AS/离场确认、K/I强杀缺口与U trap复用 | Scheduler truth，不放RR算法 |
| ipc/deployment | 同Wire私有域copy、身份与import/stub差异 | 当前K-only IPC与旧Gate事实仍保留 |
| driver-model | CPU-only优先与DMA推迟，I近期重点 | 不扩大驱动/静默能力 |
| STATUS/modules/testing | 链接本次审计、新测试、真实进度/门禁 | 现状仍以STATUS与代码为准，不记I/U完成 |
| 路由ADR | 控制面/数据面/选择与授权、扩展触发条件 | 设计决定，无框架实现 |

首次审计只修改文档与测试；授权后的生产改动和重新执行的门禁见 §7。
§5 的历史 PASS 不充当新增 I/U、Force 或物理回收证据。

## 7. 授权后的生产实现与验证

工作分支仍为 develop，base HEAD 不变；以下是当前未提交工作区的实际生产改动。
用户明确授权实现逻辑，覆盖 AGENTS 默认“人类实现”约定。没有新增 crate、路由器、
Image/Instance Registry、资源容器、业务 Wire 或 DMA 恢复框架。§1–§6 是历史审计，
生命周期/内存/部署/调度/IPC 的当前契约已就地更新。

### 7.1 I/U 装载、持久 Task 与 Core adapter（I1/I2/U1/U2 部分完成）

- **Problem**：原 I 只有同步生命周期/Gate，无可调度 Server Task；U 组件装载拒绝。直接在 private root/栈调度会保存不可恢复的 Core 执行现场。
- **Invariant**：ComponentRecord 是唯一 owner/domain/AS 真相；每 Task 的业务栈独立且首次清零；Core API 的 wait/park 只在 Core root/栈执行；U 不直调 Core 裸入口。
- **Minimal Change**：`component/{isolated_load,isolated_lifecycle,load,sandbox,isolated_api}.rs`、`export/sandbox.rs`、`task/{mod,record}.rs`、`exit.rs`。I 复用跨 AS trampoline return_call，U 复用 arch UserContext/run/stop/trap；不改普通 RiscvContext 的 satp 布局。
- **Reuse**：原 ELF/白名单重定位、私有 AS、SDK per-image HeapState、staged publication、普通 Task/CPU placement、arch U frame/timer。K/I/U Echo 是同一 business Handler/Contract/artifact（同 ISA）。
- **Removed Complexity**：移除 Sandbox todo/占位分支；无第二份 Echo dispatcher、无按业务 Method 的 Core 分派、无传输描述符。U 的 t0 syscall 编号保留 a0..a7，现有 C ABI 参数无需改 Wire。
- **Non-goals**：RV32 U、ASID、设备/IRQ/DMA、恶意 S-mode 恢复、任意 ISA/特权级二进制兼容、共享 runtime。
- **Tests**：CoreTest RV64 九格/RV32 四格真实 Task IPC；U heap create/destroy；ArchTest 原 K/I Gate/fault/restart/RX/NX 回归；host 验证未发布 lifecycle image 的 Drop 确实归还 backing。
- **Result**：I 在 RV64/RV32 S/MMU 接通，U 在 RV64 S/MMU 接通。I Task 为每 Task 私有栈与 Core 栈；U 10ms timer 返回 Core。I/U import 支持面仍窄，未知 import/platform 显式 ENOTSUP。新增两个部署 adapter 维护点，不新造 SDK。

### 7.2 Private IPC 与准入/复制事务（I2/L1 部分完成）

- **Problem**：原 transaction 只允许 K；裸 private VA 在 Core root 无效。先消费 receipt/result 再发现输出非法会遗失终态，validate 后 unmap 也可能造成 UAF。
- **Invariant**：真实 ambient owner/Task/Running CPU 鉴权；Endpoint/live/grant 独立验证；所有标量/payload 输出先验证；AS pin 覆盖 copy/Exchange 提交，Exchange 只存 Core 副本。
- **Minimal Change**：`component/export/ipc.rs`、`component/access.rs`、`memory/address_space.rs::Access/private_range_has_permission`、`endpoint.rs::bind`；Task create/memory acquire/release 持 registry 到对应资源提交与输出完成。
- **Reuse**：既有 Exchange 容量、first terminal、cancel/receipt、wait/wake permit、immutable creator grant、typed exact validate、generated C/Rust client/dispatcher。
- **Removed Complexity**：删除 K-only IPC 拒绝；不用 provider 解引用 caller 私有地址、SUM 或第二份业务 Wire；I/U 不扩充生成器职责。
- **Non-goals**：IPC timeout、业务 Session、无复制优化、shared ring、自动授权。raw submit 没有 fingerprint 参数，exact 校验仍在 typed validate/bind。
- **Tests**：九格往返、private NULL/Core VA IPC/Task 输出 EFAULT 后原 receipt 仍可 reply，无孤立 Task；原 Exchange caller/server/reply 顺序与1000槽位复用。host 不证明跨 AS。
- **Result**：同一 Exchange 已支持 I/U，逐页检查私有映射，U 要求 USER。锁序 registry→endpoint→Task 身份检查（释放 Task 锁）→AS pin→Exchange；wake/park 在全部锁释放后。Stopping 新 Task/IPC/backing 准入被拒绝；一般 Graceful cleanup Task 仍未开放。

### 7.3 Force、实际离场与 CPU-only 物理 reclaim（L2/M1/I3/U3 部分完成）

- **Problem**：Exited 在实际 context_switch 前提交；仅 Failed/Retired 或删除记录不能证明没有 CPU 继续使用代码/栈。页表 frames 无释放接口；早期 failure window free 与远端 Task 不兼容。
- **Invariant**：逻辑撤销先于物理释放；Force 不调用 destroy；无重复析构/释放；所有 owner Task Exited 且 actual departure 已确认、inflight=0，再拆 AS。不能在其他 live private root 有 stale alias 时复用 backing。
- **Minimal Change**：`component/reclaim.rs`、`sched.rs::departing/finish_switch`、`task/{record,table}.rs`、`registry.rs::reclaimed/pin_lifecycle`、`arch/vm.rs::PageFree`、Sv32/Sv39 teardown、`memory/address_space.rs::reclaim`、失败路径；`abi/core.toml` 仅新增 force_stop/reclaim 两个管理 API，生成物协调替换。
- **Reuse**：现有 fail/revoke/Exchange owner_failed、Task owner 表、LoadedComponent.memory、AS 精确映射、backend frames、共享别名计划、existing inflight。无 per-instance region 账本。
- **Removed Complexity**：统一已发布 private create/service/destroy 失败为先保留、同一 proof 后回收；移除不安全的 eager window free。Scheduler 只增加 incoming-stack acknowledgement；不造 reaper/stop phase registry。
- **Non-goals**：K image/shared heap 批量 free、任意 S-mode 强杀、remote TLB shootdown、DMA 静默/reset/IOMMU、普通 POSIX UserDomain 已发布 backing 回收。
- **Tests**：host stop_saved 不取消 Running/未确认离场的 Task，并保留另一 owner；Sv32/Sv39 teardown 只归还自有表页；ArchTest failure backing 先保留再真 reclaim、旧 AS handle 失效；QEMU U实际supervisor页load fault（cause13）、忙循环及 CPU1 Force→timer departure→reclaim。
- **Result**：Force 先失效，未离场 EBUSY，可有限重试；K 已发布 Direct 表返回 ENOTSUP 并保留 ctx，host 覆盖此边界；U timer 实际拿回控制权，K/I 非协作无保证。Reclaim 对 K ENOTSUP；私有域必须全局安全点（无任何 private Running Task/Starting/Stopping/inflight），否则 EBUSY。恢复 identity aliases 后移除 AS/root/页表，再释放独占 extents 与 Task Core/API 栈。内部 reclaimed 标记使成功幂等，Component/Endpoint 身份 tombstone 留驻。

生产镜像先声明 Component owner，再尝试 alias publication；未声明失败 image 可正常
Drop。root 创建失败可回收该 owner 的独占 image。stack/window/thunk/heap publication
失败保留既有 AS 精确映射；显式 release 先完成别名恢复，再撤 owning mapping，失败
可重试或留给实例 reclaim。不会把保留 backing 的所有权记录先删掉。未发布 root 建立
映射失败会释放已分配页表页。仍需要专门 OOM 注入逐阶段确认，而非只靠 happy path。

### 7.4 测试、文档与 IPC-only（P1/T1/R1 部分完成）

- **Problem**：只有 host Exchange 压力或同步 Gate，不能证明持久 private Server、CPU 停止与物理 backing 归还；文档已有未实现状态会与新代码冲突。
- **Invariant**：CoreTest 仅真实公开 API；测试 Task 参数 backing 保持到确认退出，超时保留；QEMU report 有界等待，不以 fake/skip 冒充隔离；路由只设计。
- **Minimal Change**：`kcomp_echo/{contract,src/lib}.rs`、`core_test/runtime/{ipc,deployment}.rs`、`tests/qemu/runner.py`（300s压力预算）、host/ArchTest、现有权威文档与本报告。NoMMU 通过公开 load 的平台前置拒绝检查判定 private 场景不适用，不新增 cfg 真相副本。
- **Reuse**：CoreTest KTAP、真实 Echo Provider、ArchTest 与 host 实现，现有迁移清单/文档索引。
- **Removed Complexity**：没有重新迁移已 IPC-only 的 Block/FS/VFS；同步策略/诊断/Gate 回归保留，尚不满足全部删除条件。还原工具格式化造成的无关 third_party 改动。
- **Non-goals**：routing/ServiceDirectory/Connection Registry/fastpath/ring 代码；DMA 组件压力回收、新上层业务。
- **Tests**：RV64 每轮交替 I/U（合计1000），RV32 I1000；同一 Echo Handler，含 normal stop、I非法指令/U真实supervisor页读fault、bad buffer、U busy、stale、repeat reclaim、Task 数量基线。远端 U busy 单独真实 CPU1。
- **Result**：测试与文档已接线，Routing ADR 仅文档。每轮验证 Task 数量回基线、reclaim 后可用物理页增加，100/200/…/1000 各阶段打印 free/returned/retained_peak；不是只检查 Stopped。最终准确命令结果见下表。

### 7.5 最新实际验证与证据范围

命令在当前 develop 工作区执行，私有 resolved profile 不修改用户 `.config`。

| 命令 | 最新结果 | 日志 / 层次 |
|---|---|---|
| `make check` | PASS，exit 0 | `/tmp/kaleidos-runtime-impl-check.log`；fmt/clippy/host/RV64 build/RV32 check |
| `make test-host` | PASS，exit 0；kernel 579 passed/6 ignored | `/tmp/kaleidos-runtime-final-host.log`；生产 truth/页表 teardown 与 ABI/构建测试 |
| `make test-qemu` | PASS，exit 0；RV64 default/no-block各119项，RV32各93项，shell与init全过 | `/tmp/kaleidos-runtime-final-qemu.log`；真实 CoreTest/IPC/1000轮，shell、init/普通U回归 |
| `make test-arch` | PASS，exit 0；RV64/RV32各43/43，SMP3/3 | `/tmp/kaleidos-runtime-final-arch.log`；RV64/RV32硬件与SMP |
| `make O=build/tests/runtime-nommu-rv32 _test-qemu-one` | PASS，exit 0；default/no-block各90项 | `/tmp/kaleidos-runtime-final-nommu.log`；RV32 S/NoMMU，private场景不适用 |

NoMMU profile 由 configure.py 以 RV32 CoreTest resolved `.config` 为基线，设置
`VM_MMU=n, VM_NOMMU=y` 创建；保留三个既有 cross-as dead_code 警告，未扩大为
NoMMU 隔离能力。没有网络/依赖环境阻断，也没有真机验证。

1000轮的常驻净增**尚未精确归因**：Component/Endpoint tombstone、名称、Core Vec/
slab metadata 会增长；目前只观察物理页总数与 Task 数，不逐一核算这些分配。
已证明独占映射/image/表页归还与重复回收幂等，**不宣称零不明泄漏**。后续 T1 需
热身后同时核对精确 backing/table-page 数与 tombstone/slab 实际容量，不能将净增
称作测量误差。当前每次建 root 带大范围共享 Core 映射，页表成本较高；本轮不优化。
RV64 default 具体样本：1000轮，Task基线90，累计returned_pages=2349500，
retained_peak=192页；U读0x80200000触发cause13，CPU1 busy的first=-16/stop=0/reclaim=0。
该192页尚未精确归因，不能据此签发无泄漏结论。66个Markdown文件的383个本地文件/anchor链接检查与
`git diff --check`通过；third_party无改动，未提交或推送。

### 7.6 尚需落地的任务与停止条件

按 §3 的 Problem/Invariant/Minimal Change/Reuse/Removed Complexity/Non-goals/Tests
继续推进；以下没有登记为完成能力：

| 小任务 | 文件 / 前置 | 验收与风险 |
|---|---|---|
| 一般 Graceful 通知/drain/有限推进 | `exit.rs/registry.rs/task/mod.rs/export/ipc.rs`；保留当前无live Task stop | 已有Task可仅为清理恢复、禁止新授权；重复stop/destroy failure/panic/stop-reply首终态、有限deadline；避免重新放开Stopping普通work |
| precise reclaim accounting | `registry.rs/endpoint.rs/memory/{slab,address_space}.rs`现有只读投影与CoreTest | backing/页表/Task实体分别为零；tombstone/容量/slab物理净增逐一核算；1000轮不接受不明余量 |
| OOM/失败阶段注入 | `isolated_load/isolated_lifecycle/backing/task/mod/address_space`及host/ArchTest | before declare/root/map/exclude/task/output/destroy各失败有owner、safe release或retained原因；不将Rust alloc abort假装可恢复OOM |
| SMP copy/unmap/stop-reply竞态 | `export/ipc.rs/access.rs/reclaim.rs`与CoreTest/ArchTest | 两CPU端到端无重复终态/UAF，临界区无反向锁；已有pin/ack只是机制证据 |
| remote TLB invalidation | `kernel_mappings/address_space/smp/ipi/sched`；明确进入/返回安全点 | 实际目标CPU确认后才复用；现全局私有安全点可EBUSY，不能删门禁再假装shootdown |
| K/I不可让出Task | `sched/timer/trap/irq`独立研究 | S-mode关IRQ/破坏Core不能保证强杀；未有安全抢占前只逻辑失效与保留，不返回假成功 |
| K/I原生浮点现场 | `os/arch/src/riscv/cpu.rs`、`context/switch{32,64}.S`、跨AS trampoline与ArchTest；按目标ABI区分 | 当前Context只有整数callee-saved/ra/sp/tp，Echo不验证FP；两Task跨yield/park保持各自fs寄存器与fcsr，并检验跨AS调用/失败返回；U frame已有FP保存不能替代原生Task证明 |
| structured retained diagnostics | `reclaim/registry/export/query`既有对象 | ActiveCpu/inflight/alias/OOM/Kraw/DMA分别可观察；不新建通用资源图 |
| DMA驱动生命周期 | 现有`device/dma/virtio`，后续Phase6 | quiesce/reset有证据前保持Quarantine，不阻塞CPU-only；没有实现IOMMU或恢复框架 |

本阶段输出可以用于继续小补丁验收；不将这份CPU-only实现称作完整的全部生命周期/
故障隔离承诺。一般Graceful、精确保留核算、原生浮点现场和完整并发/OOM矩阵是明确未完成项。

浮点验收依据 [RISC-V psABI §1.3/§2.2](https://riscv-non-isa.github.io/riscv-elf-psabi-doc/)
（2026-10-10核对）：硬件浮点ABI对fs0–fs11有按ABI_FLEN保存的要求，fcsr有线程存储期。
结合当前只保存整数的Context，浮点跨Task支持仍有缺口；这不是整数Echo回归的已验证能力。
