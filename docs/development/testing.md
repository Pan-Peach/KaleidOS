# 测试策略（testing.md）

> 相关：`docs/development/benchmark.md`（性能）、`docs/architecture/deployment.md` §9（验证设计）、`os/components/tests/core_test/`（CoreTest）。

普通应用的 libc / 双平台兼容性测试使用上游 libc-test，见
[`docs/development/compat-testing.md`](compat-testing.md)。当前只做宿主参考构建 / 运行与
组合包。RV64 普通 ELF 的执行 / fork / exec / wait 机制由 CoreTest 的 exec 分组验证；
当前上游 libc-test 尚未在 KaleidOS 通过，见 [userspace.md](userspace.md)。

## 1. 测试哲学

> **Core 中所有与硬件无关的 truth logic 必须 host-testable；Core 与 Arch / Hardware 的真实契约通过 QEMU / CoreTest / 真机验证。**

如果某段 Core 逻辑只能通过启动整个 OS 来测试，第一反应应该是：**它是不是和 Arch 耦合得太深了？** 正确姿势是把与硬件无关的 truth logic（任务状态机、所有权、设备·IRQ·DMA 归属）做成纯逻辑，在宿主上直接 `cargo test`。

测试按证明对象分工：host 验证纯逻辑，property 是 host 中的一种方法；
CoreTest 验证公开组件接口的集成契约，ArchTest 验证实际硬件行为，真机提供最终硬件证据。
默认入口 `make test` 聚合这些后端；构建、fixture 与 runner 的整理记录见
[cleanup](cleanup.md)。

（Concurrency Exploration / Model Checking 属未来工具链。）

## 2. 职责：Host / CoreTest / ArchTest

| 层 | 职责 | 手段 |
|---|---|---|
| **Host Test** | Core 与硬件无关的 truth logic：帧 / 区域所有权、任务状态机、设备·IRQ·DMA 归属、组件生命周期、地址空间语义、策略验证、ELF/parser；RISC-V 纯算法（重定位、Sv32/Sv39 页表编码与 walk）host 测**生产实现** | `cargo test`（`make test-host`） |
| **Property Test** | Core 不变式的随机化验证（proptest，dev-dependency、仅 host）：AddressSpace 随机序列四不变式、parser never-panic、生命周期状态机 | host |
| **CoreTest** | Core 与 Arch / Machine Discovery 的真实契约，以普通 `.kcomp` 身份运行（无 god-mode）；也是**唯一**的组件 / 系统集成测试编排者：用真实组件 + SDK client 复现 block / filesystem / prober / c-smoke 场景 | `make test-qemu` |
| **ArchTest** | feature-gated test kernel，跑在**完整 `core::init` + runtime VM 之后**（device MMIO 已映射），直接验证 Arch/HAL 与真实 CPU/设备的契约：trap/scause、页表权限生效（RO/NX/未映射 fault）、context switch 寄存器保存、timer 与外部中断实际投递；每 case 单独 QEMU | `make test-arch` |
| **Real Hardware** | 最终真机验证 | —— |

边界：

- **CoreTest 无 god-mode**：只走 `kcore_*` 白名单；**不得**修改 Core 私有状态。CoreTest 只断言 Core 自己报告的返回值 / 状态编码 / trace 事件。
- **平台白盒事实属于 ArchTest**（QEMU virt 的 PLIC 线号、S-mode context 公式、控制器 enable bit 布局与读回）——同一事实只在一个层次证明。
- **host fake 上下文后端不执行组件入口体**，只覆盖边界记账；任何"已隔离 / 已跨域"的结论必须由 QEMU / 真机的真实页表与特权级切换证明。
- Trace 断言强度是**"操作 → 事件"**：操作前取游标（`kcore_trace_stats.next_seq`）→ 执行**一个**受控 Core 操作 → 用该操作返回的 id 精确匹配载荷；不用"有类似事件"。

## 3. 什么值得测试

