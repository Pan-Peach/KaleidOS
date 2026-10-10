# 调度与 SMP 的职责契约

> Core owns truth. Policy proposes, Core validates and commits.
> 本文规定调度边界；ABI 数值与 wire 的唯一来源是 `abi/core.toml`、`abi/scheduler.toml`。

## 1. 职责划分

| 内容 | 所有者 | 边界 |
|---|---|---|
| TaskId、owner、生命周期、固定 CPU、Running(cpu) | Core | 组件不能修改任务表或伪造当前执行身份 |
| CPU 拓扑、Online、启动门控、IPI pending | Core | Arch 实现物理启动、门铃和应答；boot 交接入口、栈与页表 |
| 任务栈、寄存器、锚点、IRQ 状态、panic 返回现场 | Core | 每 CPU 执行边界独立；切换前释放全部锁 |
| 本 CPU 的合法候选集合 | Core | 只读快照：Runnable、CPU 匹配、owner 存活 |
| RR 游标、优先级、公平性、私有队列 | Scheduler Component | 从快照提议 TaskId；私有队列不成为存在性或状态真相 |
| 创建多少任务、初始放在哪颗 CPU | 任务所属组件 / 上层组合策略 | 提出请求；Core 校验 owner、状态与目标 Online |
| 普通业务等待条件、队列、通知语义 | 使用任务的组件 | Core提供park/unpark与pending permit；Endpoint IPC的transport谓词/等待者另由[Exchange](ipc.md)拥有 |
| 策略 endpoint 的发现与选择 | 组合方 | 显式选择 Ready provider；调度路径不按名字自动发现 |

Core 的候选扫描是机制。RR、负载均衡、work stealing、公平性规则属于策略。

## 2. 初始 CPU 归属

- `kcore_task_start(id)` 请求当前逻辑 CPU。
- `kcore_task_start_on(id, cpu)` 请求指定逻辑 CPU。caller 必须拥有 Created 任务，owner 为 Starting / Ready，目标已 Online；拒绝时状态和 CPU 归属不变。
- 成功原子提交 Runnable 与固定 CPU 归属；首次运行、yield 后重入、unpark 后恢复均由这颗 CPU 执行。
- `kcore_cpu_current()` 返回当前逻辑 CpuId；不是硬件 hartid 或权限。
- 成功表示 work 已发布，不保证任务已经执行。远端 CPU 可以在组件 create 返回前运行已启动的任务：组件必须先初始化并发布任务需要的状态，再 start。

固定 CPU 防止离场任务刚提交 Runnable、寄存器尚未保存时，被另一 CPU 取走上下文。迁移需要保存完成与所有权交接机制，不能只改 affinity 字段。

## 3. 调度提交

```text
Core 读取本 CPU 候选 → scheduler.policy 提议 TaskId
  → Core 持 registry → CPU state → task table 锁复验
  → 同一次事务提交 outgoing 状态与 incoming Running(cpu)
  → 释放全部锁 → 安装 incoming 身份 / panic 边界 → 切栈
  → 在 incoming 栈上恢复该执行流自己的 IRQ 状态
```

复验涵盖存在性、owner 存活、Runnable、CPU 归属和 outgoing 确实 Running 在当前 CPU。快照候选因并发失效时重新取快照，不算策略违约。提议根本不在传入快照内、返回非零或 panic，才使 provider 失败并退役。

`PolicyProposal` 记录提议，`PolicyAccepted` 在事务成功后记录接受，`TaskSwitch` 记录切换。无候选时回本 CPU 锚点。已选择的策略失败时使用既有确定性回退维持运行；从未选择策略仍返回 NoPolicy。

## 4. 策略执行

CHOOSE_NEXT 的 args 为 `current TaskId + CpuId`，各 u32 LE；input 是本 CPU 候选的 u32 LE 列表；output 是一个 u32 LE TaskId。ABI fingerprint 已随 CPU 字段原地更换，旧 `.kcomp` 必须协调重建。

同一个活动 policy 的回调串行执行，复用一张 Core-owned policy 栈。Core 以短锁认领 / 归还栈，回调期间不持 Core 锁；别的 CPU 等待回调结束。替换正在执行的 policy 返回 EBUSY。组件任务仍在不同 CPU 上并行运行。

回调必须有界、不可阻塞或分配；不能 yield、创建 / 启动任务、嵌套创建组件或调用普通服务。`scheduler_rr` 按 CPU 保存独立游标，Core 不解释这些游标。KernelNative 是受信部署，Core 不承诺容纳挂死或任意写内存的策略。

## 5. park / unpark 与空闲 CPU

