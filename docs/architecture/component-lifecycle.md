# 组件生命周期与实例契约（冻结）

> **状态：已冻结。** 本文件是组件身份、生命周期与入口 ABI 的**唯一依据**；与 `docs/architecture/component-model.md` 冲突时以本文件为准。

本文件只解决一件事：**让组件 ABI 走"实例化"**——每次 instantiate 从一个 `.kcomp` artifact 得到一个完整、独立、拥有自己可写镜像状态的 `ComponentId`。
它**不是**执行域（ExecutionDomain）里程碑，**不是**热更新/卸载里程碑。

---

## 1. 范围与非目标

### 现在做（本契约覆盖）

- 每次 instantiate 从一个 `.kcomp` artifact 得到一个**完整组件**（`ComponentId`），拥有**自己的可写镜像状态**（独立放段 / 重定位的 `.text` / `.rodata` / `.data` / `.bss`）+ 常驻 MemoryLease；加载同一 artifact 两次 = 两个互不共享 `.data` / `.bss` 的组件。
- 组件入口从 `kcomp_init`/`kcomp_exit` 协调替换为 `kcomp_instance_create`/`kcomp_instance_destroy` + 精确 ABI 指纹。
- 每个组件拥有**自己的状态**、资源归属（device / IRQ route / DMA mapping）、任务、接口发布。
- task entry 支持 opaque 参数。
- 服务 endpoint 可按组件命名（多个组件不能都发 `block.device`）。

### 明确**拒绝**在本轮构建

| 拒绝项 | 原因 |
|---|---|
| syscall 传输、IPC thunk、ASID、通用 ExecutionDomain manager | 仍未实现；执行域是进行中的里程碑（`STATUS.md`），私有 AS / 域切换的受限版本见下表后的更新 |
| 物理 unload、refcount→回收、回调排空框架、看门狗、强制终止任务 | 活跃实例计数**不是**代码存活证明（旧表/回调/task context/返回地址都可能仍指向镜像） |
| 通用资源转移/授予图、ResourceDomain 容器、per-instance 字节计费/配额、Core 侧内存账本（region owner / region id / Retired 表） | 违反 `AGENTS.md`；所有权转移是推迟项；Core 不做内存记账，见 `docs/architecture/memory-and-heap.md` |
| 跨域 text 去重、PIC/GOT 改造、共享 Rust runtime | 每次 instantiate 已独立放段 / 重定位自己的 `.data` / `.bss`；text 去重是**未来 loader / MM 优化**，不是组件语义（见 §9），本轮不为此改造 |
| 驱动注册框架、热插拔策略、依赖解析器、自动 ABI 兼容协商 | 无当下需求 |
| `module_init`（image 级初始化钩子） | 不可变表/元数据不需要初始化钩子；一个会声明资源/发布服务的 module_init 会立刻重造"这些归哪个实例"的问题。**7 个组件里没有一个需要它** |

> **更新（取代上表"私有地址空间、域切换"的拒绝项）**：受限的 `IsolatedNative`
> （S + 私有 AS）已落地——`KernelAddressSpace` 生命周期 + 最小跨 AS trampoline（共享 Core 映射） + 按域放段 +
> Core 预置窗口 + KernelNative → Isolated 跨域 service Gate + 失败 / 重启矩阵（RV64+RV32 QEMU
> 证明）；**ASID / U-mode / `ecall` / 出站 Isolated 调用 / 更宽的按域 import 面仍未实现**（支持面 import 已落地），边界是
> 协作式（非对抗隔离）。见 `docs/architecture/deployment.md` §10。

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
拒绝存活任务 → Stopping
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

约定：
- **返回值统一 `0 / -errno`**。**废弃**旧的"非零 = 失败 bitmap"约定（`kcomp_init` 的约定不继承）。
  组件侧不要手写 `const E*: i32`：Rust 用 `kcomp_sdk::Errno` / `Result<T>`，C 用 SDK 的
  `<errno.h>` shim（`return -ENODEV;`）。线格式仍是裸 `i32`（`0 / -errno`），类型只活在语言边界。
- Core 把 `*out_state` 初始化为 `NULL`；成功时组件写入自己完成的 state 指针；**无状态组件可成功返回 NULL**。
- 状态由该实例自己的分配器分配（**堆是 runtime / deployment 策略，不是 Core 资源、不是组件一等资源**；KernelNative 可共享 Core 内核堆，私有执行域可在自己的可写 `.data` / `.bss` 保留私有分配器）。Core 只存/传指针，不解释、不通用释放；组件身份是 `ComponentId` + `ComponentRecord.instance_state`，不与分配器 / runtime 状态合并。（分层与**无账本**契约见 `docs/architecture/memory-and-heap.md`：Core 不记 region owner，无隔离域不记归属、Isolated / Sandboxed 的归属由该实例的 AS / 页表承载；**不再有 per-instance runtime slot / runtime context**；backing 已由 `kcore_memory_acquire/release`（域视图）提供。）
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
- 驱动校验 payload 指纹/布局后，**在自己的 create 边界内**调用 `kcore_device_claim(device_id, ...)`；IRQ / DMA 都以该 `DeviceId` 为锚点（`kcore_irq_register` / `kcore_dma_map`）。
- **`DeviceId` 是选择数据，不是权限。**
- **禁止**把 prober 已认领的设备直接交给 driver——那需要跨组件所有权转移，已明确推迟。预授予若将来需要，Core 必须**先为目标实例建立所有权**再交付。

