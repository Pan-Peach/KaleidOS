# 组件生命周期与实例契约（冻结）

> **状态：已冻结。** 本文件是组件身份、生命周期与入口 ABI 的**唯一依据**；与 `docs/architecture/component-model.md` 冲突时以本文件为准。

身份与入口 ABI 已冻结：每次 instantiate 从一个 `.kcomp` artifact 得到一个完整、独立、拥有自己可写镜像状态的 `ComponentId`。
§1–§10 描述当前契约；§11 保留完整化目标与剩余缺口。已实现 I/U CPU-only Force 与显式 reclaim 的范围见 §11.1，不能推导一般 Graceful drain 或 S-mode 抢占已完成。

---

## 1. 范围与非目标

### 现在做（本契约覆盖）

- 每次 instantiate 从一个 `.kcomp` artifact 得到一个**完整组件**（`ComponentId`），拥有**自己的可写镜像状态**（独立放段 / 重定位的 `.text` / `.rodata` / `.data` / `.bss`）+ 由 LoadedComponent 持有的 MemoryLease；加载同一 artifact 两次 = 两个互不共享 `.data` / `.bss` 的组件。
- 组件入口从 `kcomp_init`/`kcomp_exit` 协调替换为 `kcomp_instance_create`/`kcomp_instance_destroy` + 精确 ABI 指纹。
- 每个组件拥有**自己的状态**、资源归属（device / IRQ route / DMA mapping）、任务、接口发布。
- task entry 支持 opaque 参数。
- 服务 endpoint 可按组件命名（多个组件不能都发 `block.device`）。

### 明确**拒绝**在本轮构建

| 拒绝项 | 原因 |
|---|---|
| ASID、通用 ExecutionDomain manager | 本轮不引入；I/U 窄 adapter 复用既有机制 |
| 通用物理 unload、refcount→回收、回调排空框架、K/I 强杀 | 私有 CPU-only reclaim 单独证明静止；活跃计数不是裸引用存活证明 |
| 通用资源转移/授予图、ResourceDomain 容器、per-instance 字节计费/配额、Core 侧内存账本（region owner / region id / Retired 表） | 违反 `AGENTS.md`；所有权转移是推迟项；Core 不做内存记账，见 `docs/architecture/memory-and-heap.md` |
| 跨域 text 去重、PIC/GOT 改造、共享 Rust runtime | 每次 instantiate 已独立放段 / 重定位自己的 `.data` / `.bss`；text 去重是**未来 loader / MM 优化**，不是组件语义（见 §9），本轮不为此改造 |
| 驱动注册框架、热插拔策略、依赖解析器、自动 ABI 兼容协商 | 无当下需求 |
| `module_init`（image 级初始化钩子） | 不可变表/元数据不需要初始化钩子；一个会声明资源/发布服务的 module_init 会立刻重造"这些归哪个实例"的问题。**7 个组件里没有一个需要它** |

