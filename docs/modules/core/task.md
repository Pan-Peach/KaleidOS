# task（os/core/src/task/）

> 任务**身份与生命周期真相**：谁存在、属于谁、处于什么状态、跑在哪个 CPU、用哪个内核栈、上下文在哪。
> 这里的"上下文"是执行状态；**调度策略**（下一个跑谁）不在这里。

## owns 什么真相

- Task 身份（`TaskId`）与 owner（`ComponentId`）。
- 任务状态机（`TaskState`：Created / Runnable / Running(CpuId) / Blocked / Exited）。
- 内核栈（`Kernelstack`）与任务执行上下文。
- 每个任务最多一份 pending unpark permit（提前通知会被下一次 park 消费；重复通知合并）。
- 任务归属：`TaskRecord.owner`（组件停止 / 失败时按 owner 回收）。

## 暴露什么机制

- `create_task(requester, entry, arg)` / `start_task(requester, task)`：语义入口，只接受 Core 导出的白名单；入口必须落在调用者已加载的镜像内。
- 静态 `TASK_TABLE`（`TaskTable`）；`TaskTable::create/start/transition/remove/get/has_live_tasks`。
- `consume_park_pending` / `unpark` 实现每任务一位 permit、owner 校验与 `Blocked→Runnable`；调度层还需把 permit 快速路径与 block commit 正确衔接。
- Core 拥有的 `task_entry_trampoline`（组件任务从 Core 边界进入）。
- 类型：`TaskId`、`TaskState`、`TaskRecord`、`TaskTable`、`Kernelstack`、`TaskError`。

## 明确不做

- **不做调度**：runqueue / vruntime / cursor 属于 `sched` + Scheduler 组件；task 只提供状态与执行原语。
- 不拥有 event、waitqueue 或 condition 语义；组件按自己的条件维护等待者 `TaskId`，再调用 park/unpark。
- `unpark` 允许从 IRQ 回调调用；任务表锁必须在 irq-save 保护下获取，并在释放表锁后才恢复 IRQ。
- 拒绝在 IRQ 上下文创建 / 启动任务（`TaskError::InvalidTransition` → `-EINVAL`）。
- 非 owner 操作任务被拒（`WrongOwner`）；入口越出 owner 镜像被拒（`EntryOutOfImage`）。
- 不跨组件共享任务真相：`TaskId` 只在 Core 内唯一，组件拿不到自报告 identity。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/task/mod.rs` | 语义入口 `create_task` / `start_task` + 边界测试 |
| `os/core/src/task/id.rs` | `TaskId` |
| `os/core/src/task/state.rs` | `TaskState`（含 `Running(CpuId)`） |
| `os/core/src/task/record.rs` | `TaskRecord`（owner / state / pending permit / stack / context） |
| `os/core/src/task/table.rs` | `TaskTable`（`BTreeMap<TaskId, TaskRecord>`）、状态转换 |
| `os/core/src/task/kstack.rs` | `Kernelstack` |
| `os/core/src/task/error.rs` | `TaskError` |