### Core 侧的创建操作（最小）

```c
int32_t kcore_component_create(const uint8_t *image_name, size_t image_name_len,
                               const struct KcompCreateArgs *args,
                               uint32_t *out_instance);
```

现有 `kcore_component_load` 保留为"默认配置启动"的便利操作，不必立刻暴露完整的 image 管理 API。

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

`kcore_task_create` 现在只收 entry 地址，导致 task 只能读 globals（`driver_prober/src/runtime.rs:101-145`）。必须加 opaque 参数：

```c
typedef void (*KcompTaskEntry)(void *arg);   /* 契约：必须经 Core 退出 */

int32_t kcore_task_create(KcompTaskEntry entry, void *arg, uint32_t *out_task);
```

任务归属仍来自 Core 的执行边界，**不是**来自 `arg`。

---

## 7. 资源归属 identity 规则（重要陷阱）

`RequestContext::ambient()` 解析的是**当前 Core 管理的边界或任务 owner**（`os/core/src/resource/context.rs`）。**直接调用另一个组件的函数表不会进入对方的 Core 边界。** 但 **IRQ 回调会安装一个 Core 拥有的归属边界**（`EscapeKind::Irq`，由 `containment::with_irq_scope` 在 `irq::dispatch_callback` 投递 `RouteOutcome::Callback` 时建立；`os/core/src/irq/mod.rs:94-96`）：principal = **该中断线的 owner**（Core 路由表的真相，不是被中断的执行），`task` 为 `None`；被中断执行的边界被保存，并在回调返回后**显式恢复**（嵌套按后进先出；`os/core/src/component/containment.rs:691-712`）。该作用域**同步、不可 yield**，是**受信 KernelNative 组件下的协作式记账，不是认证边界**：它记录 Core 这次投递为谁而做，但**不能证明**回调代码真的属于那个 owner。

在 IRQ 回调作用域内，调度类 Core 操作在 Core 机制层被拒绝并返回 `-EINVAL`（`SchedError::InvalidTransition` / `TaskError::InvalidTransition`，`os/core/src/errno.rs:68,55`）：`sched::run` / `yield_current` / `exit_current`（`os/core/src/sched.rs:343-360`，祖先感知门禁 `:77-83`）与 `task::create_task` / `start_task`（`os/core/src/task/mod.rs:61,102`）；只读入口与资源访问不受影响。IRQ 回调内的 panic **不**被收敛，保持**致命**：`panic_escape()` 恢复被中断的 guard 后拒绝逃逸，因为 IRQ 回调没有 Core 拥有的上下文可恢复（`os/core/src/component/containment.rs:849-873`），这与 init / task / service-call 边界不同。

### 窄定义的调用边界：`kcore_endpoint_call` 的 service call

`kcore_endpoint_call`（Contract/Endpoint 模型的调用面，`os/core/src/component/call.rs`）就是上面所说的**窄定义的调用边界**：provider 的 `kcomp_service_dispatch` 跑在 Core 拥有的 **per-call service stack** 上（`containment::call_component_service`），principal 是 **provider 自己**——caller 的 task 只作为执行来源（provenance）传递，不构成对该任务的授权；`ambient_init()` 在边界内为 `None`（service call 不得发布）。

- **provider panic 收敛**：dispatcher panic 逃逸回 caller 的 Core 帧，Core 把 provider 标 `Failed`、撤销其 authority 并永久失效它的全部 endpoint；**caller 存活且不变**（绝不把 provider 的 panic 归因 / 终止到 caller）。被放弃的 service stack 保守驻留（phase 1 不回收）。
- **祖先感知门禁**：边界链上任一 IRQ 作用域或 service call 都禁止调度操作（`containment::scheduling_forbidden`，沿 guard 链祖先遍历——嵌套 init/exit 不能把祖先藏起来）；provider 已在当前同步链上 → `CallError::Reentrant`（`-EBUSY`）；IRQ 祖先链上的通用服务调用 → `CallError::InIrqContext`（`-EINVAL`）。调度锚点不被当作 service predecessor 穿越。
- **真实分派由 QEMU 证明**：host fake 上下文后端不执行组件入口体，所以真实 stack switch、真实 provider panic 与端到端 principal 顺序必须由 QEMU 上导出 `kcomp_service_dispatch` 的组件验证（host 用例只覆盖边界记账）。

