# 路线图（roadmap.md）

> 本文件只保留**现状 / 下一阶段 / 未完成方向 / 依赖关系**。历史规划、逐阶段验收记录与阅读计划不再保留（Git 保存历史）。
> 设计契约以 `docs/architecture/` 为准；本文件是进度快照，会随时间变化。

## 0. 当前状态

已落地并端到端验证（RV64 + RV32 双 profile，QEMU）：

```text
Boot 全链：_start → FDT discovery → MachineInfo → core::init → Core Monitor（core> 交互 shell）
MMU：Sv39（identity + 高半区双映射 + hand-off）与 Sv32（identity）；
     KernelAddressSpace（Core 语义 ledger）+ Sv39PageTable/Sv32PageTable（buddy 回调分配页表页，
     mid-map 失败回滚，host 测试直驱生产实现）；boot 页表策略在 boot crate 的 vm/，arch 只留翻译机制
组件加载链（Linux insmod 教学版，语言无关）：
  .kcomp（ELF32/ELF64 ET_REL）→ make init.kpkg（cpio + manifest）→ .initpkg 内嵌
  → store（cpio 解析）→ loader（段放置 + RISC-V 重定位）→ registry（生命周期状态机）
  → monitor load；Rust 走 tools/build-kcomp.sh，freestanding C 走 tools/build-kcomp-c.sh
导出白名单（EXPORT_SYMBOL 教学版）：内存域视图 / 输出 / 机器与系统只读查询 / 组件生命周期 /
  任务 / 调度 / 设备认领 / IRQ route / DMA（alloc 与 map 分离，backing 撤销进 QUARANTINE）；
  错误码统一 0 / -Errno；组件只能调白名单，未导出符号 → UnresolvedSymbol，整次加载失败
Component Endpoint Registry：ContractId / InterfaceAbi（exact fingerprint）/ EndpointId（opaque，
  绝不重定向）；staged publish / lookup / discover / bind；组件间依赖只走 endpoint，无 flat ELF 符号表
调度执行链：load core_test → 加载 scheduler_rr → 发布 SchedulerPolicy → 任务创建/启动 →
  Core propose→validate→commit（RR 交替）→ yield/exit；core_test 端到端自检全 PASS
组件 panic containment（init / task 边界，协作式）：独立 Core 栈 + stack-switch 回 Core，
  标记 Failed 后重调度；panic=abort、无 unwinding、不承诺内存回收
受限 IsolatedNative：私有 AS + 双映射 assembly gateway + 按域放段 + Core 预置窗口 / 邮箱 +
  KernelNative → Isolated 跨域 service Gate + 失败 / 重启矩阵（RV64+RV32 QEMU 证明）；
  仍缺 ASID / U-mode / ecall / 出站 Isolated 调用 / 按域 import 解析（见 deployment.md §7/§10）
测试体系：make check（fmt/clippy/host 单测/构建）、make test-host、make test-qemu（boot smoke +
  core_test 判定）、make test-arch（ArchTest 白盒 selftest，每 case 独立 QEMU）
```

Core 的最小词汇表已立住（TaskId / PhysicalRange / ComponentId / DeviceId / ResourceDomain 视图 /
ExecutionDomain），策略一律 propose→validate→commit。

## 1. 下一阶段

按依赖顺序推进，前一项不成立后一项无从谈起：

```text
DeviceTable / device claim / IRQ / DMA → 第一个 Driver Component
                                       → QEMU RV32 M-mode NoMMU
                                       → 真实 MCU
```

- **第一个 Driver Component**：设备发现链 FDT/board description → DeviceRecord → `DeviceId`（identity）
  → `kcore_device_claim` → Driver Component → Device Interface → Service Component。
  `DeviceTable` 已含独占 owner、失败 quarantine 与 `revoke_owner(ComponentId)`；
  Endpoint Registry 已提供驱动 / 服务的绑定机制（见 `component-model.md` §3.2）。
- **任务化组件（C9）**：`kcomp_task` + 任务参数 + `kcomp_instance_destroy` / 卸载协议（逻辑层先行；
  物理回收仍不在本阶段）。