- 每个 Core 新功能：**先写 host test，再写实现**（至少同 PR 提交）。
- **对抗性测试与功能测试同等重要**：Core 的"拒绝错误提案"行为必须显式测试。
  - 目标类别：double free / wrong owner / stale 资源身份 / invalid task transition / duplicate claim / illegal map / invalid scheduler proposal；
  - 已落地子集：重复 `device_claim` → `-EBUSY`、quarantine 后再 claim → `-EBUSY`、仍有 live IRQ route / DMA mapping 时 `device_release` → `-EBUSY`、非 owner `irq_register` → `-EACCES`、顺序错误 → `-EINVAL`、ordinal 越界 → `-ENOENT`、stale mapping id → `-ENOENT`。
- **同一 artifact 多组件的独立 backing（关键不变量）**：host `same_artifact_loads_produce_independent_components`（同名 artifact 连续 load 两次 → 两个 `ComponentId`、独立常驻 backing、镜像区间不重叠、`.data` / `.bss` 不共享）；CoreTest `driver-multi-device`（prober 自动为所有块设备创建独立 `virtio_blk`；含无签名盘、重复认领拒绝与 RV32 LBA 溢出，RV64+RV32）；ArchTest `isolated-restart`（同 artifact 的并发 Isolated 组件各自独立私有 AS / backing）。
- Core 的 API 每多一个，就多一份必须验证的承诺——这反过来约束 Core 词汇表保持最小。