因此：
- 通过 provider 的 `ctx` **读**它的状态：可以。
- 从普通 consumer 上下文**申请/释放 provider 拥有的资源**：可能撞 owner 检查。
- provider panic：经 `kcore_endpoint_call` 的 service-call 边界**归因到 provider 并收敛**（见上）；绕过 Core 边界直接调用 provider 的函数表则不会。

**不要**用"信任 `ctx` 里的 instance id"去绕过。资源获取与拆除保持在实例生命周期 / 被拥有的任务上下文里；若某服务确实需要 provider 归属的 Core 调用，走 `kcore_endpoint_call` 的 service-call 边界（见上）。

### `ctx` 指什么

- `ctx` 指向**组件私有实例状态**（或状态里一个稳定的 service 子对象），**不是** Core 的实例记录。
- 同一实例的多个服务共用同一个 state 指针是可以的；**不强求相等**。
- 现有设施已具备：`BlockDeviceService::ctx()` 指向自己的 provider 字段（`os/components/kcomp-sdk/src/block.rs:172-180`）。迁移主要是**去用它**，而不是留空 ctx + 全局状态。

---

## 8. 失败、destroy 与隔离

- 沿用现状：failure **不调用** exit/destroy（`failure.rs`），MMIO 设备与 DMA backing 被**隔离（quarantine）**。
- **设备隔离持续到重启**。创建新实例**不得**清除隔离。
- destroy 的命名**不得**暗示 Core 能安全回收全部状态。本轮**保留已暴露的 state 存储**——尤其通过 `'static` SDK 引用交出去的。现有 consumer 可能持有拷贝过的 binding；释放其 ctx 会把今天的"stale 逻辑访问"变成 use-after-free。
- destroy 可以 quiesce 设备并显式清理私有资源，但仅此而已。

---

## 9. 重启与 text 共享

- **不实现 `instances == 0 → unload` / 物理回收。** 组件 backing 保持 pinned-until-reboot；`Stopped` / `Failed` 的记录留作 tombstone，其 backing 仍归**旧组件**所有。
- **重启 = 从同一 artifact 重新 instantiate**：得到**全新 `ComponentId`**、**全新可写 image state**（`.data` / `.bss` 回到 artifact 初始值）、**全新资源归属 / endpoint**。旧组件的 `Stopped` / `Failed` 记录与 backing 驻留（phase 1 不回收）。
  > Isolated 域：**每次 instantiate 都做全新的按域放置**（`isolated_load::place`）到**全新私有 backing + 全新私有 AS**——**没有** same-image backing 复用。同一 artifact 可以有多个**并发** Isolated 组件（各自私有 AS + backing）。trampoline / 共享 Core 映射 / trap 故障收敛 / import 白名单不变（`tp` 是普通架构 / 任务执行状态，由 Core 在任务切换 / trap 时透明保存 / 恢复，全新上下文起点为 0，不再是组件运行时指针）。见 `architecture/deployment.md` §10。
- 可为观察目的派生一个计数，但**不需要原子 refcount 或回收语义**。
- **重启 ≠ 设备恢复**（隔离到重启，见 §8）。

### 关于 text 共享

- **每次 instantiate 都重新放段 + 重新应用重定位**，因此每个组件拥有独立、可写的 image backing（`.data` / `.bss` 天然 per-component，不共享）。
- **代码页（`.text` / `.rodata`）的物理去重是未来的 loader / MM 优化**，不是组件语义模型的一部分，也**不是** ABI / 生命周期承诺：只有当重定位后的可执行字节完全相同（same VA + same import-target VA）时才可能共享，精确条件与依赖缺口见 `docs/architecture/deployment.md` §6。

---

## 10. VirtIO 多实例

`virtio_blk` 的 `DEVICE_ID` / `MMIO_BASE` / `DMA_MAP` / `BLK` 现在是**组件私有 static**——每次 instantiate 都得到独立的可写 image state，因此第二个 `virtio_blk` 组件可以独立接管第二台设备（各自私有 static，互不覆盖）。CoreTest `driver-multi-device` 已在 RV64 + RV32 上验证第二个同 artifact 驱动组件独立 attach。

**剩余的是编排缺口，不是模型限制**：prober 在第一个 `Match` 后停止，不为第二台设备 provision 第二个驱动组件。**不要**在 Core 里造通用的 "current device" 设施，或 fork 第三方驱动框架来掩盖编排问题。

> 旧的 scoped HAL context gate 已随 image 私有化消失：每个组件有自己的 static，"同一份共享 static 被多实例争用"的补救不再需要。通用迁移模式（不可变表保持共享；带可变生命周期的状态移入地址稳定的显式分配；回调经 ctx 访问该状态）的其余部分属于实现进度，不入本契约；C 生命周期 smoke 已落地。