- **执行域（C10，进行中）**：在已落地的受限 IsolatedNative 上继续补 ASID / U-mode / `ecall` /
  出站 Isolated 调用 / 按域 import 解析；`SandboxedNative`（U-mode + 私有 AS）仍是未来强制边界
  （`todo!()` 占位）。IsolatedNative 是**可选实验、非承诺里程碑**；其缺口清单见
  `docs/architecture/deployment.md` §7/§10。
- **timer / 抢占（C5）**：SBI TIME + 时钟中断的 trap 可返回路径、Timer/Irq 原语与 sched seam 已就位，
  抢占逻辑（`sched::on_timer_tick`）待写；见 `docs/modules/core/timer.md`。

## 2. 尚未完成方向

> 这些是**方向，不是承诺的里程碑**；没有排期，只在具体需求出现时推进。

- **运行期组件热插拔 / Runtime Graph / 依赖解析器**：`.kcomp` loader 与 `init.kpkg` 已落地，
  运行期图与依赖解算未做。
- **Wasm 执行后端**：`scheduler.wasm` / `filesystem.wasm` 等作为 Component 的一种执行方式；
  Core/Arch 保持 native Rust。执行模型与执行域正交（见 `deployment.md` §3）。
- **IPC / 隔离 / 多 profile**：UserAddressSpace 执行域、微内核形态、`game` / `unix`（POSIX
  personality）/ `micro` / `debug` 等 profile。
- **多架构**：x86_64 → aarch64 → loongarch64（Arch 换实现，Core 不动）。
- **热替换**：在 `quiesce → stop → unbind → reset → replace → bind → start`（短暂中断可接受）
  基础上向无感替换演进；不做 live state migration。
- **内存回收（未来里程碑）**：完整 buddy、通用 Core heap、完整 panic recovery（内存回收 / 真隔离）
  均推迟；phase 1 只做资源归属撤销与 quarantine，**不承诺共享堆字节回收**，也不承诺对抗隔离。
- **验证工具链 / 确定性测试**：Kani / Loom / Miri / Verus 与 Test Scheduler / Hunt Mode。
- **第三方库移植 / 调包能力**：第三条核心能力，方向与候选地图见 `docs/architecture/porting.md`
  （FatFs 只读、littlefs 已落地；其余为候选，非集成）。

明确不做（本阶段，务必遵守）：

```text
✗ 真正动态加载          ✗ 真正 hot migration
✗ 复杂 IPC              ✗ 微内核模式
✗ Wasm runtime          ✗ WIT / IDL
✗ 完整 capability 系统  ✗ 完整 POSIX
✗ Linux syscall 兼容    ✗ 复杂 VFS
✗ 复杂 SMP scheduler    ✗ 形式化证明
✗ 完整 driver framework ✗ 完整 dependency solver
```

## 3. 依赖关系

里程碑依赖链（未完成项按依赖排序）：

```text
P0 地基：Sv39/Sv32 启动、启动地址去硬编码        —— 已完成
P1 任务系统：context_switch 实机验证、调度执行链   —— 已完成
P2 中断/驱动：timer / 抢占（C5）、设备·IRQ·DMA（C6）→ 第一个 driver   —— 部分
P3 组件化进阶：区域分配（C7）、域视图交付（C8）、任务化组件（C9）        —— 部分
P4 执行域/隔离（C10）：受限 IsolatedNative 已落地；ASID / U-mode / ecall / 出站
        Isolated / 按域 import / SandboxedNative 未完成
```

- **物理帧分配是 Core 内部机制**，不是策略流；`MemoryPolicy`（未来）只能提议偏好，
  最终选择 / 验证 / 提交仍在 Core。
- **执行域定位（D2=A）**：`KernelNative`（S + 共享 AS）是常态、长期模式，就是可信代码
  （无硬件访问强制，撤销协作式）；`IsolatedNative`（S + 私有 AS）只做条件性故障隔离；
  `SandboxedNative`（U + 私有 AS）才是未来硬件强制边界。细节见 `driver-model.md`。
- **`Registry::unload()` 绝不能直接变成物理释放原语**：缺少"停止新工作 → 排空 IRQ/回调 →
  确认零活跃执行"的协调协议时不得释放内存（phase 1 不承诺物理回收）。
