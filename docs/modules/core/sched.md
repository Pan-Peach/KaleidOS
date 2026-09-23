# sched（os/core/src/sched.rs）

> **每 CPU 调度真相 + propose→validate→commit 路径**：Core 提供机制，策略组件只"提议"下一个任务；Core 验证存在 / Runnable / 未在别 CPU 后提交。
> 这是 "Policy proposes, Core validates and commits" 的规范实现。
> 调度策略走**专用执行路径**（`PolicyCall` 边界，Core 是 caller），不是通用 service call。

## owns 什么真相

- 每 CPU 调度状态：`CpuState { anchor, current }`（当前任务锚点）。
- **调度策略配置**（`PolicySlot`）：选中的 `EndpointId` + 为策略执行准备的 Core 栈 + `retired` 标志。`endpoint == None` = 从未配置（`NoPolicy`）；已安装策略失败 / 失效后 Core 用确定性回退继续调度，**不**退化成 `NoPolicy`。
- 从"提议"到"提交"的最终裁决：验证 task 存在 / Runnable / 不在别的 CPU；提交后记录 trace。
- SchedulerPolicy 的契约身份来自 `abi/scheduler.toml`（生成常量）：name `scheduler.policy` / ABI fingerprint / contract id / `CHOOSE_NEXT` wire 格式。

## 暴露什么机制

- `set_policy(EndpointId)`（导出 `kcore_sched_set_policy`）：校验活 endpoint + contract + abi + provider 有 `kcomp_service_dispatch`，**只提交 EndpointId**（不发布）。IRQ / service-call / policy 执行内拒绝。
- `select_provider(ComponentId)`：组合辅助（monitor / ArchTest）——显式 discover + select。
- `SchedError`（含 `PolicyEndpoint` / `NoDispatcher` / `NoPolicyStack`）。
- `init()`、`current_task()`。
- `run()`：从锚点进入调度循环。
- `yield_current()` / `exit_current()`。
- `on_timer_tick()`（`todo!`，抢占未落地）、`abort_current_task()`。

## 明确不做

- **不实现任何调度算法**：RR / CFS 在 `scheduler_rr` 等组件里。
- **没有内建 / 兜底调度器**：从未选择过策略时返回 `NoPolicy`；已安装策略失败后的确定性回退（id 序首项，提交前验证 owner）只是"Core 不被坏组件挂起"的机制，不是算法。
- **不按名字发现调度器**：组合方显式 `(provider, port_name, contract)` 发现 + select；Core 不持有全局名字。
- 拒绝在 IRQ / service-call / policy 执行上下文 `run` / `yield` / `exit` / `set_policy`。
- 不持有任务 handle：策略只收候选 `TaskId` 并提议。
- 通用 `kcore_endpoint_call` 拒绝 `scheduler.policy` 契约（保留契约）。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/sched.rs` | `PolicySlot`、`set_policy` / `select_provider`、`CpuState`、`run` / `yield_current` / `exit_current`、`pick_next` 验证与提交 |
| `os/core/src/component/call.rs` | `PolicyTarget`、`prepare_policy` / `call_policy`（锁内准备 + 无锁调用） |
| `os/core/src/component/containment.rs` | `EscapeKind::PolicyCall`、`call_component_policy`（专用执行边界） |
| `abi/scheduler.toml` | `scheduler.policy` 契约常量 + `CHOOSE_NEXT` wire 格式（单一来源） |
