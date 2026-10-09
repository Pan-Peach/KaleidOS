# 部署与绑定：执行域约束交互机制（deployment.md）

> 本文件是**"部署（deployment）决定调用机制"**的设计契约：谁提议部署、Core 验证什么、`(caller domain, callee domain)` 如何选出调用机制、binding 携带什么、不支持的组合如何拒绝。
> 它是**设计契约，不是进度快照**。**KernelNative 与受限的 IsolatedNative 两种部署真实存在**：K/I 双向 Gate 已真正派发（§10）；Sandbox transport 仍**未实现**（§10 是实现状态表，§7 是逐条缺口）。
> 与 `docs/architecture/component-lifecycle.md` 在"同一份组件代码能否跨执行域原样运行"上冲突时，**以本文件为准**：`component-lifecycle.md` §9"代码页去重是未来的 loader / MM 优化，不是 ABI / 生命周期承诺"的结论**由本文件补充**（见 §6、§8）。本文件不否认它的现状描述，而是把目标写清楚，并把缺口显式登记。

---

## 1. 分离概念与职责图

组件实例、执行域、接口契约与交互机制分别描述身份、部署环境、语义与双方关系。
Artifact 是程序字节，Endpoint 是一次发布；它们不另建组件生命周期。

Service、Transport、Inline/Queued 与业务 Session 的职责分离以
[服务执行契约](service-execution.md) 为准。这里的 Gate 是同步调用边界，不代表独立
Server Task；部署决定合法调用窗口，不代替 provider 的并发、等待与对象失效语义。

| 概念 | 回答什么 | 身份 / 载体 | 代码锚点 |
|---|---|---|---|
| **Contract** | 这个服务**语义**是什么 | 接口名 + exact ABI fingerprint + typed `#[repr(C)]` function table | `abi/block.toml`、`os/components/kcomp-sdk/src/block.rs:68-75` |
| **Artifact** | 一个**组件程序字节**（不是运行实例） | `.kcomp`（ET_REL）；artifact 名 | `tools/kcomp-link.sh`、`os/core/src/component/loader.rs` |
| **Component** | **一个完整运行组件**：instantiate 后拥有自己的已加载程序 | `ComponentId`；`loaded`（`base` + `create` / `destroy` / `service_dispatch`）+ 资源归属（device / irq / dma / task / publication） | `os/core/src/component/registry.rs`、`os/core/src/component/load.rs` |
| **Endpoint** | provider **发布的服务点** | `EndpointId`（provider ComponentId + port_name + contract） | `os/core/src/component/endpoint.rs` |
| **Execution Domain** | 在什么特权与地址空间环境执行 | Component 的部署属性；Core 保存域与 AS | 本文件 §6、§7 |
| **Interaction Mechanism** | 这次交互怎么执行 | Direct / Gate 是两端的关系；bind 按两端域约束选定，不是整个组件的唯一属性 | 本文件 §2、§3 |

**核心判断（不可违背）：**

- **Core owns the execution-domain truth。** 组合器（composer / profile）**提议**部署（哪个组件跑在哪个执行域），Core **验证并提交**。Core **不硬编码信任级策略**（不写"所有组件都必须走同一套重型机制"）。
- **尽量复用业务实现与契约。** SDK adapter 承担可支持的部署差异；不承诺任意 `.kcomp` 跨 ISA、特权级或 import 面原样运行。
- **Binding 以调用者的执行域为作用域，不能跨调用域转交。** 合法机制取决于**两端**（caller domain **且** callee domain），**绝不**只看 provider 的部署标签。
- **Core 在 bind 时一次性选定机制**，运行期**不按调用重新决策**。SDK **实现**每种机制，但**不得选择**机制；否则组件可能悄悄降级到 native，这是**禁止**的。不支持的部署 / 绑定必须**显式拒绝**。

```text
                        组合器 / Profile（composer）
                              │ propose：组件图 + 每个组件的部署域
                              ▼
┌─────────────────────────────── Core ────────────────────────────────┐
│ owns execution-domain truth                                         │
│ validate：owner / liveness / exact ABI / trust + platform capability│
│ resolve ：Endpoint 发布真相 + 两端执行域 → 调用窗口               │
│           → 选定 mechanism：Direct / Gate / syscall-IPC / rejected  │
└───────────────────────────────┬─────────────────────────────────────┘
                                │ binding（scoped to caller domain）
                                ▼
      ┌──────────────────────┐              ┌───────────────────────────┐
      │ consumer（业务代码） │              │ provider 组件             │
      │  服务前端 typed      │              │  业务后端（真实 read）    │
      │  block.read(lba,buf) │              │  provider local entry     │
      │        │             │              │        ▲                  │
      │  调用后端 call       │──────────────┼────────┘                  │
      │  backend（SDK，机制  │  Direct：直接调 function table + ctx      │
      │  由 Core 在 bind 时  │  Gate  ：Core call gate（per-call stack） │
      │  固定）              │  syscall：ecall → Core → dispatch         │
      └──────────────────────┘              └───────────────────────────┘

   Contract（语义）          Artifact（程序字节）          Component（完整运行组件）
     block.device            .kcomp → ET_REL               ComponentId
     exact ABI + repr(C)     create / destroy 入口          loaded: base + create/destroy
          │                  （每次 instantiate 重新放段）  state + owner(device/irq/dma/task)
          └──────────── Endpoint（发布点） ◄──────────────────┘
                               EndpointId
                                  │
                          Transport / deployment
                          Core 在 bind 时按 (caller domain, callee domain) 选定
```

> **部署形态本身就是安全策略。** 不信任一个组件，就不要把它部署成 KernelNative；Core 不靠"所有组件都过同一套重机制"解决信任问题（`docs/architecture/driver-model.md` §2）。

---

## 2. 部署 → binding 的完整流程