双盘→双 FS→VFS→POSIX 纵向负载、已验收部分与剩余前置见
[服务研究 §4](service-runtime-study.md#4-真实纵向负载与前置)，推进状态见
[STATUS §6](../../STATUS.md#6-近期依赖链)。driver-multi-device 现验证自动全枚举，storage-real-chain 把真实两盘分别交给 FatFs
和 littlefs 并验证内容、错误路径与旧句柄；这仍不是 VFS 或应用运行期文件访问。
`tests/build/test_filesystem_provider.py` 编译生产 C provider 与上游 FS 库，在宿主
块介质 fake 内强制停住读 I/O，验证并发 close/mount 返回 EBUSY；同时验证旧句柄、
零长度读、句柄耗尽以及 C SDK 普通数据缓冲前端。FatFs 的节点场景经 C SDK
生成 IPC client/dispatch 经 host transport mock 调用真实 backend / FatFs 库，覆盖嵌套目录、大小写 token 复用、
缺失与非法名字、非目录/过期 parent、节点表耗尽和重新挂载失效；SDK host
测试另验证 generated C/Rust wire 编码；C Block facade 覆盖多块拆分、LBA 溢出和部分完成。它证明库/状态串行纪律，不声称
硬件隔离或 DMA 撤销。源码推导和报告推演不得登记为 QEMU PASS。

## 4. 如何运行与观察

```text
make check        质量快车道：fmt + clippy -D warnings + host 单测 + RV64 构建 + RV32 check
make test-host    宿主单测
make test-qemu    CoreTest + ksh 串口流程 + init 启动流程 + shutdown
make test-init    RV64/RV32：FAT、双盘 FAT 根、无盘会话、坏盘 monitor 回退
make test-arch-smp-rv64  RV64：CPU 启动 / IPI / per-CPU（组件调度由 test-qemu 中的 CoreTest 验证）
make test-arch    ArchTest 白盒 selftest（每 case 独立 QEMU，精确 scause 判定）
```

`make test` 聚合 `test-host`、`test-qemu`、`test-arch`。
`test-arch` 已包含 RV64 三个 SMP 硬件用例；单独运行
`test-arch-smp-rv64` 只跑这三个，不重复基础套件。CI 使用同样的子入口。
`test-host` 包含 Kconfig、ABI generator、compat runner、构建归属与 QEMU harness 自测。
Linux/Windows 宿主兼容性参考 suite 仍单独选择。

测试 profile 各自使用 `build/tests/<suite>-<arch>/`，不修改用户 `.config`；
日志位于对应目录的 `logs/`。每个 guest 有唯一临时盘目录，退出时清理并保留串口日志。
QEMU 可执行名、平台参数和默认内存只由 genmk 提供；OOM 的 128 MiB 是显式场景差异。

只跑一个硬件用例或列出用例：

```sh
make _test-arch-rv64 TEST_CASE=timer
make test-arch-smp-rv64 TEST_CASE=smp-ipi
# 在已构建的 ArchTest profile 上列出基础用例
make O=build/tests/archtest-rv64 _test-arch-list
```

普通 `cargo test -p kernel --lib` 只测逻辑，不隐式构建组件。
`make test-host` 显式准备真实工件并启用 `kernel/test-fixtures`，覆盖 parser/loader/relocation。
准备步骤见 [构建指南](building.md)。

CoreTest 使用带 `[core-test] ` 前缀的 KTAP：header、连续编号与稳定名称、尾部 `1..N`
计划及最终 PASS。runner 不另维护一份用例名清单；空计划、缺项、重复项、失败、
任意 SKIP、超时与异常退出均不能算通过。RV64 默认要求两 CPU，不足时报告失败。

观察面：结构化 trace ring（`TraceEvent`，固定容量、无分配、`seq` 单调；`kcore_trace_stats` 只读）。host 测试的 ring 是**线程本地替身**，不覆盖生产的锁 / 并发语义（SMP 行为由 QEMU / 真机承担）。

## 5. 新增功能测试纪律

- 新 Core 功能：host test + 实现同 PR；跨组件 / 系统集成场景进 CoreTest；平台白盒事实进 ArchTest。
- CoreTest 只走真实 Core API；host fake 上下文后端不算跨域证明。

`tests/qemu/ksh.py` 在 CoreTest 完成后提交真实串口命令，检查输入 / 输出、加载失败后的
会话存活、引号 / 转义、行编辑 / 历史 / 补全、cat 与 exit 回到 monitor。这是 shell 用户流程 smoke；driver / FS / scheduler
的细粒度集成编排仍由 CoreTest 负责。两种硬件 topology 都走这条流程。

设备观察分层验证：Core host 覆盖主 PIO / 多资源计数、合法零 ID / 零 IRQ、未映射 IRQ、
认领 / 释放 / quarantine 的值投影及错误时输出不变；SDK host 检查复制值是否合法。
CoreTest 从公开 API 验证认领后同一设备的窗口 / owner、释放后状态、短缓冲和不存在 ID；
串口 smoke 检查 compatible / IRQ / owner 的可读显示，普通 init 流程另验证 `virtio_blk` 认领者。

CoreTest 的私有 profile 叠加 `configs/coretest.fragment`，避免默认 init 提前认领设备。
`tests/qemu/init_runner.py` 另用普通 board profile 检查 boot → init → FAT root → ksh
的用户流程，以及无盘 / 坏盘分支；含真实 FAT 路径的引号 / 转义读取和引号 exec
参数。使用独立磁盘副本，未把断言塞入生产 init。
RV64 另在全新 128 MiB guest 中运行 `exec_probe/oom.S`：耗尽 `brk` backing 后，
`mprotect` 必须返回 `ENOMEM`、保留原有可写映射；程序正常退出后，再次装载组件
须返回 `ENOMEM`，shell 的引号 echo、历史回忆 / 查询、设备表与退出继续响应。
该探针不进入普通 exec corpus 或 Linux 参考测试，避免把硬件内存压力带入其它用例。

- 同一事实只在一个层次证明，避免三层重复断言。

## 6. Task park/unpark 契约测试

`TaskTable` 有 active `proptest!`，随机生成 unpark、错误 owner、consume 序列并与一位 permit 模型比对。host 调度集成测试检查提前 unpark 快速路径、阻塞唤醒、任务表锁释放和 IRQ 恢复。

CoreTest 还从普通组件 ABI 侧覆盖同一契约：启动前重复 unpark、permit 只消费一次，以及两个组件任务完成 64 轮“park → 查询 Blocked → unpark → 重新调度”。该场景位于精确 `sched-trace` 检查之后，不污染其事件窗口。

```sh
cargo test -p kernel --lib park -- --test-threads=1
cargo test -p kernel --lib irq::tests::nested_irq_save_guards_restore_the_outer_state -- --exact --test-threads=1
make test-qemu
```

host 调度集成用例失败时会 fail-fast，并用 RAII 清理全局任务/调度状态；CoreTest 的调度调用返回后，失败路径会尽力唤醒并跑完自身任务，避免污染后续场景。

## 7. 长期陷阱与工具

- **IRQ 电平触发**：`external-irq` 用 UART **THRE** 拉线；**先 claim 再关设备源**——先关 `IER` 会让 PLIC pending 随电平撤销，claim 取到 0。
- **身份模型限制**：`ComponentId` 只在单个 `Registry` 实例内唯一，trace ring 是进程 / 整机全局；断言锚在"本组件刚加载的 `ComponentId` + 该次 load 前的游标"，这是当前身份模型允许的最强形式（全局唯一 ComponentId / boot epoch 未做）。
- **DMA 归属**：`kcore_dma_alloc` 是 device-agnostic；`kcore_dma_map` 当前按 device owner 记账，受信 Native Direct 路径不检查 ambient caller；unmap 无 caller 校验，普通借入 buffer 未 pin。详见 driver-model §6.3；不得声称已经验证私有域 DMA 权限。**未决**：组件可 claim PLIC 等设备（"认领一台设备 = 拿到它的全部语义，含控制其他设备的中断线"），该边界问题无人回答，记录而非"修"。
- 未来工具链：CHESS / Test Scheduler / Hunt Mode（确定性并发）、Kani / Loom / Miri / Verus（模型检查 / UB / 演绎验证）、FSCQ（FS 崩溃一致性）。见 `references.md`。

## SMP 组件执行验证

`make test-qemu` 的 RV64 场景运行双 CPU CoreTest。`core_test/runtime/smp.rs` 编排并创建普通任务，只通过公开 `kcore_*` ABI 验证：两 CPU 不 yield 的 rendezvous 证明物理并行；每 CPU 两任务验证本地 RR 进展；128 轮跨 CPU park/unpark 验证唤醒与固定归属；所有任务最终 Exited。CPU1 与 CPU0 的 panic 用例各加载一个独立 `kcomp_smp`，该 fixture 只负责故意 panic；CoreTest 在另一个 CPU 上继续工作，通过任务状态与指定 ComponentId 的 Failed trace 验证结果。共享控制窗口来自 CoreTest 实例分配，跨镜像仅传 C 布局标量与 u32 窗口，并全部使用原子访问。

`make test-arch-smp-rv64` 只负责启动 / IPI / per-CPU 三个硬件契约用例；CI 的 ArchTest job 也运行此门禁。组件调度集成的唯一编排者仍是 CoreTest，ArchTest 不读取私有表来替代公开 ABI 的集成验证。

host 对提交竞争、park 快速检查后的远端通知、固定 CPU、并发 policy 栈占用与每 CPU 身份分别做确定性测试。QEMU 不替代这些状态机验证，也不证明真实硬件长期稳定性。

## 部署场景的归属

RV64 CoreTest 的 `runtime/deployment.rs` 通过公开 load、ComponentInfo、Endpoint 与 trace
验证 K/I 堆后端、同一工件多实例、拒绝 Sandboxed 部署、K/K 与 K/I 调用，
以及通过普通 Isolated fixture 的 block 服务入口执行 I/K、I/I、I→K→I、重入拒绝、
provider panic、失败状态、新实例与过期绑定。测试控制属于 test-only fixture，
没有增加 Core ABI 或修改 Core 私有状态。

ArchTest 的 isolated 用例保留私有 AS、实际 backing、satp 恢复、访问权限、
销毁故障与回收现场的证据。当前公开 create 固定 KernelNative，load 只支持默认配置，
已有 `kcore_component_stop`；需要指定 Isolated config 或检查私有域销毁现场的用例仍由 ArchTest 承担。
这些约束不能靠测试后门绕过。

## Component 形态与 Stop 准入实验

CoreTest `runtime/convergence.rs` 编排 test-only `kcomp_checksum`，同一 artifact / create
入口的 config 选择 Passive、Active、Hybrid，所有实例仍使用 ComponentId 与普通 Task。
两个 Passive 实例不创建任务；Active / Hybrid 各拥有一个 Worker。请求 / 回复是组件
私有单槽协议，不经 Core 业务队列；所有共享 C-layout u32 只用原子访问。
每种 Worker 完成 64 次回复，Hybrid 同时执行 64 次 Direct checksum；RV64 跨 CPU，
RV32 单 CPU 协作运行。Consumer 错误 unpark provider-owned task 必须 EACCES。

同一 fixture 的 Gate-only 模式没有 Worker / Direct 表。RV64 CPU1 在 consumer Task
内执行 Gate，CPU0 Stop 返回 EBUSY 且仍 Ready；返回后 Stop 成功，旧 endpoint 拒绝，
重新创建获得新身份。host 的 128 轮 begin_call/begin_stop 竞争只证明锁和状态机，
QEMU 场景才证明真实双 CPU 在途调用。命令为 `make test-qemu`。

同一 fixture 的 LifecycleProbe 模式由 consumer Task 创建并停止。Init / Exit 调用公开
`task_yield`、`task_park`、`task_exit` 均返回 EINVAL；随后 endpoint 发布与销毁成功，
验证真实临时栈返回后 principal 仍正确。该模式无 Worker、无 Direct 表。

Worker 使用已有 yield，错误路径有有限超时；没有跨 owner 的 wake、通用取消、Task
join 或私有域 Worker 能力。这不是生产 RPC runtime。Direct 发布后 ctx 保留，停止
Worker 不代表销毁实例；完整证据与未通过路径见 [收敛审计](core-convergence.md)。

## 通信 Cleanup 门禁（2026-10-09）

现有Echo/混合VFS/真实Fat/virtio主链不要重复实现；每次迁移按
[专项审计](component-communication-audit.md#6-本轮真实门禁与已定位回归)记录准确HEAD、
profile与失败，CoreTest分组通过不等于整套通过，旧PASS不覆盖当前失败。

`python3 -m unittest discover -s tests/build -p test_ipc_codec.py -v` 编译实际C/Rust
SDK envelope，验证独立LE golden、长度与错误层次；Rust decoder 上界已修补，4项全部通过。
`test_kabi_methods.py` 验证生成 client/dispatch，7项通过（C启用遇错终止的UBSan，含VFS嵌套codec与domain回复）；均纳入test-tools，
不证明AS隔离。现行语法见 [方法生成](kabi-methods.md)。
SDK test-only IPC链接替身返回ENOTSUP，只恢复旧前端单测，不模拟Core收发。

I域普通Gate对照在真实Task/IPC/copy替代通过前保留；先核对工件UNDEF，不能因为源代码
未执行IPC分支就认为没有IPC import。NoMMU、MMU与Sandbox分别报告；Block吞吐目前未测。

## Runtime 生命周期与回收验证

场景矩阵、K/I/U通信格、逐层职责和本轮真实命令结果见
[Runtime 第一轮交付 §4–§5](component-runtime-consolidation.md#4-lifecycle-test-matrix)。
`exchange/tests.rs` 新增server exit后的成功回复保留、caller/server/reply六种顺序中另一
实例不受影响，以及1000轮Endpoint/receipt槽位退役。它们只证明host传输真相，
不执行component入口、不证明私有AS、CPU强制停止或物理页回收。

真回收必须使用CPU-only组件：热身后基线→load→真实Task/IPC→stop/fault→CPU离场
确认→destroy/force→AS/backing teardown→物理基线。逐项记录Task/Endpoint逻辑数量、
image/stack/heap extent、页表页与可用物理页、预期tombstone/缓存和Quarantine原因。
先100轮，再数百与1000轮；现实现不满足条件时列为待验收，不能让host槽位压力替代。

已新增真实 private 生命周期压力：CoreTest RV64 I/U 交替合计1000轮，RV32 I1000轮，
每轮真 Task/Endpoint/生成 IPC 后 stop/fault/force、显式 reclaim，比较 Task 数与
物理页。U 无 yield 忙循环以及远端 CPU1 的 timer 停止另行验证。runner 的 report
预算为300秒，场景 Task 参数使用持久 backing，超时不释放尚可能使用的参数。
NoMMU 的 public load 能力前置拒绝表示 private 场景不适用，不计隔离PASS。
最新结果、精确保留核算缺口见 [报告 §7](component-runtime-consolidation.md#7-授权后的生产实现与验证)。