- 每任务最多一份 permit，多次提前通知合并。park 有早期快速检查；最终 permit 检查和 Running → Blocked 必须在同一次 task table 事务内，覆盖远端 CPU 在策略执行期间通知的情况。
- unpark 校验 caller 是 owner。Blocked → Runnable 提交后通知固定 CPU；尚未 Blocked 时记 permit。等待条件与循环重查属于组件。
- start、远端 wake、安装策略触发 Reschedule IPI；handler 只应答和标记 pending，调度在 Core 安全点进行。
- AP 在空闲循环运行本 CPU 候选；BSP 在 monitor / console 安全点服务本地工作。monitor 读串口前先服务 Runnable 任务，避免在组件 console 任务 yield 回锚点后抢读其输入；未配置策略时保留 monitor 初始组合入口。检查工作与休眠在本地关 IRQ 下衔接，保留 pending 门铃，避免丢失唤醒。忙任务仍需主动 yield / park / exit。
- Init / Exit 临时栈及其嵌套边界拒绝 yield / park / exit；Init 创建、启动 Worker 与启动锚点 sched_run 保留。边界规则见 [组件生命周期 §7](component-lifecycle.md#7-资源归属-identity-规则重要陷阱)。
- task table 与 registry 的共享锁在获取前关闭本地 IRQ，释放后恢复。组件创建与停止以同一 registry → task 顺序完成 admission，停止不能漏掉并发创建的任务。

## 6. 失败边界与当前范围

执行 guard、创建身份、Core ABI 深度、task-abort 栈 / 上下文、跨 AS 返回链均按 CPU 保存。一颗 CPU 的组件 panic 不得修改另一颗 CPU 的执行边界。

KernelNative 失败是协作式逻辑死亡、物理驻留，不构成内存隔离。失败 owner 不再进入候选；已在另一 CPU 运行的任务在下一次调度边界停止，不承诺立即中断不让出的执行流。生命周期与隔离契约见 `docs/architecture/component-lifecycle.md`、`docs/architecture/deployment.md`。

当前范围是 RV64 KernelNative 的协作式 SMP；RV32 单核回归继续覆盖。普通用户
程序的 private AS 在每次 U-mode step 激活，trap 后先恢复同一 task 的内核栈 / kernel
satp，再调度 personality 任务，调度器不在用户 AS 或 per-CPU trap 栈切换任务。
personality 提议执行 deadline，Core 保留更早的已有 deadline 并交 timer 验证；
到期返回 task，由 personality 决定 yield。这不改变 KernelNative 组件任务的协作式
契约。通用内核抢占、迁移、work stealing、CPU hotplug 和第二 ISA 调度另行推进。
验证入口见 `docs/development/testing.md`。

## 7. Runtime 停止与私有 AS

本节 Task/AS 接线与离场确认已经实现；Graceful drain 已实现；S-mode 抢占与远端 shootdown 仍是目标。
Task 的 owner 仍是 ComponentId；私有组件 Task 关联 ComponentRecord 中既有 AS，
Task/AS 生命周期提交复验 owner、domain、AS handle 与 CPU。不复制实例表，不把
personality PID 带进 Core。最小初期固定 CPU；I 私有业务栈逐 Task 分配，Core kernel
stack/context 保持 Core root 下有效。调度前后 root、栈、trap、IRQ flags 与身份必须
一致恢复；只在 RiscvContext 添加 satp 字段或开放 import 不构成接通。

停止需要两个不同谓词：新 work 准入关闭；Graceful 期间已有 Task 仍能为清理而恢复。
may_run 保持 Starting/Ready 的新资源准入；may_execute 允许已有 Stopping Task
恢复清理。stop 通知唤醒 Blocked 并保留提前 park permit，worker 用真实当前 Task
停止查询协作退出。Force/Failed 禁止返回组件业务代码。

`commit_switch` 先提交 Exited/current，再在锁外 context_switch；CPU 此时可能仍写
旧 context、使用旧栈。已增加 per-CPU departing 与 Task.execution_retired，incoming stack 在
finish_switch 完成保存与 root 恢复后提交确认。Reaper 在确认后移除栈/context；
绝不在 Task exit hook 或远端观察 Exited 时立刻 Drop。跨 CPU 读写用同一同步纪律。
Created/Runnable/Blocked 的终结也要排除正在交接的 context。

SMP Force 请求可复用 IPI pending，但 IPI 送达/应答不能代替 task 离场应答；确认必须
绑定目标 Component/Task 与本次停止请求，避免迟到应答属于旧执行。确认后不得再次
调度目标 root；页表更新还须完成涉及 CPU 的 TLB invalidation。无迁移的初期可通过
固定 CPU + Core root 返回时全量 sfence 限定证明范围；无法排空别的活 root 时不复用。

K/I S-mode 内核抢占尚无实现，禁 IRQ/持锁/破坏 Core 的执行仍不能安全强杀。
RV64 U runner 已复用普通 arch UserContext 的 timer/trap，在
Core root / 自己的 Task 栈检查 stop 并禁止下一次 sret；默认 slice 为 10ms。Core ABI 执行区不能
任意丢弃持锁栈；trap handler 不运行 Scheduler。回收必须等 copy/API 返回引用也排空。

原生 K/I Task 的 `RiscvContext` 与 `context/switch{32,64}.S` 当前仅保存整数
callee-saved、ra/sp/tp，未保存浮点寄存器或 fcsr。当前 Echo 只验证整数/字节业务；
不能据此保证浮点业务跨 yield/park 恢复正确。补齐原生上下文与真实 FP ArchTest
是独立任务；U frame 的既有 FP 保存不自动补齐 K/I Task 上下文。

有限等待、destroy 与错误语义以 [生命周期 §11](component-lifecycle.md#11-runtime-完整化当前与目标)
为准；实际文件任务见 [Runtime 审计](../development/component-runtime-consolidation.md)。