```text
① 提议（composer / profile，非 Core）
     组件图：谁 provides / requires 什么
     + 每个组件的部署域（KernelNative / IsolatedNative / SandboxedNative / …）
     + 该部署域对平台能力的要求（MMU / 私有 AS / 特权级 / IOMMU）

② Core 验证（Policy proposes, Core validates and commits）
     a. owner         ：provider 是该 endpoint 的 owner（Core 从 init 边界解析，不信任自报）
     b. liveness      ：provider 存活且 Ready；endpoint 处于 Live（不交付死 endpoint）
     c. exact ABI     ：consumer 的 Contract ABI 与 provider 的**逐位相等**（不是"兼容范围"）
     d. trust + platform capability：请求的机制是否落在两端执行域**真实具备**的能力内

③ Core 选定 mechanism（一次性，bind 时）
     输入：(caller domain, callee domain)
     输出：Direct | Gate | rejected（syscall-IPC 未实现）
     规则见 §3 模式矩阵

④ Core 交付 binding 并记录 trace（没有单独的 binding registry）
     binding 携带：
       - Direct：provider 的 repr(C) function table 指针 + opaque ctx
                 （endpoint 记录上的 `api` / `ctx`，Core 只存不解引用）
       - Gate  ：Core call-gate handle（不透明 EndpointId；provider principal +
                 per-call service stack + panic containment 由 Core 拥有）
       - SDK 缓存已选机制；域 / owner / 存活仍从 Core 实例与 endpoint 真相解析

⑤ 不支持的组合 → 显式拒绝
     绝不静默降级。例：跨域绑定绝不返回裸 function table；平台无 U-mode / 无私有 AS
     时请求 Sandbox 部署 → 拒绝（`-ENOTSUP` / `-EINVAL` 一类），不是"当成 native 跑"。
```

> `lookup` 比较 contract + 存活；SDK 的 `Endpoint<C>::from_id` 经 `kcore_endpoint_validate` 补齐 exact ABI 校验，`bind` 再验证契约与存活。

**SDK 在 bind 后沿已选机制调用。** Direct 直接执行表，不逐次进入 Core；Gate 每次进入 Core，重新检查 caller / provider 身份、存活、域与重入条件。SDK 不自行重新协商或降级机制。

provider selection 属于组合策略，Core 不从候选中猜 root/default 服务。
**候选，未实现**：组合方在真实需求下可提出 transport preference，Core 仅交付验证后
的机制。当前 bind ABI 无 preference/fallback 字段，仍遵循 §3；显式 K/K endpoint_call
也不等于普通 SDK bind 支持选择 Gate。EndpointId 是发布身份，不是完整 per-consumer
grant capability，不能把可发现当作不可信组件的访问授权。

---

## 3. 调用双方模式矩阵

行 = caller 的执行域，列 = callee 的执行域。单元格 = **合法机制**。`rejected` 表示该组合**必须被 Core 显式拒绝**。

> **实现状态**：K/K Direct（及显式 Gate）、K→I、I→K、I→I Gate 均已接线；Sandbox 参与的调用仍显式拒绝。ArchTest `isolated-domain-service` 在 RV64/RV32 以同一工件、同一 SDK `block.device` 前端验证四种组合、嵌套、panic、stale 与循环重入。

| caller ↓ \ callee → | KernelNative | IsolatedNative | SandboxedNative |
|---|---|---|---|
| **KernelNative** | **Direct**（可显式 Gate） | **Gate** | **rejected** |
| **IsolatedNative** | **Gate** | **Gate** | **rejected** |
| **SandboxedNative** | **rejected** | **rejected** | **rejected** |

这是当前支持矩阵。每个 Isolated 实例有自己的 AS，I/I 不交付裸指针。Sandbox
部署和参与的 binding 都返回 ENOTSUP；没有隐式 KernelNative fallback。
未来跨特权交互需要真实 U-mode 进入、ecall 与访问检查，不能从现有 Gate 推导。

同一个实例可以零 Task、多个 Task、Direct 与 Worker 共存；Worker 请求编码、
队列、reply、取消和业务同步属于组件 / Runtime。Gate 的同步调用栈不可 yield，
不能把它与可阻塞的 Task Request/Reply 当成同一执行上下文。Wasm 是未来执行后端，
不是第四个 ExecutionDomain。没有通用 Core RPC 子系统。

**为什么 native binding 绝不能跨域传递：**

Direct binding 携带的是 `(api, ctx)` 两个**裸指针**，只在 provider 的地址空间里有意义。把它交给另一个执行域的 consumer：

1. **地址无意义或更糟**：在另一个 AS 里，那两个 VA 可能未映射（fault），也可能指向**别的东西**（静默错误）。私有 AS 的前提就是 VA 不共享。
2. **绕过 callee 域的入口与强制**：直接调 function table 跳过了 callee 域的进入 / 退出、provider principal、per-call service stack、panic containment。U-mode 的强制边界（页表 + 特权级）在裸指针下**完全失效**。
3. **作用域错配**：binding 是"**以调用者执行域为作用域**"的；跨域复制它，等于把 A 域的访问窗口塞给 B 域。

因此 **binding 不能跨调用域转交**：Core 必须**按 caller 域重新解析**，产出该域合法的机制，而不是把 A 的绑定拷贝给 B。

**Direct 买到速度，但买不到两件事（写清楚，别高估）：**

| 能力 | Direct | Gate |
|---|---|---|
| 无 Core 通用分派、无栈分配、无消息打包、零 per-call 重决策 | 是 | 否 |
| ambient owner 切换（调用期把归属切到 provider） | **否** | 是 |
| "A 在 B panic 后仍能继续"的承诺（provider panic 收敛、caller 存活） | **否** | 是 |

- Direct 下 provider 与 caller 同特权、同地址空间：**没有** Core 拥有的边界来切换 owner，也**没有**可恢复的上下文来收敛 provider 的 panic。B panic = 进程级 abort 的一部分，Direct **不**承诺 caller 存活。
- Gate 下 provider 跑在 Core 拥有的 per-call service stack 上，principal = provider 自己，provider panic 被 Core 收敛（标 `Failed`、撤销 authority、永久失效其 endpoint），**caller 存活且不变**。

**`inflight` 计 Core 管理的 Gate、policy 与 IRQ 执行。** Core 的 endpoint `inflight` 计数在**经 Core 边界** 的执行上递增；Direct 绑定**完全绕过 Core**，Core 看不到。因此：

> **"inflight == 0 → 可以 stop（否则 -EBUSY）"永远不能证明"所有调用都已结束"。** 它只证明"没有正在进行的 Core-managed 调用 / 回调"。若存在 Direct 绑定，consumer 手里那张 function table 的调用对 Core 不可见——这正是 KernelNative 无隔离 + 物理驻留的直接后果。teardown 的正确性不能仅建立在 `inflight` 上。当前 Native 实例发布非空 Direct 表即保守拒绝 Stop/destroy（EBUSY），旧表、ctx 保持驻留；没有 Direct release 协议。

---

