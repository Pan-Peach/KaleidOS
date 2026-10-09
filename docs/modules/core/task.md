# task（os/core/src/task/）

> 任务**身份与生命周期真相**：谁存在、属于谁、处于什么状态、跑在哪个 CPU、用哪个内核栈、上下文在哪。
> 这里的"上下文"是执行状态；**调度策略**（下一个跑谁）不在这里。

## owns 什么真相

- Task 身份（`TaskId`）与 owner（`ComponentId`）。
- 任务状态机（`TaskState`：Created / Runnable / Running(CpuId) / Blocked / Exited）。
- 内核栈（`Kernelstack`）、任务执行上下文与该执行流的 IRQ 保存值。
- 当前每个 Task 栈固定 16 KiB（四个分配 granule），容纳有界 IPC 副本与 Core 切换帧；无 guard page 或逐任务可调栈。
- start 时提交的固定逻辑 CPU，首次运行和 wake 后均受 Core 验证。见 `docs/architecture/scheduling.md`。
- 每个任务最多一份 pending unpark permit（提前通知会被下一次 park 消费；重复通知合并）。
- RV64 普通用户任务的私有 AS、U 映射、整数 / FP 现场与实际 trap 关联；PID / ELF / syscall 留在 personality。
- 任务归属：`TaskRecord.owner`（组件停止 / 失败时按 owner 裁决，失败后 backing 保留驻留）。

## 暴露什么机制

- `create_task(requester, entry, arg)` / `start_task(requester, task)` / `start_task_on(requester, task, cpu)`：语义入口，只接受 Core 导出的白名单；入口必须落在调用者已加载的镜像内。
- 静态 `TASK_TABLE`（`TaskTable`）；`TaskTable::create/start/transition/remove/get/has_live_tasks`。
- `consume_park_pending` / `unpark` 实现每任务一位 permit、owner 校验与 `Blocked→Runnable`；最终 permit 检查与 block commit 共用同一次表锁事务，覆盖跨 CPU 通知。
- Core 拥有的 `task_entry_trampoline`（组件任务从 Core 边界进入）。
- 类型：`TaskId`、`TaskState`、`TaskRecord`、`TaskTable`、`Kernelstack`、`TaskError`。

## 明确不做

- **不做调度**：runqueue / vruntime / cursor 属于 Scheduler 组件；`sched` 负责验证与提交；task 只提供状态与执行原语。
- 不拥有 event、waitqueue 或 condition 语义；组件按自己的条件维护等待者 `TaskId`，再调用 park/unpark。
- `unpark` 允许从 IRQ 回调调用；任务表锁必须在 irq-save 保护下获取，并在释放表锁后才恢复 IRQ。
- 拒绝在 IRQ 上下文创建 / 启动任务（`TaskError::InvalidTransition` → `-EINVAL`）。
- 非 owner 操作任务被拒（`WrongOwner`）；入口越出 owner 镜像被拒（`EntryOutOfImage`）。
- 不跨组件共享任务真相：`TaskId` 只在 Core 内唯一，组件拿不到任务表写权限。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/task/mod.rs` | 语义入口 `create_task` / `start_task` + 边界测试 |
| `os/core/src/task/id.rs` | `TaskId` |
| `os/core/src/task/state.rs` | `TaskState`（含 `Running(CpuId)`） |
| `os/core/src/task/record.rs` | `TaskRecord`（owner / state / pending permit / stack / context） |
| `os/core/src/task/table.rs` | `TaskTable`（`BTreeMap<TaskId, TaskRecord>`）、状态转换 |
| `os/core/src/task/kstack.rs` | `Kernelstack` |
| `os/core/src/task/user.rs` | 普通用户执行、逐页 copy、clone / replace / protect / staging 回滚；RV64 S/MMU，其他目标 ENOTSUP |
| `os/core/src/task/error.rs` | `TaskError` |

用户执行从同一 TaskRecord 的 KernelNative personality 入口进入，trap 恢复该任务的
内核栈 / kernel satp，再允许 yield / park。复制和权限编辑只允许 owner 操作未启动
任务或当前任务，不把原始页表 / 用户 backing 交付组件。成功替换与终态 backing 保持
驻留；staging discard 仅针对 never-started 用户任务。新建 AS 与 map / copy / prepare /
protect / clone / replace 在 registry 锁内复验 owner 为 Starting / Ready，并保持到相应
Task / AS 提交完成，锁序为 registry → task → mapping plan / AS；discard 保留拆除语义。
ABI 以 `abi/core.toml` 为准。