> 当前支持：I 的持久 Task/IPC 和私有 heap 已在 RV64/RV32 S/MMU 接线，
> U `.kcomp`/ecall/Task/heap/IPC 在 RV64 S/MMU 接线。复用相同实例身份，
> 无设备/DMA/IRQ import；I 仍为可信协作式 S-mode。详见 [部署 §10](deployment.md#10-实现状态)。

---

## 2. 身份模型

```text
ComponentId          ← 唯一的运行时身份：一个完整的运行组件
  ├─ name（artifact 名）
  ├─ loaded: LoadedComponent（base / create / destroy / service_dispatch / text_size）
  ├─ 常驻 MemoryLease（本组件私有的可写镜像状态：.text / .rodata / .data / .bss）
  ├─ 生命周期 state
  ├─ 资源归属（device / IRQ route / DMA owner）
  ├─ 任务归属（TaskRecord.owner）
  ├─ endpoint 发布归属（EndpointRecord.owner）
  ├─ failure 状态 / containment 身份
  └─ opaque instance state 指针（由组件 create 返回）
```

实例声明时，Core 另记录实际创建请求的 ComponentId（启动 Core 锚点可为 None），
其后不可经公开 API 改写。它及不可改写的真实创建祖先链只提供 [IPC grant](ipc.md) 的显式组合授权，不继承
child 的设备、内存、Task 或其他资源所有权；父实例重启不会接管旧 child 的创建者身份。

**关键点**：`ComponentId` 就是唯一的一等运行时身份，承载一个**完整的运行组件**——它自己的已加载程序、常驻 backing、资源、任务、endpoint。**没有** `ComponentImageId` / `ComponentImage` / `ImageTable` 二级身份：`LoadedComponent`（base / create / destroy / service_dispatch / text_size / MemoryLease）由 `ComponentRecord` **1:1 直接拥有**，不存在 instance → image 的二级查找。

每次 instantiate（同一 `.kcomp` artifact 或不同 artifact）都**独立**做段放置 + 重定位，得到自己的可写 image backing。加载同一 artifact 两次 = 两个 `ComponentId`，它们的 `.data` / `.bss` 互不共享、独立重定位。`.text` / `.rodata` 的物理去重是**未来 loader / MM 优化**，不是组件语义模型的一部分。

### 所有权划分

| 属于 `ComponentRecord`（`registry.rs`） | 归属 |
|---|---|
| `name` | **Component**（artifact 名；同名可并存，每次 instantiate 各自记录） |
| `loaded.base` / `loaded.create` / `loaded.destroy` / `loaded.service_dispatch` / `loaded.text_size` / `loaded.memory`(MemoryLease) | **Component** |
| `state` | **Component** |
| `id` | **Component** = `ComponentId` |

- `LoadedComponent`（`os/core/src/component/loader.rs`）的 `text_size` 直接保留在记录里。
- 资源的授权表仍按 `ComponentId` 归属（`failure.rs`）——**不要**把 owner 改成 domain id。

---

## 3. 生命周期

状态机不变（`os/core/src/component/mod.rs:55-117`，单一真相 `ComponentState::can_transition`）：

```text
declare component → Resolved → Starting
    → 在该组件身份下调用 kcomp_instance_create(args, &out_state)
    → Core 记录 out_state
    → 提交该组件的 pending publications
    → Ready
```

停止：

```text
同一准入事务：拒绝存活任务 / 在途 Gate、policy、IRQ / Native Direct 发布 → Stopping
    → 在该实例身份下调用 kcomp_instance_destroy(state)
    → Core 兜底撤销归属（device quarantine / DMA 停车）/ unbind
    → Stopped
```

规则：
- `kcomp_instance_create` 失败 / panic / 发布失败 → 走现有 **Failed** 路径（revoke + discard pending）。
- **panicked 或未完整构造的实例，不要调用 destroy**。构造函数内部的错误清理由组件自己负责。
- create 成功后的 commit 失败：按现有"物理驻留"原则保守保留状态。
- destroy 失败 / panic → 实例置 **Failed**，保留内存，走 Core containment。**绝不自动重试析构**。
- 生命周期顺序沿用现有 staged publication（`endpoint.rs`），不要改成"先发布后初始化"。

---

## 4. 组件 ABI（C 侧声明）

C 是根（`AGENTS.md`：「Rust ABI 永不成为 Component ABI」）。Rust 侧是本头文件的薄 wrapper。

```c
#include <stdint.h>
#include <stddef.h>

/* create 参数：仅在调用期间借用。payload 必须拷贝后才能持久化。 */
struct KcompCreateArgs {
    uint64_t    config_abi;   /* config 负载的精确指纹，0 = 无负载 */
    const void *config;       /* Core 视为不透明字节 */
    size_t      config_len;
};

/* 精确契约指纹（手工维护，非版本号）。Core 在调用组件代码前校验其 ELF 定义、边界与值。 */
extern const uint64_t kcomp_abi;

/* 必需导出。返回 0 或负 errno。 */
int32_t kcomp_instance_create(const struct KcompCreateArgs *args, void **out_state);
int32_t kcomp_instance_destroy(void *state);
```

SDK 还可选导出 `kcomp_runtime_init(const struct kcomp_runtime *)`。Core 在业务 create 前以实例身份调用一次，交付部署后端：KernelNative 获得共享堆的窄 C alloc/dealloc 地址，私有域两项为零、使用镜像内分配器。失败按 create 失败收敛。描述符布局由 `abi/component.toml` 生成；没有堆的组件可省略入口，业务 create 签名不变。细节见 `memory-and-heap.md` §6.1。

约定：
- **返回值统一 `0 / -errno`**。**废弃**旧的"非零 = 失败 bitmap"约定（`kcomp_init` 的约定不继承）。
  组件侧不要手写 `const E*: i32`：Rust 用 `kcomp_sdk::Errno` / `Result<T>`，C 用 SDK 的
  `<errno.h>` shim（`return -ENODEV;`）。线格式仍是裸 `i32`（`0 / -errno`），类型只活在语言边界。
- Core 把 `*out_state` 初始化为 `NULL`；成功时组件写入自己完成的 state 指针；**无状态组件可成功返回 NULL**。
- 状态由该实例自己的分配器分配（**堆是 runtime / deployment 策略，不是 Core 资源、不是组件一等资源**；KernelNative 可共享 Core 内核堆，私有执行域可在自己的可写 `.data` / `.bss` 保留私有分配器）。Core 只存/传指针，不解释、不通用释放；组件身份是 `ComponentId` + `ComponentRecord.instance_state`，不与分配器 / runtime 状态合并。（分层与**无账本**契约见 `docs/architecture/memory-and-heap.md`：Core 不记 region owner，无隔离域不记归属、Isolated / Sandboxed 的归属由该实例的 AS / 页表承载；**不再有 per-instance runtime slot / ambient 堆指针**；backing 已由 `kcore_memory_acquire/release`（域视图）提供。）
- `kcomp_abi` 是手工维护的精确契约指纹；**不加版本后缀、不做兼容协商、不自动生成哈希**。
- **协调替换**：原地删除 `kcomp_init` / `kcomp_exit`，**不留 legacy fallback**（`AGENTS.md`：不保证陈旧 `.kcomp` 可加载）。

### 资源输入

`config` 里放**组件自定义的 C 布局小结构**；跨组件交付的 config 使用**扁平字节、
无嵌套指针**（设备选择场景的规范布局在 `abi/probe.toml` 的 `DriverCreateConfig`）：

```c
/* 固定 8 字节头部；endpoint_name 字节紧随其后（offset 8，长度 = name_len，无 NUL）。
   总长恰好 8 + name_len。Core 视整段为不透明字节。 */
struct kcomp_driver_create_config {
    uint32_t device_id;          /* offset 0：候选设备（选择数据，不是权限） */
    uint32_t endpoint_name_len;  /* offset 4：结果端口名长度 */
    /* uint8_t endpoint_name[];  offset 8，长度 = endpoint_name_len */
};
```

- Core 视其为**不透明字节**。
- 驱动校验 payload 指纹/布局后，**在自己的 create 边界内**调用 `kcore_device_claim(device_id, ...)`；IRQ 以 `(DeviceId, resource_index)` 为锚点（`kcore_irq_register` / `kcore_irq_enable` ...），DMA 以 `DeviceId` 为锚点（`kcore_dma_map`）。
- **`DeviceId` 是选择数据，不是权限。**
- **禁止**把 prober 已认领的设备直接交给 driver——那需要跨组件所有权转移，已明确推迟。预授予若将来需要，Core 必须**先为目标实例建立所有权**再交付。

### Core 侧的创建操作（最小）

```c
int32_t kcore_component_create(const uint8_t *image_name, size_t image_name_len,
                               uint32_t domain, const struct KcompCreateArgs *args,
                               uint32_t *out_instance);
```

`kcore_component_load(name, len, domain)` 是“默认配置启动”的便利操作，返回组件 id / -errno。
`domain` 使用 schema 的整数编码（KernelNative=0 / IsolatedNative=1 / SandboxedNative=2）；
Core 验证支持面，不隐式回退，SandboxedNative 当前返回 ENOTSUP。带自定义 config 的
`kcore_component_create` 使用同一 domain 编码，配置与部署正交；Core 把不透明 config
送到所选域的真实 create 边界，不扩充完整 image 管理 API。
`kcore_component_stop(id)` 复用同一个 Stop 操作，供受信 KernelNative 组合方使用；
IRQ / Gate / policy 上下文拒绝，没有父子权限表。Stop 拒绝不改状态，销毁入口在锁外运行。

import 签名变化与生命周期布局变化一样必须原地协调替换 `KCOMP_ABI`，不保留旧签名兼容。

---

## 5. 服务 endpoint 命名

**多个组件可以各自发布 `block.device`**：endpoint 身份 = `(provider ComponentId, port_name, contract)`
（`EndpointId`），端口名只要求在 provider 组件内唯一；同 ABI 的再次发布**绝不覆盖**任何
已有 endpoint，provider 停止 / 失败即其全部 endpoint 永久失效、绝不重定向。

- **由组合策略提供**端口名（多实例场景），**ABI 指纹不变**。
- SDK 的 provider wrapper 提供 `publish_endpoint(port_name, port)`；consumer 侧显式
  `Endpoint::lookup(provider, port_name)` + `bind`。
- **不引入服务发现框架。**

---

## 6. 任务 ABI

`kcore_task_create` 接受入口和 opaque 参数；一个实例可以没有任务，也可以拥有多个任务。
后台 Worker 与 Direct 入口共用该实例身份、资源与生命周期，不另设 Server 基类：

```c
typedef void (*KcompTaskEntry)(void *arg);   /* 契约：必须经 Core 退出 */

int32_t kcore_task_create(KcompTaskEntry entry, void *arg, uint32_t *out_task);
```

任务归属仍来自 Core 的执行边界，**不是**来自 `arg`。

---

## 7. 资源归属 identity 规则（重要陷阱）

`RequestContext::ambient()` 解析的是**当前 Core 管理的边界或任务 owner**（`os/core/src/resource/context.rs`）。**直接调用另一个组件的函数表不会进入对方的 Core 边界。** 但 **IRQ 回调会安装一个 Core 拥有的归属边界**（`EscapeKind::Irq`，由 `containment::with_irq_scope` 在 `irq::dispatch_callback` 投递 `RouteOutcome::Callback` 时建立；`os/core/src/irq/mod.rs:94-96`）：principal = **该中断线的 owner**（Core 路由表的真相，不是被中断的执行），`task` 为 `None`；被中断执行的边界被保存，并在回调返回后**显式恢复**（嵌套按后进先出；`os/core/src/component/containment.rs:691-712`）。该作用域**同步、不可 yield**，是**受信 KernelNative 组件下的协作式记账，不是认证边界**：它记录 Core 这次投递为谁而做，但**不能证明**回调代码真的属于那个 owner。

在 IRQ 回调作用域内，调度类 Core 操作在 Core 机制层被拒绝并返回 `-EINVAL`（`SchedError::InvalidTransition` / `TaskError::InvalidTransition`，`os/core/src/errno.rs:68,55`）：`sched::run` / `yield_current` / `exit_current`（`os/core/src/sched.rs:343-360`，祖先感知门禁 `:77-83`）与 `task::create_task` / `start_task`（`os/core/src/task/mod.rs:61,102`）；只读入口与资源访问不受影响。IRQ 回调内的 panic **不**被收敛，保持**致命**：`panic_escape()` 恢复被中断的 guard 后拒绝逃逸，因为 IRQ 回调没有 Core 拥有的上下文可恢复（`os/core/src/component/containment.rs:849-873`），这与 init / task / service-call 边界不同。

### Init / Exit 的临时栈与创建准入

Init / Exit 使用 Core 临时栈，祖先链上存在这两种边界时，`task_yield`、`task_park`、
`task_exit` 返回 `-EINVAL`，避免在 caller Task 上切栈并丢失 lifecycle principal。
Init 仍可创建、启动所属 Worker；启动锚点内的 `sched_run` 保留。进入真正 Task 时建立
独立 Task 边界，不把启动锚点的 Init 当作该 Task 的祖先。

create / load 拒绝 IRQ 和 Policy 祖先上下文（`-EINVAL`）；有 ambient caller 时，
必须处于 Starting / Ready，否则 `-EPERM`。完成装载后、登记新实例时，在 registry
锁内再次复验 caller，防止失败后的迟到登记。Core 内部无 ambient 的启动调用仍可创建。

### 窄定义的调用边界：`kcore_endpoint_call` 的 service call

`kcore_endpoint_call`（Contract/Endpoint 模型的调用面，`os/core/src/component/call.rs`）就是上面所说的**窄定义的调用边界**：provider 的 `kcomp_service_dispatch` 跑在 Core 拥有的 **per-call service stack** 上（`containment::call_component_service`），principal 是 **provider 自己**——caller 的 task 只作为执行来源（provenance）传递，不构成对该任务的授权；`ambient_init()` 在边界内为 `None`（service call 不得发布）。

- **provider panic 收敛**：dispatcher panic 逃逸回 caller 的 Core 帧，Core 把 provider 标 `Failed`、撤销其 authority 并永久失效它的全部 endpoint；**caller 存活且不变**（绝不把 provider 的 panic 归因 / 终止到 caller）。被放弃的 service stack 保守驻留（phase 1 不回收）。
- **祖先感知门禁**：边界链上任一 IRQ 作用域或 service call 都禁止调度操作（`containment::scheduling_forbidden`，沿 guard 链祖先遍历——嵌套 init/exit 不能把祖先藏起来）；provider 已在当前同步链上 → `CallError::Reentrant`（`-EBUSY`）；IRQ 祖先链上的通用服务调用 → `CallError::InIrqContext`（`-EINVAL`）。调度锚点不被当作 service predecessor 穿越。
- **真实分派由 QEMU 证明**：host fake 上下文后端不执行组件入口体，所以真实 stack switch、真实 provider panic 与端到端 principal 顺序必须由 QEMU 上导出 `kcomp_service_dispatch` 的组件验证（host 用例只覆盖边界记账）。

因此：
- 通过 provider 的 `ctx` **读**它的状态：可以。
- 从普通 consumer 上下文**申请/释放 provider 拥有的资源**：可能撞 owner 检查。
- provider panic：经 `kcore_endpoint_call` 的 service-call 边界**归因到 provider 并收敛**（见上）；绕过 Core 边界直接调用 provider 的函数表则不会。

**不要**用"信任 `ctx` 里的 instance id"去绕过。资源获取与拆除保持在实例生命周期 / 被拥有的任务上下文里；普通服务在 provider owned Server Task 中执行 Core 调用；保留的同步诊断可走 `kcore_endpoint_call` 的 service-call 边界（见上）。

### `ctx` 指什么

- `ctx` 指向**组件私有实例状态**（或状态里一个稳定的 service 子对象），**不是** Core 的实例记录。
- 同一实例的多个服务共用同一个 state 指针是可以的；**不强求相等**。
- 上述 ctx 规则适用于保留的同步策略/隔离与生命周期诊断。普通 Block/Filesystem 已删除函数表及 BlockDeviceService；IPC Server Task 持本镜像私有状态，endpoint 的 api/ctx 为零。

---

## 8. 失败、destroy 与隔离

生命周期保证分层：**Logical death**（拒绝新工作/身份失效）、**Execution quiescence**
（Task、调用、已准入 callback 静止）、**Physical reclamation**（代码/状态/backing
可释放）分别证明。`Failed` 或 `inflight == 0` 不代表后两项成立；Direct 引用与 DMA
设备静默不能从一个调用计数推导。业务 Recovery 属于组合与服务 Runtime，见
[服务执行 §5–§6](service-execution.md#5-连接会话与失效)。

- 沿用现状：failure **不调用** exit/destroy（`failure.rs`），MMIO 设备与 DMA backing 被**隔离（quarantine）**。
- **设备隔离持续到重启**。创建新实例**不得**清除隔离。
- destroy 的命名**不得**暗示 Core 能安全回收全部状态。Native 实例发布非空 Direct 表后，Core **拒绝 Stop/destroy（EBUSY）**；即使 endpoint 已失效，也不能证明表已归还。不新增 Direct 引用计数，不追踪每次调用。failure 不调用 destroy，已暴露的 state 必须驻留。本轮**保留已暴露的 state 存储**——尤其通过 `'static` SDK 引用交出去的。现有 consumer 可能持有拷贝过的 binding；释放其 ctx 会把今天的"stale 逻辑访问"变成 use-after-free。
- destroy 可以 quiesce 设备并显式清理私有资源，但仅此而已。

---

## 9. 重启与 text 共享

- 不按 `instances == 0` 推断可回收。K backing 保留驻留；I/U CPU-only 在执行离场及全局私有域安全点证明后显式 reclaim。`Stopped`/`Failed` 身份 tombstone 永久保留。
- **重启 = 从同一 artifact 重新 instantiate**：得到**全新 `ComponentId`**、**全新可写 image state**（`.data` / `.bss` 回到 artifact 初始值）、**全新资源归属 / endpoint**。旧组件记录保留，私有 backing 是否释放由 reclaim 的独立证明决定。
  > Isolated 域：**每次 instantiate 都做全新的按域放置**（`isolated_load::place`）到**全新私有 backing + 全新私有 AS**——**没有** same-image backing 复用。同一 artifact 可以有多个**并发** Isolated 组件（各自私有 AS + backing）。trampoline / 共享 Core 映射 / trap 故障收敛 / import 白名单不变（`tp` 是普通架构 / 任务执行状态，由 Core 在任务切换 / trap 时透明保存 / 恢复，全新上下文起点为 0，不再是组件运行时指针）。见 `architecture/deployment.md` §10。
- 可为观察目的派生一个计数，但**不需要原子 refcount 或回收语义**。
- **重启 ≠ 设备恢复**（隔离到重启，见 §8）。
- **重启 ≠ 业务恢复或 Live Update**：新实例不继承旧 endpoint、open handle 或连接身份。
  上层可显式建立新路由；旧对象不静默指向新实例。状态重建/重放、静止点、迁移与回滚
  分别需要服务契约和证据，不由 instantiate 自动提供。

### 关于 text 共享

- **每次 instantiate 都重新放段 + 重新应用重定位**，因此每个组件拥有独立、可写的 image backing（`.data` / `.bss` 天然 per-component，不共享）。
- **代码页（`.text` / `.rodata`）的物理去重是未来的 loader / MM 优化**，不是组件语义模型的一部分，也**不是** ABI / 生命周期承诺：只有当重定位后的可执行字节完全相同（same VA + same import-target VA）时才可能共享，精确条件与依赖缺口见 `docs/architecture/deployment.md` §6。

---

## 10. VirtIO 多实例

`virtio_blk` 的 `DEVICE_ID` / `MMIO_BASE` / `DMA_MAP` / `BLK` 现在是**组件私有 static**——每次 instantiate 都得到独立的可写 image state，因此第二个 `virtio_blk` 组件可以独立接管第二台设备（各自私有 static，互不覆盖）。CoreTest `driver-multi-device` 已在 RV64 + RV32 上验证第二个同 artifact 驱动组件独立 attach。

prober 当前已逐台编排，事实见 [STATUS](../../STATUS.md) §3.29。**不要**在 Core 里造通用的 "current device" 设施，或 fork 第三方驱动框架来掩盖编排问题。

> 旧的 scoped HAL context gate 已随 image 私有化消失：每个组件有自己的 static，"同一份共享 static 被多实例争用"的补救不再需要。通用迁移模式（不可变表保持共享；带可变生命周期的状态移入地址稳定的显式分配；回调经 ctx 访问该状态）的其余部分属于实现进度，不入本契约；C 生命周期 smoke 已落地。

## 11. Runtime 完整化当前与目标

### 11.1 终止与回收分别报告

当前管理入口 `kcore_component_force_stop` 与 `kcore_component_reclaim` 只开放给
活 KernelNative 管理上下文。Force 立即逻辑失效并跳过 destroy；尚未实际离场返回
EBUSY，可有限重试。K 仍有复制过的 Direct 表时，Force 完成逻辑撤销后返回
ENOTSUP，不声称裸调用已排空。Reclaim 对 K 返回 ENOTSUP；I/U 必须 Failed/Stopped、无 inflight、
所属 Task 均 Exited 且 execution_retired、其他私有域无 Running Task 或生命周期执行。
成功清除实体 backing/AS/Task，保留身份并设置内部 reclaimed 标记，重复 reclaim 成功。
不需要新 ComponentState。页表/别名恢复失败保守保留；调用者不能从 force 返回值推导
物理回收。当前没有完整的逐资源 retained reason 结构化 API。
U 每 10ms timer 返回 Core 复验状态；K/I 不保证非协作执行可强杀。一般 Graceful
通知/cleanup Task/drain 协议仍是下述目标；Echo 测试先以业务 STOP 退出 Server。

Component 是资源生命周期的基本归属单位；复用 `ComponentRecord.loaded`、AS、
Task owner、Endpoint owner 和现有设备表，不增加 Image/Instance Registry。
`Stopped` 表示正常逻辑终止，`Failed` 表示异常逻辑终止；两者均禁止新业务。
执行是否排空、哪些物理资源已回收必须另行报告，不能仅由状态推断。

完整目标的内部结果应包含 logical state、execution drained、reclaimed extents/pages 与
retained reason；`Reclaimed` / `Quarantined` 是回收结果，不增加公开 ComponentState。
允许部分资源回收、部分保留；至少区分 ActiveCpu、ActiveCallback、NativeRawReference、
UnknownDma、AliasRestoreFailed、PageTableTeardownMissing 与 DestroyFailure。
诊断按现有 ComponentId / 资源身份定位，不新建通用资源图。当前没有这份完整结果 API。
停止编排初期仍由受信KernelNative组合方发起；U不因知道ComponentId就可停止别的
实例。U自退出只操作自己的Core Task/实例身份，不从payload取得管理权限。
若停止请求来自目标自己的Task，只提交停止请求并返回/退出；不能同步等待自身排空
再destroy/free正在使用的栈。后续在Core安全上下文推进已认领的停止。

### 11.2 Graceful Stop

现有 Stop 仍是无等待操作：live Task / inflight / Native Direct 发布即 EBUSY，
重复 Stop 返回 EINVAL。目标按以下依赖推进，顺序允许组件清理需求的局部调整：

1. registry 准入事务认领一次 Stop，提交 Ready → Stopping；同时拒绝新 Task、
   publish、grant、submit 与 backing/device/DMA 获取。已认领 Stop 不再次运行 destroy。
2. 通知已有 Task 停止。最小候选是 Core 停止谓词 + owner 内 wake，组件在自己的
   Server/Worker 循环清理并退出；不引入 Unix signal 或通用事件框架。
   **必须同时修改调度准入**：现有 `may_run(Stopping)==false` 无法让这些 Task 收尾。
   仅 Graceful 的已有 Task 可在清理期间恢复；不能让 Stopping 重新获取普通授权。
3. 初期采用 cancel-and-drain：关闭所有服务 Endpoint，已有成功 reply 保留首个结果，
   其余请求完成 ENOTCONN；accepted receipt 的业务执行仍须排空。consumer 的未结请求
   取消/放弃，移除 send grant。close 只终结 transport，不回滚已执行的业务。
   以后若有真实需求，再支持“拒绝新 submit、保留 accepted reply”的独立排空模式。
4. 从 Task kernel stack 返回到 Core；等待所有 owner Task、同步 Gate/policy/IRQ、
   copy 与 AS 进入引用退出。`Exited` 提交在切栈前，必须有 incoming-stack 完成确认，
   不能此时删除 TaskRecord。回收确认前，旧 context / return address 保活。
5. 执行排空后在实例域运行一次 destroy；生命周期入口不属于普通 RPC。保留私有
   lifecycle 栈与 ABI 窗口直至入口返回 Core；destroy 不能 yield/park/exit。
6. 撤销残余 IRQ/DMA/device/Endpoint，满足[内存回收条件](memory-and-heap.md#9-runtime-回收矩阵目标与基线)后释放独占资源，最后提交 Stopped。

Stop 不在 Core 锁内等待、执行 destroy 或调用业务；并发 Task 创建、IPC submit 与
资源获取均须持 registry 锁复验，再在各自表提交。维持现有局部锁序，不能在 AS/PLAN
锁内反向获取 registry/task。destroy 成功与 finish_stop 竞争 Failed 时，Failed 优先，
不能重新变为 Stopped，也不能再次析构。

初期 Stop 使用非阻塞推进/查询与调用方有限 deadline，不在 Core 加 timer waiter 队列。
deadline 到期返回 Pending/TimedOut 和阻塞原因，实例留 Stopping；默认不自动升级 Force。
在没有可调度 lifecycle Task 或安全抢占之前，K/I destroy 挂死无法保证调用返回：
有界 destroy 是可信组件前提，deadline **不能**中断同步入口。U destroy 可在完成真实
U trap/deadline 后受控中止。当前 Stop ABI 只有 i32，新增结果需 schema/fingerprint 协调替换。

### 11.3 Forced Stop 与失败

Force 先关闭业务和资源准入，异常终态使用 Failed；跳过 destroy，终结 IPC，撤销
未来 callback 准入，向正在运行的 CPU 请求离场。Created/Runnable/Blocked Task 可以
在证明没有正在保存/恢复 context 后由 Core 终结；Running Task 的终结必须由本 CPU
从 Core 安全上下文提交。不能由远端删记录或直接把 Running 改成 Exited 并 free。

K/I 当前协作式 S-mode 不具备不 yield 任务的有限时间停止能力；IPI 只有门铃/安全点，
timer 未接内核抢占。即使 S-mode 抢占接通，禁中断、破坏 Core 或持锁挂死仍无通用恢复
保证。遇到这类任务报告 execution pending，代码、栈、AS 和关联 backing 保留。
U-mode 则需真实 timer/IPI trap 回 Core task stack、stop 检查及“禁止再次 sret”才能
强制停止忙循环；不能在 per-CPU trap 栈运行 Scheduler。跨 CPU 确认见[调度契约](scheduling.md#7-runtime-停止与私有-as)。

### 11.4 竞争与失败处理

| 情况 | 目标结果 / 保留条件 |
|---|---|
| Pending IPC | 使用 Exchange 首终态规则；取消不承诺业务回滚，关闭不代替 CPU 排空 |
| Server 已退出 | 复用 task_exited 关闭端口；无 Server 不成为 Stop 永久等待条件 |
| Task 不退出 | deadline 后 Pending/TimedOut；K/I 不声称已强杀，保留被访问 backing |
| destroy 返回错误 / panic | Failed；不重试、不继续业务；只回收独立证明安全的 Core 资源，裸状态保留 |
| 两 CPU Stop | registry 内一次认领；第二请求观察同一结果/进行中，不第二次 destroy |
| 重复 Stop | 现行 EINVAL 保持；目标查询/推进复用结果，不引入第二生命周期 |
| Stop 与 Failed | Failed 不回滚；Force 优先关闭准入；若 destroy 已开始，不能从远端释放其栈/镜像 |
| Stop 与 Exit / Reply | Exchange 锁串行首终态；Task 状态与切栈完成分别确认；不重复 wake/collect/free |
| Create 失败 / commit 失败 | 未构造不 destroy；已启动 Task 也要排空，不能因 create 失败直接释放镜像 |
| OOM | 未发布 lease 可 Drop；已发布资源保留原因，拆除不依赖临时的大额分配 |

只把停止所需的小标志/确认放入已有记录与 per-CPU state；具体字段由实现任务决定。
不加通用 ResourceManager、POSIX process 语义或路由前置。源码审计、逐文件实施任务和
测试证据见 [Runtime 第一轮交付](../development/component-runtime-consolidation.md)。