## 4. 三个角色与目标调用链

三个角色**保持极小，不为每个角色造框架 / crate**：

| 角色 | 是什么 | 位置 | 约束 |
|---|---|---|---|
| **服务前端（typed frontend）** | consumer 看到的强类型入口，如 `block.read(lba, &mut buf)` | SDK（`os/components/kcomp-sdk/src/block.rs` 一类） | **域无关**；**不持有裸可调用物**；业务代码**永不见** method number / frame / mode 分支 |
| **业务后端（business backend）** | provider 的**真实**实现，如 VirtIO 读盘 | provider 组件内部 | 被**各部署的本地入口 / adapter** 调用；自身**不感知**部署 |
| **调用后端（call backend，在 SDK）** | Core 在 bind 时**已固定**的机制 | SDK 私有 | 持有机制专有信息：同域 native = vtable + ctx；跨域 = Core call-gate handle |

**目标调用链：**

```text
consumer 业务代码
   │  block.read(lba, &mut buf)         ← typed 前端，域无关
   ▼
服务前端（typed frontend）
   │  调用后端（call backend，SDK 私有，机制已在 bind 时由 Core 固定）
   ▼
┌─ Direct：直接调 provider function table（api, ctx），稳态零 Core 介入
├─ Gate  ：Core call gate（provider principal + per-call stack + panic containment）
└─ syscall-IPC：ecall → Core → dispatch
   ▼
provider local entry（按部署的本地入口 / adapter）
   ▼
业务后端（同一份真实 read 实现，不按部署重写）
```

**关键不变量：** 从 consumer 业务代码到业务后端的**语义路径**在三种部署下**完全相同**；变的只有中间那段"调用后端 + 本地入口"。**同一份 consumer 代码不得出现 `if mode == ...`**。

> **API / ctx 保留为 Native Direct transport。** 同域 Direct 的 binding 携带 `api`（`#[repr(C)]` function table）+ `ctx`（provider opaque state），由 `EndpointRegistry::bind` 在选定 Direct 时交付。绑定身份是 **`EndpointId`**（provider + port_name + contract）：一个端口名不再对应"全局唯一的 provider 槽"，provider 停止 / 失败即永久失效、绝不重定向。

---

## 5. BlockDevice 例子

契约（`abi/block.toml`）：`block.device`，ABI = `0x424C_4F43_4B44_4556`（ASCII `"BLOCKDEV"`），function table 三个方法：`capacity_sectors` / `read` / `write`，单位 512 字节 sector。

### 5.1 Rust 前端草案（consumer 侧，域无关）

```rust
// 业务代码：不出现 method number、不出现 frame、不出现 mode 分支
fn load_superblock(blk: &BlockDeviceHandle, lba: u64, buf: &mut [u8]) -> Result<()> {
    blk.read(lba, buf)          // typed 前端
}

// SDK 提供的强类型句柄：内部持**调用后端**，不持裸可调用物
pub struct BlockDeviceHandle {
    backend: CallBackend,       // SDK 私有：Direct { api, ctx } | Gate { endpoint }
}

impl BlockDeviceHandle {
    pub fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        // 机制已由 Core 在 bind 时固定；这里只"实现"，不"选择"
        self.backend.block_read(lba, buf)
    }
}
```

### 5.2 C 前端草案（同一份 consumer 代码）

```c
/* 业务代码：与 Rust 版同形，不按 mode 分支 */
static int load_superblock(kcomp_block_device *blk, uint64_t lba,
                           uint8_t *buf, size_t len) {
    return kcomp_block_read(blk, lba, buf, len);   /* typed 前端 */
}
```

### 5.3 provider 的按部署本地入口

业务后端只有一份；每个部署提供一个**本地入口 / adapter**（运行环境提供，业务后端不重写）：

```c
/* 业务后端：provider 的真实实现，不感知部署 */
int32_t virtio_block_read(void *state, uint64_t lba, uint8_t *buf, size_t len);

/* Direct 部署：SDK 生成的 repr(C) function table 直接指向 adapter，
   adapter 收窄入参后调业务后端（现状 `BlockDeviceService` 就是这个形状） */
static const kcomp_block_device_api BLOCK_API = {
    .capacity_sectors = adapter_capacity,
    .read             = adapter_read,      /* → virtio_block_read */
    .write            = adapter_write,
};

/* Gate / syscall 部署：Core 调用的 image 级统一入口 */
int32_t kcomp_service_dispatch(void *instance_state, uint32_t port,
                               uint32_t method, const kcomp_call_frame *frame) {
    if (port != PORT_BLOCK) return -ENOSYS;
    return block_dispatch(instance_state, method, frame);  /* → virtio_block_read */
}
```

> 现状锚点：`kcomp_service_dispatch` 已经是组件 ABI 的**可选** image 级入口（`abi/component.toml:114-142`），`kcore_endpoint_call` 经它分派（`os/core/src/component/call.rs`）。Direct 的 function table 形状见 `BlockDeviceService`（`os/components/kcomp-sdk/src/block.rs:114-167`）。

### 5.4 扁平调用帧（args / input / output）

Gate / syscall 的调用帧是**扁平**的（`abi/component.toml:40-77`）：

```text
KcompCallFrame（kcomp_call_frame）
  args    : *const u8   args_len   : usize    ← 标量参数区（SDK 编解码，Core 不解析）
  input   : *const u8   input_len  : usize    ← 输入负载（只读）
  output  : *mut   u8   output_len : usize    ← 输出负载（可写）
```

- 六个字段**全部指针宽**（`size_ptrs = 6`）：RV32 为 24 B、RV64 为 48 B；同一 ISA 内按字段跨执行域搬运，不能跨 XLEN 直接复制结构；**没有嵌套 raw pointer**，标量参数编码在 `args` 的扁平字节区里。
- `method` / `port` 是独立标量参数（`kcomp_service_dispatch`），Core **从不解释**语义。
- Direct 路径**不用这个 frame**：直接调 function table，参数就是普通的 C 参数。这正是 Direct 快的原因。

---

## 6. artifact / import 可移植方案

四层要分开看，**不要**把它们压成一层：

