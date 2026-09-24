# 组件生命周期与实例契约（冻结）

> **状态：已冻结，实施中。** 本文件是所有并行迁移实现的**唯一依据**；与 `docs/architecture/component-model.md` 冲突时以本文件为准。
> 决策来源：Oracle 架构评审（image/instance/domain 拆分）。**人类已确认接受评审版本**（含对原始提案的三处否决）。

本文件只解决一件事：**把"一份加载的组件代码"与"一个跑起来的组件实例"分开**，并把组件 ABI 从"一次性 `kcomp_init`"改成"实例化"。
它**不是**执行域（ExecutionDomain）里程碑，**不是**热更新/卸载里程碑。

---

## 1. 范围与非目标

### 现在做（本契约覆盖）

- 拆分**镜像身份**与**实例身份**；一份常驻镜像 → N 个独立实例。
- 组件入口从 `kcomp_init`/`kcomp_exit` 协调替换为 `kcomp_instance_create`/`kcomp_instance_destroy` + 精确 ABI 指纹。
- 每个实例拥有**自己的状态**、资源归属（device / IRQ route / DMA mapping）、任务、接口发布。
- task entry 支持 opaque 参数（否则"从 statics 迁出"做不完整）。
- 服务 endpoint 可按实例命名（多个实例不能都发 `block.device`）。

### 明确**拒绝**在本轮构建（评审结论）

| 拒绝项 | 原因 |
|---|---|
| syscall 传输、IPC thunk、ASID、通用 ExecutionDomain manager | 仍未实现；执行域是进行中的里程碑（`docs/development/roadmap.md`），私有 AS / 域切换的受限版本见下表后的更新 |
| 物理 unload、refcount→回收、回调排空框架、看门狗、强制终止任务 | 活跃实例计数**不是**代码存活证明（旧表/回调/task context/返回地址都可能仍指向镜像） |
| 通用资源转移/授予图、ResourceDomain 容器、per-instance 字节计费/配额、Core 侧内存账本（region owner / region id / Retired 表） | 违反 `AGENTS.md`；所有权转移是推迟项；Core 不做内存记账，见 `docs/architecture/memory-and-heap.md` |
| loader 复制 `.data/.bss`、跨域 text 去重、PIC/GOT 改造、共享 Rust runtime | **见 §9：当前共享地址空间下，globals 仍是 image-global，per-instance 状态来自显式分配**。复制 BSS 不会重定向已按原 globals 完成重定位的指令 |
| 驱动注册框架、热插拔策略、依赖解析器、自动 ABI 兼容协商 | 无当下需求 |
| `module_init`（image 级初始化钩子） | 不可变表/元数据不需要初始化钩子；一个会声明资源/发布服务的 module_init 会立刻重造"这些归哪个实例"的问题。**7 个组件里没有一个需要它** |

> **更新（increment 3–7，取代上表"私有地址空间、域切换"的拒绝项）**：受限的 `IsolatedNative`
> （S + 私有 AS）已落地——`KernelAddressSpace` 生命周期 + 双映射 assembly gateway + 按域放段 +
> Core 预置窗口 / 邮箱 + KernelNative → Isolated 跨域 service Gate + 失败 / 重启矩阵（RV64+RV32 QEMU
> 证明）；**ASID / U-mode / `ecall` / 出站 Isolated 调用 / 按域 import 解析仍未实现**，边界是
> 协作式（非对抗隔离）。见 `docs/architecture/deployment.md` §10。

---

## 2. 身份模型

```text
ComponentImageId     ← 新增：一份常驻加载的代码
  ├─ 常驻段 / MemoryLease / base
  ├─ create 入口地址（kcomp_instance_create）
  ├─ destroy 入口地址（kcomp_instance_destroy）
  ├─ artifact name（不再强加"每 artifact 只能一个实例"）
  └─ kcomp_abi 指纹

ComponentId          ← 保持现状，语义 = 实例 ID（不新增平行的 ComponentInstanceId）
  ├─ 生命周期 state
  ├─ 资源归属（device / IRQ route / DMA owner）
  ├─ 任务归属（TaskRecord.owner）
  ├─ endpoint 发布归属（EndpointRecord.owner）
  ├─ failure 状态 / containment 身份
  └─ opaque instance state 指针（由组件 create 返回）
```

**关键点**：`ComponentId` 已经在承担实例身份（tasks/devices/routes/mappings/publications/failure 全部按它归属）。**不要**引入第二个平行实例句柄；只新增 `ComponentImageId`。改名可有可无，功能价值很小。

### 所有权划分（现状 → 目标）

| 现在挂在 `ComponentRecord`（`os/core/src/component/registry.rs:33-48`） | 目标归属 |
|---|---|
| `name` | **Image**（artifact 名；唯一性约束从"每名一实例"放宽） |
| `base` / `entry` / `exit` / `memory`(MemoryLease) / `text_size` | **Image** |
| `state` | **Instance** |
| `id`（现为融合身份） | **Instance** = `ComponentId` |