| 层 | 是什么 | 现状锚点 |
|---|---|---|
| **artifact 文件** | `.kcomp` = ET_REL，语言无关；UNDEF 只允许 `kcore_*` | `tools/kcomp-link.sh`、`os/core/src/component/loader.rs` |
| **已加载组件程序（loaded）** | 每次 instantiate 都重新放段 + 重定位：`base` / `create` / `destroy` / `service_dispatch` / `text_size` / MemoryLease；由 `ComponentRecord` 1:1 拥有 | `loader.rs`、`registry.rs` |
| **运行组件** | `ComponentId` + `state` + 归属 | `os/core/src/component/registry.rs` |
| **按域的映射** | 一个组件放进某执行域时的 VA 布局 + import 解析；每次 instantiate 重新按域放置 | 按域 VA 布局**已实现并接线**（`isolated_load.rs` / `isolated_lifecycle.rs`）；按域 import 解析**支持面已实现**（诊断 / 只读、panic、私有 backing 与 endpoint API），更宽的 import 面未实现 |

### 6.1 `kcore_*` import 在三种部署下如何解析（目标）

```text
Native    ：直接符号地址（现状：loader 重定位到 export 白名单解析出的函数地址）
            → loader.rs:337-341（export::resolve）
Isolated  ：支持面 import 解析到共享 Core 低别名 → 普通 C-ABI 直接调用（satp 不变）
            → 已实现（isolated_load.rs::SUPPORTED_IMPORTS）；更宽的 import 面未实现
Sandbox   ：syscall stub（自有稳定 wire ABI，ecall 进 Core）
            → 未实现（wire ABI 设计见 driver-model.md §6.4）
```

### 6.2 text / data / bss / instance-state 的 VA 与重定位

- 现状（KernelNative）：**每次 instantiate 都重新放段 + 重新应用重定位**（`loader.rs`，返回 `LoadedComponent`，存进本组件的 `ComponentRecord`）；段放置只提供段内对齐、**没有页级权限分离**；import 对每次放置各解析一次；`create` / `destroy` / `service_dispatch` 是绝对 `usize`，运行时被 transmute 成函数指针（`os/core/src/component/containment.rs:906,913`）。`.text` / `.rodata` 物理去重尚未做（未来 loader / MM 优化）。
- **Isolated 生命周期 + 跨域 service Gate + 失败/重启矩阵已接线**：`os/core/src/component/isolated_load.rs` 按域重新放段——每个 ALLOC 段拿到**自己的页对齐范围**（text = R+X、rodata = R、data/bss = R+W），并按域 base 重新应用重定位（复用 `loader.rs` 的私有 ELF API，绝不复用 KernelNative 放段结果）；`os/core/src/component/isolated_lifecycle.rs` 创建私有 AS、落镜像、预置**组件栈 + 实例内存窗口**，经 跨 AS trampoline 执行 `kcomp_instance_create` / `kcomp_instance_destroy` 与 `kcomp_service_dispatch`（KernelNative caller → Isolated provider），任一步失败即退役 AS + 归还预置窗口 + `Failed`。由 ArchTest 在 RV64 + RV32 QEMU 端到端证明（`isolated-lifecycle` / `isolated-lifecycle-fail` / `isolated-service` / `isolated-service-fault`）：组件在私有 AS 里跑过、ABI 交窗口（args / config / out_state，串行布局见下）正确、新上下文以 `tp == 0` 进入（`tp` 是普通架构 / 任务执行状态，不是组件运行时指针）、窗口只在该实例 AS 里可达、destroy 入口真的执行、Core AS 每次切换后恢复；跨域调用的扁平帧**直接**交付（caller 是 KernelNative，共享 Core 映射让 `frame` / args / input / output 在 provider 的 AS 里 same VA → same PA 直接有效——无拷贝、无中间页）、provider 原地读写 caller 缓冲、provider 故障被 普通 Core trap 路径收敛（caller 拿到类型化错误、实例 `Failed` + AS 退役）。
- **失败 / 重启矩阵**：每个阶段的失败都有可观察终态与清理——放段失败 / import 白名单外符号在**声明实例之前**拒绝（`isolated-load-reject`）；config 超窗口固定区在 create 入口执行前拒绝（`isolated-config-reject`）；create 入口返回非零 / trap（`isolated-lifecycle-fail` / `isolated-lifecycle-fault`）与 service dispatch 故障（`isolated-service-fault`）都走"Core 中止实例"：退役 AS + **解映射并归还**预置窗口（栈 / 窗口）+ `Failed` + endpoint 永久失效；destroy 入口 trap 走"实例生命尽头"路径（`isolated-destroy-fault`）：`DestroyPanicked` + `Failed` + 退役 AS，**窗口按 phase 1 契约保持驻留**（AS 退役后不可进入），且**绝不重试析构**；`isolated-prepare-reject` 钉住 `isolated::prepare` 的窄拒绝（入口不可执行 / 栈不可写 / AS 已退役 → 类型化错误，且不改变实例真相）；`isolated-stale-access` 证明已解析但已死的 endpoint 在 Core 边界被拒（provider 从未再次执行）；`isolated-ready-fault` 证明"已经服务过调用"的实例故障后仍被完整收敛；**重启 = 重新 instantiate**（`isolated-restart`）：前一个组件 `Failed` / `Stopped` 之后，同名 artifact 创建**全新组件**——全新按域放置、全新私有 backing、全新 AS / 全新预置窗口；同一 artifact 的**多个并发组件合法**（各自独立 backing + AS）。**支持面 import 解析已实现**（诊断 / 只读、panic、私有 backing 与 endpoint API，解析到共享 Core 低别名、普通 C-ABI 直接调用、`satp` 不变）；更宽的 import 面（共享堆、调度入口、组件创建、设备 / DMA / IRQ）仍显式拒绝，Isolated provider 可自行 staged publish，K/I caller 均可通过 SDK 发起调用。
- **Isolated 的内存路径（Core 预置窗口，窄 import 面）**：Core 为每个实例预置一块**实例内存窗口**（Core backing、零初始化、只映射在该实例的私有 AS），以固定串行布局交付 create args / `out_state` / config——`+0` `KcompCreateArgs`、`+32` `out_state`(`usize`)、`+64` config payload（`WINDOW_CONFIG_MAX = 256`）、`+320` 域 `MemoryView`（24 B）、`+352` runtime 部署描述符（RV64 24 B / RV32 16 B），窗口大小 4096。旧 `+64` 的 64 字节 "runtime context block" **已删除**；全新同步进入的 Isolated 上下文观察到 `tp == 0`（`tp` 只是架构 / 任务执行状态，见 `docs/modules/arch.md`）。Core 以 **`kcore_memory_view` 编码**（`kind = LOCAL_VA`、`base/len` = 本窗口）把该域视图预交付给实例。表示是**实例内 VA**（`view.base/len` 只在那个 AS 里有意义，归属由该实例的页表承载；绝不出现物理地址 / Core 私有 VA），ArchTest `isolated-lifecycle` 断言编码与「窗口只在该实例 AS 里可达」。`kcore_memory_acquire/release` 已加入 Isolated import 面：按调用身份取得实例 AS，动态 backing 放在 `0x23000000..0x2f000000`，release 只接受该窗口内的精确 acquire 映射。SDK runtime 在业务 create 前初始化，使同一个工件的分配自动选择 K 共享堆或 I 私有堆（见 `memory-and-heap.md` §6.1）。Isolated 仍是 S-mode 普通直接调用；ecall 属于后置的 SandboxedNative。
- **跨域 service 传输**：K caller 保留共享 Core 映射下的直接帧/缓冲交付。I caller 经 `isolated_call` 检查范围权限、搬运 args/input/output、切到独立 Core 栈与挂起的 Core root，再按 provider 域分派；返回后恢复 caller root，传输成功时写回 output/status。payload 是不透明字节，嵌套指针由 SDK adapter 编码；provider 不得保留本次借用。没有固定长度上限，资源不足返回 ENOMEM。
- **同一 artifact 能否按域重定位？** 可以，但**必须按域重新放段 + 重新解析 import**（新 `base`、新 import 目标）。Isolated 侧已具备"按域重新放段 + 重定位 + **支持面 import 解析**"（支持面 = 诊断 / 只读查询、panic、私有 backing 与 endpoint API，解析到共享的低别名，运行时是普通 C-ABI 调用、`satp` 不变）；KernelNative 侧每次 instantiate 重新放段 / 重定位、完整导出面。
- **text 何时可跨域共享？** 只有当**重定位后的 text 字节完全相同**时才能共享可执行页，即：**same VA**（同一段虚拟地址）+ **same import-target VA**（import 在两端解析到**同一 VA**）。共享 Core 映射已让支持面 import 在**每个 Isolated AS 里解析到同一低别名 VA**（same VA → same PA），因此支持面 import 的 same import-target VA 条件天然成立；更宽的 import 面若引入按域不同的目标，才需要额外的固定 VA 机制。做不到这两条，就必须按域各自放段 / 重定位，**不能共享 text**。
- **instance-state**：`kcomp_instance_create` 返回的 opaque state 是**实例**私有、不是 image 共享；它由该实例自己的分配器分配，backing 以 **region 粒度**由 Core **提供**（Core 不记 owner、无账本；backing 经 `kcore_memory_acquire/release` 交付。契约见 `memory-and-heap.md`）。**Component 身份 = `ComponentId` + `ComponentRecord.instance_state`**，不与分配器 / runtime 状态合并。堆是 **runtime / deployment 策略**、不是组件一等资源：KernelNative 可共享 Core 内核堆，私有执行域可在自己的可写 `.data` / `.bss` 保留私有分配器。

> **不要声称一个 `mode` 字段就能实现这些。** 一个字段只表达"意图"；上面每一条都需要真实机制（§7 的依赖排序缺口）。

### 6.3 依赖排序的缺口清单

按依赖顺序，前者不成立后者无从谈起：

1. **执行域字段 + 私有 AS 运行时**：`ComponentRecord::execution_domain`（已落地）、私有地址空间、`satp` 切换、ASID、U-mode、`ecall` 处理。
   （**私有 AS 切换机制已落地**：最小跨 AS trampoline（共享 Core 映射） + Core 侧准备 + 窄故障分派；**组件生命周期已接入**（create / destroy 都经跨 AS trampoline），**K/I 双向 service 已接入**（I 出站经 Core 栈/root 桥接，I provider 经跨 AS trampoline 执行 dispatcher），**失败 / 重启矩阵已逐条证明**（含 destroy 故障、stale 阻断与重新 instantiate（重启），见 §10）；**ASID / U-mode / `ecall` 仍未实现**。）
2. **loader 按域放段 / 按域 import 解析**：每次 instantiate 都按域重新放段 + 重定位（`component/isolated_load.rs` / `isolated_lifecycle.rs`）；`kcomp_service_dispatch` 已按域解析成实例域 VA；**没有 same-image backing 复用**——同一 artifact 的多个组件（含并发 Isolated）各自全新 backing / AS。按域 **import** 解析的**支持面已实现**（诊断 / 只读查询、panic、私有 backing 与 endpoint API，解析到共享 Core 低别名、普通 C-ABI 直接调用、`satp` 不变）；更宽的 import 面未实现。KernelNative 侧同样是每次 instantiate 重新放段。
3. **per-domain 本地入口 / adapter**：Gate 的 image 级入口 `kcomp_service_dispatch` 已按域调用（KernelNative = 共享 AS 内 Core 栈、Isolated = 私有 AS 内经跨 AS trampoline）；Direct 的按域 function table 与其它 transport 仍未接线。
4. **组件支持范围元数据**：manifest **没有**任何字段声明组件支持哪些部署（`os/core/src/component/store.rs` 只解析 `manifest` 文本 + 组件条目）。
5. **平台能力声明 + 拒绝语义**：MMU / IOMMU / 特权级 / 私有 AS 是否具备，以及"不具备就拒绝"的路径（`driver-model.md` §11 的能力诚实表）。

---

## 7. 已有 / 目标 / 缺口（逐条，带 `file:line`）

> 以下事实来自已完成的源码审计，**不软化**。

### 7.1 执行域与隔离