- `LoadedComponent`（`os/core/src/component/loader.rs:39-52`）的 `text_size` 现在被丢弃；拆到 image 记录后应保留。
- 资源的授权表仍按 `ComponentId` 归属（`failure.rs:61-71`）——**不要**把 owner 改成 domain id。

---

## 3. 生命周期

状态机不变（`os/core/src/component/mod.rs:55-117`，单一真相 `ComponentState::can_transition`）：

```text
declare instance → Resolved → Starting
    → 在该实例身份下调用 kcomp_instance_create(args, &out_state)
    → Core 记录 out_state
    → 提交该实例的 pending publications
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
- 状态经该实例自己的 `HeapState` 分配；**共享的是分配器实现代码，不是堆**。Core 只存/传指针，不解释、不通用释放。（分层与**无账本**契约见 `docs/architecture/memory-and-heap.md`：Core 不记 region owner，无隔离域不记归属、Isolated / Sandboxed 的归属由该实例的 AS / 页表承载；per-instance runtime context 是目标；backing 已由 `kcore_memory_acquire/release`（域视图）提供。）
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

**多个实例可以各自发布 `block.device`**：endpoint 身份 = `(provider, port_name, contract)`
（`EndpointId`），端口名只要求在 provider 实例内唯一；同 ABI 的再次发布**绝不覆盖**任何
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

## 9. 常驻、重启与 text 共享

- **不实现 `image.instances == 0 → unload`。** Image 保持 pinned-until-reboot；Stopped/Failed 实例记录留作 tombstone。
- 新创建得到**全新 instance ID** 与**全新 state**，引用同一常驻 image（若复用合法）。
  > Isolated 域的状态：image 记录部署域 + 按域段规划，同域复用 = **逻辑重启**（前一个实例
  > `Failed` / `Stopped` 之后创建全新实例：全新私有 AS / 全新 Core 预置窗口 / 全新 runtime slot）；
  > 并发活跃实例与跨域复用显式拒绝。已在 RV64+RV32 QEMU 证明，见 `architecture/deployment.md` §10。
- 可为观察目的派生一个计数，但**不需要原子 refcount 或回收语义**。
- **逻辑重启 ≠ 设备恢复**（隔离到重启，见 §8）。

### 关于 text 共享的更正

原始提案说"text 共享一份、`.data/.bss` 每实例一份"——**在当前架构下这是错的**：

- 现在是**共享地址空间**：globals 仍是 **image-global**，per-instance 状态来自**显式分配**。
- **只复制 BSS 不会重定向**那些已经按原 globals 完成重定位的指令。
- 跨域共享可执行页依赖：兼容的虚拟布局、重定位、imports、传输 stub。当前重定位按**单一 load base** 解析（`loader.rs:190-243`），段放置只提供段内对齐、**没有页级权限分离**（`loader.rs:156-187`）；含重定位指针的 API 表也不自动可共享。

结论：**共享 text 是未来的 loader 优化，不是"一个链接好的 KernelNative 二进制能在任何执行域原样运行"的 ABI 承诺。**

> **本条结论已被取代（superseded）：** 目标方向（同一份组件代码 + 契约不按部署重写、text 何时可跨域共享的精确条件、依赖排序的缺口清单）见 `docs/architecture/deployment.md` §6。上面这段**现状事实**（单一 load base、无页级权限分离、import 只重定位一次）仍然有效；被取代的是"这不是 ABI 承诺"这个**目标层面**的判断。

---

## 10. 组件迁移清单

通用模式：**不可变表保持共享；带可变生命周期的状态移入地址稳定的显式分配；回调经 ctx 访问该状态。**
IRQ / 重入需要的同步要保留——单 CPU **不**构成放开 `&mut` 别名的理由。

| 组件 | 最小迁移 | 单例裁定 |
|---|---|---|
| `virtio_blk` | 每设备一份 state 分配：block 对象 + claimed **DeviceId** + MMIO 基址 + DMA 记账。在配置的 endpoint 上发布该 state。create 只探测**选中的一个**候选，不再消费整条 assignment 流 | 每个已 attach 设备一个实例 |
| `scheduler_rr` | `CURSOR` 移入实例状态（`scheduler_rr/src/lib.rs:36-60`）；表保持 static | 当前 profile 保留**一个活跃调度角色**，但允许用全新 cursor 的替换实例 |
| `driver_prober` | `SET`/`CURSOR` 移入 state 并传给 dispatch（`runtime.rs:28-43,96-145`）；候选目录保持不可变全局 | 当前系统图保留**一个 prober** |
| `core_test` | 保持串行诊断运行，**不是**每设备一实例（report state 已在入口局部） | 单例 |
| `kbench` | 保持**一个**活跃 benchmark 运行（并发会破坏测量）；有意迁移/重置 run 相关全局，含 task/IRQ 状态（`lib.rs:68-74`、`sched.rs:52-67`、`irq.rs:83-87`） | 单例 |
| `kcomp_smoke` / `kcomp_min` / `kcomp_panic` | 无状态 fixture 返回 null state；no-op / 日志 destroy；**保留 panic fixture 的失败行为** | 无状态 |
| SDK / `logger` | SDK 是库，不是实例。`logger` 是空脚手架（可选组件） | 不需要发明运行时生命周期机器 |

### VirtIO 是非机械部分（启用多实例的 gate）

当前 HAL 回调**没有 receiver/ctx**，直接读全局 `DEVICE_ID` / `MMIO_BASE` 与 `DMA_MAP`（`virtio_blk/src/lib.rs`）。只把这些全局搬进 `State`，HAL **仍然找不到它们**。

可接受方案：**驱动私有的 scoped HAL context**，但**仅在**满足以下条件时成立——所有入口路径显式建立它；HAL 执行**非 yield、非重入、不从 IRQ 回调进入**；panic escape **不会** unwind Rust guard，所以 stale context 必须**无害直到被显式替换/重置**，且**不得**只依赖 `Drop` 做恢复。

**这是 gate：在适配器被证明正确之前，不要启用多个 VirtIO 实例。** 也**不要**在 Core 里造通用的 "current device" 设施，或 fork 第三方驱动框架来掩盖问题。

### 迁移中已一并修的既有 bug

旧 `virtio_blk` 的 `MMIO_HANDLE: AtomicUsize` 在 **RV32 上截断 u64 handle 的一半**。mechanism-first 模型删除 u64 handle 后该问题消失：现在的驱动状态是 `DEVICE_ID: AtomicU32`（claim 锚点）+ `MMIO_BASE: AtomicUsize`（claim 返回的基址），都不需要 u64 handle。

---

## 11. 实施顺序与验证门

协调替换：**loader / containment / SDK / packer 必须一起切**（中间状态不编译是预期的，不要加兼容垫片）。

1. **冻结契约与范围**（本文件）。
2. **冻结 C 侧声明**（`kcomp.h` + Rust 镜像布局 + drift test）——**在写 FatFs 胶水之前**。顺带定：首版 FatFs 是只读，还是需要 block flush（当前契约**没有** flush/ioctl，`block.rs:71-72`）。
3. **写 host contract tests**，然后拆分 image/instance 记录。测试须覆盖：两实例共享一 image、独立 ctx/owner、不同 endpoint、发布失败保留既有 provider、stop/failure 只影响选中实例。
4. **一起切换 loader / containment / SDK / packer**：保留嵌套调用者恢复，在现有 Core-owned 栈上传递 create/destroy 参数，更新 `tools/kcomp-link.sh` 的符号保留/校验；**重建每个组件**，不维护兼容。
5. **迁移普通组件 + task 上下文**：在碰硬件之前，先验证"两个简单有状态实例 + 用全新 ID/ctx 重启"。scheduler/prober/诊断按组合策略保持单例。
6. **迁移 VirtIO 与 prober，RV32/RV64 验证**：多设备启用以 §10 的 HAL-context 证明、全宽 DeviceId / MMIO 基址、独立 endpoint binding、正确的 per-instance teardown、隔离行为不变为前提。
7. **C 生命周期 smoke 已落地，FatFs 胶水待接**：`os/components/tests/kcomp_c_smoke` 是一个
   clang 编的 freestanding C 组件，经 `tools/build-kcomp-c.sh` + SDK C 运行时
   （`kcomp-sdk/c/kcomp_rt.c`）走**同一个** packer / loader 路径；`make test-c-smoke`
   在 RV64/RV32 端到端验证 create（`kcore_log_line`）与 destroy。**仍未做**：C 侧的
   endpoint binding smoke。之后才接 FatFs 胶水——先测 C/Rust 布局与真实调用，再接
   文件系统语义。FatFs 的卷路由与库内全局适配全部留在该组件内，Core 不感知 FAT 或
   `virtio-blk`。

**停止点**：**从一份常驻镜像得到多个独立管理的 KernelNative 实例，且 C ABI 经过测试。** 不悄悄滑向执行域里程碑（C10 进行中，见 `docs/architecture/deployment.md` §10）。

### 验证门（每步）

- `make check`（fmt + clippy `-D warnings`）全绿。
- `make test-host` 全绿（含新增 contract tests）。
- `make init.kpkg`：所有组件（含 C 组件 `kcomp_c_smoke`）全部通过 `.kcomp` 四项契约校验（ET_REL / 入口 DEFINED / UNDEF 仅 `kcore_*` / 重定位白名单）。
- `make test-qemu` + `make test-arch` + `make test-c-smoke` 在 **RV64 与 RV32** 双 profile 全绿。