| 项 | 已有 | 目标 | 缺口 / 证据 |
|---|---|---|---|
| 私有地址空间 | **已接线**：按域放段、共享 Core 映射、私有 backing 别名排除、跨 AS trampoline、生命周期与 K/I 双向 Gate；异常由普通 trap 路径归因 | ASID / U-mode / Sandboxed 执行器、任务与 DMA teardown 仍未实现 | `memory/{address_space,kernel_mappings}.rs`；`component/{isolated,isolated_call,isolated_lifecycle}.rs`；`arch/src/riscv/trampoline/` |
| 上下文切换 | 普通 Task 保存 `ra/sp/s0-s11/tp` | 含 `satp` 切换 | `os/arch/src/riscv/cpu.rs` 的 `RiscvContext`（不含 satp，IRQ 使能由 Core 执行流状态另存）；跨 AS 进入走独立的最小 trampoline（`arch/riscv/trampoline/`，每次调用独立 `Context`：调用者 `satp` + ABI 现场按需保存 / 恢复，`satp` 只在目标与调用者不同时切换 + 全量 `sfence.vma`）；普通任务切换仍用 `__switch`（`RiscvContext`） |
| `activate()` | 写 satp + sfence，**运行期无人调用**（boot 的 runtime root 除外） | 按域激活 | `os/arch/src/riscv/mmu/mod.rs`；boot 的 runtime root 在 `kernel::init` 后 activate；Isolated 切换由 `trampoline` 直接消费 `PreparedActivation` 的预打包 satp，ASID 恒 0 |
| U-mode 组件后端 | **SandboxedNative 组件未实现**；RV64 普通用户 Task 的 U-mode / trap 已接线 | U-mode 组件 import、heap、生命周期与服务授权 | 普通用户路径 `os/core/src/task/user.rs`，personality 见 [POSIX 模块](../modules/posix.md)；不能从它推导 `.kcomp` Sandbox 可用 |

### 7.2 重新放段与入口

| 项 | 已有 | 目标 | 缺口 / 证据 |
|---|---|---|---|
| 每次 instantiate 重新放段 + 重定位 | **已实现**：KernelNative 每次 instantiate 走 `loader.rs`，Isolated 走 `component/isolated_load.rs`（按域放段 + 页级权限分离 + 按域重定位，create / destroy / **可选 `kcomp_service_dispatch`** 都解析成实例域 VA），`isolated_lifecycle.rs` 消费它（ArchTest 在 RV64/RV32 证明）；同一 artifact 可并存多个组件，各自独立 backing / AS | 更宽的按域 import 面 | KernelNative：`loader.rs`（入口是绝对 `usize` transmute 成 fn 指针，`containment.rs:906,913`）；Isolated：`isolated_load.rs` / `isolated_lifecycle.rs`、ArchTest `isolated-image` / `isolated-perm-*` / `isolated-lifecycle` / `isolated-service*` |

### 7.3 部署 / 模式字段

| 项 | 已有 | 目标 | 缺口 / 证据 |
|---|---|---|---|
| 部署字段 | **已实现** | 组件实例 / 部署域记录 | `ComponentRecord::execution_domain` 落地 + 创建入口按域分派（见 §10）；`KernelNative` / `IsolatedNative` 两域可执行，`SandboxedNative` 是 Core `sandbox.rs` 骨架（create 装载前 `-ENOTSUP`，U-mode / destroy 未实现） |

### 7.4 consumer ABI 校验

| 项 | 已有 | 目标 | 缺口 / 证据 |
|---|---|---|---|
| 组合期 exact ABI | **SDK 消费路径已实现** | consumer ABI 与 provider 逐位相等 | `lookup` 只发现 id，SDK `Endpoint<C>::from_id` 经 `kcore_endpoint_validate` 核对 exact ABI；bind 再校验契约与存活。裸 C consumer 也须 validate/bind |

### 7.5 ABI 文档与重入

| 项 | 已有 | 目标 | 缺口 / 证据 |
|---|---|---|---|
| `kcore_endpoint_call` 文档 | **覆盖 K/I** | 与代码一致 | `abi/core.toml` 与生成物描述 K/I 执行栈、Core root 桥接、扁平缓冲、借用与错误语义；`call.rs` / `isolated_call.rs` 是实现 |
| 重入 | **链成员门禁 + I 单实例并发门禁** | 可选嵌套深度上限 | A→B→A 在进入前拒绝；I provider active_calls 非零时拒绝第二次进入，判断/计数在同一 registry 事务内；无固定嵌套深度上限 |

### 7.6 DMA 归属（三条不同规则）

| 操作 | 归属规则 | 证据 |
|---|---|---|
| `alloc` | ambient caller | `os/core/src/resource/dma.rs:273,280` |
| `map` | 锚在 **device owner** | `dma.rs:304,320-329` |
| `unmap` | **不解析 caller** | `dma.rs:339-348` |

### 7.7 冻结文档冲突

`docs/architecture/component-lifecycle.md` §9 **明确否认**"一个链接好的 KernelNative 二进制能在任何执行域原样运行"是 ABI / 生命周期承诺。本文件补充其目标方向（不改其现状描述）。

### 7.8 组件的部署支持范围（无元数据可声明）

| 组件 | 能否 Sandbox | 原因 | 证据 |
|---|---|---|---|
| `virtio_blk` | **否（按现状）** | 需要裸 MMIO | `os/components/drivers/virtio_blk/src/lib.rs:302,392`（volatile 读写寄存器） |
| `core_test` | **否（按现状）** | 需要裸 MMIO | `os/components/tests/core_test/src/runtime/resource.rs:56,61,187-188` |
| `kbench` | **否（按现状）** | 需要裸 MMIO | `os/components/kbench/src/irq.rs:104` |
| `driver_prober` | **否（无 Core 侧 broker 时）** | 需要 Core-authority 操作：组件加载 / 任务创建 / `device_nth` | `os/components/driver_prober/src/runtime.rs:149,178,249` |
| `scheduler_rr` | **原则上可以** | 纯逻辑，只走服务 | `os/components/scheduler_rr/src/lib.rs` |
| `fatfs` | **原则上可以** | 纯逻辑，只走服务 | `os/components/filesystems/` |

> **今天没有任何元数据能声明一个组件的支持范围。** manifest 没有相关字段（`os/core/src/component/store.rs`）。要 Sandbox 一个需要裸 MMIO 的组件，必须先把它改成经 Core 窗口访问，或加 Core 侧 broker——这属于 §6.3 的缺口 4。

---

## 8. 已收敛边界与后续缺口

旧“全局名字 → 单 binding”实现已删除，Endpoint 是唯一发布真相。lookup 只发现
contract 与存活；validate / bind 执行 exact ABI 校验。无需给 lookup 再加一份校验参数。
SDK typed 前端与 K/I 调用后端均已接通，不能再列为待迁移步骤。

当前剩余工作按 §7 依赖顺序推进：支持范围元数据、私有域任务 / 设备、真实低特权
transport、AS 并发回收与 DMA 静默条件。每项先有消费者与验证，再扩展支持面。
业务发现 / provider 选择可以外置，但现有目录有 init、ksh 和 SDK 消费者；没有迁移
证据时保留简单索引，不复制成第二个长期 Registry。收敛审计与实验见
[Core convergence](../development/core-convergence.md)。

---

## 9. 验收设计

### 9.1 host（`make test-host`）

- **部署 / 绑定矩阵**：对每个 `(caller domain, callee domain)` 组合断言**合法机制**；**不支持的组合显式拒绝**（返回明确错误码，**绝不静默降级成 Direct**）。
- **exact ABI**：consumer ABI 与 provider 不一致 → 拒绝 bind（SDK lookup 后的 validate 与 bind）。
- **staged publication**：`kcomp_instance_create` 期间 publish 只记 pending，create 返回 0 后 Core 原子提交。
- **多组件**：同一 artifact 两个组件，独立 loaded backing（`.data` / `.bss` 不共享）/ state / owner / endpoint。
- **invalidation / no-redirect**：provider 停止 / 失败后 endpoint **永久死亡**，**绝不重定向**到新实例。
- **attribution**：DMA `alloc` / `map` / `unmap` 归属按 §7.6 的三条规则。

### 9.2 Native（QEMU，RV64 + RV32）

- **真实 C / Rust 链**：consumer 经 typed 前端调 provider 的**真实**实现。
- **稳态 direct call**：typed 前端直接走 function table，**不经 `kcore_endpoint_call`**，**不分配 service stack**（用 trace / 计数器证明）。
- **RV32 / RV64 布局与宽度一致**：`KcompCallFrame` `size_ptrs = 6`（32/64 同布局）；kcore ABI 宽度规则（`overview.md` §5）。

### 9.3 执行域验证

- Isolated 的真实 satp / 栈切换、参数可达性、权限、panic、重入与重启由 QEMU
  ArchTest 的 `isolated-*` 验证；CoreTest 通过公开 ABI 验证 K/I 业务调用组合。
- host fake 不执行组件入口，只证明协议和状态记账；不作为硬件隔离证据。
- Native containment 的寄存器 / 栈切换不是 AS 切换；Isolated 经专用跨 AS trampoline。
- SandboxedNative 尚未实现。U-mode 普通用户程序的测试不等于 Sandbox 组件实现。
- RV64 Gate/Stop 的双 CPU 在途执行验证由 CoreTest 编排，不用私有表模拟并行。

### 9.4 性能

分四段量，**不要混成一个数**：

| 段 | 含义 |
|---|---|
| **bind cost** | 一次绑定的验证 + 提交（含 ABI 比较） |
| **call cost** | 每次调用的固定开销 |
| **AS-switch cost** | 跨域切换地址空间 / 特权级的开销（Gate / syscall 才有） |
| **data-transfer cost** | 参数 / 负载搬运（frame 打包、跨域拷贝） |

**Native 基线 = 裸 function table 直接调用**（无 Core 通用分派、无栈分配、无消息打包）。任何 Gate / syscall 的数字都必须对照这条基线报告。

---

## 10. 实现状态

> **这张表是防止把目标当成现状的唯一防线。**

| 能力 | 状态 | 证据 |
|---|---|---|
| Contract（名字 + exact ABI + repr(C) table） | **已实现** | `abi/block.toml`、`block.rs:68-75` |
| Artifact → Component（一次 instantiate = 一个完整组件，各自独立可写 image） | **已实现** | `registry.rs`（`ComponentRecord.loaded`）、`load.rs`、`loader.rs` |
| Endpoint 真相（publish / discover / invalidate） | **已实现** | `endpoint.rs` |
| Gate 调用边界（per-call stack + principal + panic containment） | **已实现** | `call.rs`、`containment.rs` |
| SchedulerPolicy 专用路径（选择 = `kcore_sched_set_policy`；`PolicyCall` 边界；通用调用拒绝保留契约；vtable + 名字绑定已删） | **已实现** | `sched.rs`、`component/call.rs`、`containment.rs`、`abi/scheduler.toml` |
| `kcore_endpoint_call` 文档与代码一致 | **已做（KernelNative 执行边界）**：abi doc 写"执行边界已落地"并指向 `component/call.rs`；跨域（Isolated provider）语义在同一模块文档与 §3/§10 | `abi/core.toml`（`kcore_endpoint_call` doc）、`component/call.rs` |
| consumer exact ABI 校验 | **已实现** | SDK lookup 后 validate，bind 再做 exact contract + abi + 存活校验；裸 lookup 只发现 id |
| 部署字段（`ComponentRecord::execution_domain`） | **两域可执行**：K 与 I 的生命周期、堆与通用服务均接线；I 自行 staged publish，K/I 双向 Gate 真实切 root/栈。任务、设备/DMA/IRQ 与 Sandbox 仍未实现 | `registry.rs`、`load.rs`、`isolated_lifecycle.rs`、`isolated_call.rs`、`export.rs`、`call.rs` |
| 执行模型 / ISA / runtime 维度（native machine code vs Wasm） | **未开始**，且**不属于 `ExecutionDomain`**——与执行域正交，需**单独维度**表达 | 本文件 §3 |
| 按 `(caller, callee)` 域选机制 | **K/I 矩阵已接线**：K/K Direct；K→I、I→K、I→I Gate。I 的扁平调用帧由 Core 搬运，不交付 provider 裸入口 | `endpoint.rs`、`call.rs`、`isolated_call.rs`、ArchTest `isolated-domain-service` |
| Direct / Gate 作为**绑定机制**分离 | **部分实现**：Direct（KernelNative 同域）与 Gate（跨域 + 调度策略）都在跑；Gate 的 binding 只携带 opaque `EndpointId` + `port`（绝不交付 provider 域内裸入口） | `endpoint.rs::bind`、`component/call.rs` |
| "inflight 只计 Gate" 的契约约束 | **设计完成** | 本文件 §3 |
| Isolated 失败 / 重启矩阵 | **已证明**：逐条覆盖放段失败 / import 白名单外符号 / config 超限 / prepare 拒绝 / create 返回非零 / create 故障 / destroy 故障 / service 故障 / 超容量帧 / Ready 期故障 / stale 访问阻断 / 重新 instantiate（重启）。每个失败断言：文档化终态（`Failed` / `Stopped` / 无组件）、AS 退役或释放、Core 预置窗口归还或（destroy 路径）驻留、endpoint 永久失效、caller 类型化错误、Core 存活、KernelNative 不受影响。**未证明**：生产 create 路径内的 prepare 失败（组件不可注入，只能来自 Core 不变式破坏——机制层拒绝已证明） | ArchTest `isolated-load-reject` / `isolated-config-reject` / `isolated-prepare-reject` / `isolated-destroy-fault` / `isolated-stale-access` / `isolated-ready-fault` / `isolated-restart` + `isolated-lifecycle-{fail,fault}` / `isolated-service-{limits,fault}`（RV64+RV32） |
| 私有地址空间 / `satp` 切换 / ASID | **部分实现（机制落地 + 生命周期 + 跨域 service 已接线；ASID / U-mode 未实现）**：映射生命周期 / 精确查询 / 退役状态 / 激活描述符（`prepare_activation`）**加上最小跨 AS trampoline（共享 Core 映射）**（Core 侧 `prepare` + arch 汇编进入 / trap 往返 / 恢复 / 放弃、窄 Core 故障分派钩子）已落地；ArchTest 在 **RV64 + RV32 QEMU** 证明「Core → 私有 AS → Core 往返」「私有 AS 内时钟中断在 Core AS / Core trap 栈处理后恢复」「组件页故障可恢复 / 可放弃」。**按域放段**已落地（`isolated_load.rs`：页级权限分离、按域重定位、显式拒绝），ArchTest 证明真实 `.kcomp` 在私有 AS 里执行、段数据可读、页表按段权限强制（写 R+X text → scause 15 / 取指 R+W data → scause 12）、别的实例的私有映射不可达（共享 Core 映射本身可达）。**组件生命周期已接线**（`isolated_lifecycle.rs`：Isolated 的 create / destroy 经跨 AS trampoline 在私有 AS 里执行，失败即退役 AS + 归还预置窗口）；**跨域 service Gate** 让 KernelNative caller 经 Core call gate 调用 Isolated provider 的 `kcomp_service_dispatch`（扁平帧经共享 Core 映射直接交付，provider 原地读写 caller 缓冲；故障由普通 Core trap 路径收敛成 `Failed` + AS 退役）。`activate()` 仍无生产调用方，**ASID 恒 0 + 全量 `sfence.vma`**（不实现 / 不声称 ASID 分配复用）；这是**协作式**边界（S-mode 可直接改 satp），不是对抗隔离 | `memory/address_space.rs`、`component/isolated.rs`、`component/isolated_load.rs`、`component/isolated_lifecycle.rs`、`arch/src/riscv/trampoline/`、`arch/src/vm.rs`、`riscv/mmu`、ArchTest `isolated-*` / `isolated-lifecycle*` / `isolated-service*` |
| U-mode / `ecall` | **未开始** | `supervisor.rs:63-66`（`UserEnvCall` panic） |
| 每次 instantiate 重新按域放段 | **已实现并接线**：`component/isolated_load.rs` 按域放段 + 重定位 + 页级权限分离（create / destroy / 可选 `kcomp_service_dispatch` 解析成实例域 VA），`isolated_lifecycle.rs` 每次 instantiate 全新放置到全新私有 backing + 全新私有 AS；同一 artifact 可并发多个 Isolated 组件（各自独立 backing / AS）。ArchTest `isolated-image` / `isolated-perm-*` / `isolated-lifecycle` / `isolated-service` / `isolated-restart` / `isolated-ready-fault` 在 RV64+RV32 证明。**按域 import 解析的支持面已实现**（诊断 / 只读、panic、私有 backing 与 endpoint API），更宽的 import 面未实现 | `isolated_load.rs`、`isolated_lifecycle.rs`、`load.rs::validate_isolated_load` |
| `kcore_*` import 的 Isolated / Sandbox 解析 | **I 支持面**：诊断、只读、panic、私有 backing acquire/release、endpoint publish/lookup/validate/bind/call；共享 heap、任务、设备/DMA/IRQ、组件创建仍装载前拒绝。Sandbox 无 native 导出面 | `isolated_load.rs::SUPPORTED_IMPORTS`、`isolated_call.rs` |
| 组件支持范围元数据 | **未开始** | manifest 无字段 |
| 重入嵌套深度上限 | **未开始** | 只有链成员门禁 |

> **IsolatedNative 的诚实边界（本阶段，不放大）**：
> - **协作式、非对抗**：S-mode 组件与 Core 同特权级，可直接改写 `satp` / `stvec` / 自己的映射；共享 Core 映射意味着组件也能到达 Core 内存。真正的强制边界是未来的 U-mode（SandboxedNative），**未实现**。
> - **ASID 恒 0 + 全量 `sfence.vma`**：不实现 / 不声称 ASID 分配复用；`satp` 只在目标 AS 与调用者不同时切换。
> - **import 支持面**：诊断 / 只读查询、`kcore_panic_escape` 与私有 backing acquire/release；共享堆、调度入口、组件创建、设备 / DMA / IRQ 获取仍显式拒绝（`-ENOTSUP`）。
> - **出站通用 dispatch 已接线**：I→K / I→I 经 Core 栈/root 桥接。当前是同步、不可 yield 的调用；循环重入被拒绝，尚无 Isolated 任务调度、跨 CPU 服务调度或远端 TLB shootdown。
> - **回收边界**：create / service 故障归还 Core 预置窗口 backing；destroy 路径（含 destroy 故障）只退役 AS，窗口 backing 驻留（AS 退役后不可再进入，无页表 teardown）；KernelNative 失败只保证逻辑失效 / 物理驻留。同一 artifact 的多个组件各自独立 `.data` / `.bss`（每次 instantiate 独立放段）。

今天真实可执行的是 KernelNative 与协作式 IsolatedNative；共享/私有堆和 K/I 通用服务均可用同一 SDK 业务代码与工件。Sandbox、Isolated 任务/设备、自动物理回收与 ASID 仍是缺口。

---

## 11. 相关文档

- `docs/architecture/overview.md`：分层、ResourceDomain / ExecutionDomain 概念、三个信任域；
- `docs/architecture/component-lifecycle.md`：组件生命周期与组件契约（本文件补充其 §9 的目标方向）；
- `docs/architecture/component-model.md`：Interface / ResourceDomain / 依赖图；
- `docs/architecture/driver-model.md`：执行域模型、能力诚实表、未来 syscall wire ABI；
- `docs/architecture/kconfig.md`：backend 能力来自 Kconfig；部署要求来自组合配置，Core 验证并提交；
- `docs/development/testing.md`：host / CoreTest / QEMU 验证策略；
- `docs/development/benchmark.md`：性能分段与基线。
