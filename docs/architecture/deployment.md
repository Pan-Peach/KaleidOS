# 部署与绑定：执行域约束交互机制（deployment.md）

> 本文件规定部署能力、Core 验证与绑定边界；支持范围以 §10 与源码为准。

普通业务使用 Endpoint Request/Reply：K 经 Core C ABI，I 经 Core 栈/root 桥接，
U 经私有 syscall thunk/ecall。IPC-only 发布 `port=0, api=NULL, ctx=NULL`，bind
验证 exact/live 后返回 Ipc=2 与零 api/ctx；原始 IPC 仍须显式 grant。
RV64 S/MMU 支持 K/I/U 九格，RV32 S/MMU 支持 K/I 四格；RV32 U 与无 MMU
私有域显式拒绝。Provider 在自己的 Task/AS 下处理 Core 副本。详见 [IPC](ipc.md)。

下面的 Direct/Gate 矩阵只描述保留的同步策略/诊断，不允许普通业务新增这些路径。
Contract/Handler 可复用；不同 ISA、特权级、import 面不保证二进制天然兼容。
身份和入口 ABI 以 [生命周期](component-lifecycle.md) 为准。

---

## 1. 分离概念与职责图

本节至§3的 Direct/Gate 图描述仍受 Core 支持的同步策略/诊断机制；普通业务的单一 IPC 路径见§4–5。

组件实例、执行域、接口契约与交互机制分别描述身份、部署环境、语义与双方关系。
Artifact 是程序字节，Endpoint 是一次发布；它们不另建组件生命周期。

Service、Transport、Inline/Queued 与业务 Session 的职责分离以
[服务执行契约](service-execution.md) 为准。这里的 Gate 是同步调用边界，不代表独立
Server Task；部署决定合法调用窗口，不代替 provider 的并发、等待与对象失效语义。

| 概念 | 回答什么 | 身份 / 载体 | 代码锚点 |
|---|---|---|---|
| **Contract** | 这个服务**语义**是什么 | 接口名 + exact ABI fingerprint + 方法/wire；历史发布另有 typed function table | `abi/block.toml`、`os/components/kcomp-sdk/src/block.rs:68-75` |
| **Artifact** | 一个**组件程序字节**（不是运行实例） | `.kcomp`（ET_REL）；artifact 名 | `tools/kcomp-link.sh`、`os/core/src/component/loader.rs` |
| **Component** | **一个完整运行组件**：instantiate 后拥有自己的已加载程序 | `ComponentId`；`loaded`（`base` + `create` / `destroy` / `service_dispatch`）+ 资源归属（device / irq / dma / task / publication） | `os/core/src/component/registry.rs`、`os/core/src/component/load.rs` |
| **Endpoint** | provider **发布的服务点** | `EndpointId`（provider ComponentId + port_name + contract） | `os/core/src/component/endpoint.rs` |
| **Execution Domain** | 在什么特权与地址空间环境执行 | Component 的部署属性；Core 保存域与 AS | 本文件 §6、§7 |
| **Interaction Mechanism** | 这次交互怎么执行 | Direct / Gate 是两端的关系；bind 按两端域约束选定，不是整个组件的唯一属性 | 本文件 §2、§3 |

**核心判断（不可违背）：**

- **Core owns the execution-domain truth。** 组合器（composer / profile）**提议**部署（哪个组件跑在哪个执行域），Core **验证并提交**。Core **不硬编码信任级策略**。统一业务 wire 不等于扩大某个域的执行能力。
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
     输出：IPC-only 发布返回 Ipc；同步诊断按 Direct | Gate | rejected
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

> **实现状态**：K/K Direct（及显式 Gate）、K→I、I→K、I→I Gate 均已接线；Sandbox 参与的同步 Gate 调用仍显式拒绝；普通 IPC 支持表见 §10。ArchTest `isolated-domain-service` 在 RV64/RV32 以同一工件、专用 test-only `domain.test` 同步前端验证四种组合、嵌套、panic、stale 与循环重入。

| caller ↓ \ callee → | KernelNative | IsolatedNative | SandboxedNative |
|---|---|---|---|
| **KernelNative** | **Direct**（可显式 Gate） | **Gate** | **rejected** |
| **IsolatedNative** | **Gate** | **Gate** | **rejected** |
| **SandboxedNative** | **rejected** | **rejected** | **rejected** |

这是同步诊断矩阵。I/I 不交付裸指针；U 不支持 Gate/Direct。
普通 IPC 不使用此表：RV64 的真实 U runner/ecall 已接线，无 native fallback。

同一个实例可以零 Task、多个 Task、Direct 与 Worker 共存；Worker 请求编码、
队列、reply、取消和业务同步属于组件 / Runtime。Gate 的同步调用栈不可 yield，
不能把它与可阻塞的 Task Request/Reply 当成同一执行上下文。Wasm 是未来执行后端，
不是第四个 ExecutionDomain。已有有界Endpoint Exchange；Core不管理通用业务RPC语义。

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

**`inflight` 计 Core 管理的 lifecycle、Gate、policy 与 IRQ 执行。** Core 的 endpoint `inflight` 计数在**经 Core 边界** 的执行上递增；Direct 绑定**完全绕过 Core**，Core 看不到。因此：

> **"inflight == 0 → 可以 stop（否则 -EBUSY）"永远不能证明"所有调用都已结束"。** 它只证明"没有正在进行的 Core-managed 调用 / 回调"。若存在 Direct 绑定，consumer 手里那张 function table 的调用对 Core 不可见——这正是 KernelNative 无隔离 + 物理驻留的直接后果。teardown 的正确性不能仅建立在 `inflight` 上。当前 Native 实例发布非空 Direct 表即保守拒绝 Stop/destroy（EBUSY），旧表、ctx 保持驻留；没有 Direct release 协议。

---

## 4. 三个角色与目标调用链

三个角色**保持极小，不为每个角色造框架 / crate**：

| 角色 | 是什么 | 位置 | 约束 |
|---|---|---|---|
| **服务前端（typed frontend）** | consumer 看到的强类型入口，如 `block.read(lba, &mut buf)` | SDK（`os/components/kcomp-sdk/src/block.rs` 一类） | **域无关**；**不持有裸可调用物**；业务代码**永不见** method number / frame / mode 分支 |
| **业务后端（business backend）** | provider 的**真实**实现，如 VirtIO 读盘 | provider 组件内部 | 被**各部署的本地入口 / adapter** 调用；自身**不感知**部署 |
| **调用后端（call backend，在 SDK）** | Core 在 bind 时**已固定**的机制 | SDK 私有 | 普通 SDK 仅持固定 Endpoint；保留的同步诊断才持 api/ctx 或 Core call-gate handle |

**普通业务调用链：**

```text
consumer → typed SDK Binding → generated client / Wire
  → Core Endpoint / Exchange → owned Server Task
  → generated dispatch → Provider 业务 Handler → reply
```

普通 `.kcomp` 边界共用一套稳定 Wire；组件内部可直接调用普通函数或 Rust trait。
Core 只管理 endpoint、权限、请求终态、执行域和调度，不解析业务方法或对象。
同步 PolicyCall 和保留的隔离/生命周期诊断使用下文历史调用矩阵；没有普通业务 SDK Backend 分支。
后续同信任域优化必须复用同一 Contract 和 Handler；当前未实现透明 local dispatch。

---

## 5. 当前 BlockDevice 例子

契约及 exact fingerprint 的唯一权威是 [block.toml](../../abi/block.toml)：
`capacity_sectors` / `read` / `write`，单位512字节。BlockDeviceApi 与 BlockDeviceService 已删除。

Rust consumer：

```rust
let block = BlockBinding::connect(endpoint)?;
block.read(lba, &mut buffer)?;
```

C consumer：

```c
struct kcomp_block_binding block;
int32_t rc = kcomp_block_bind(endpoint, KCOMP_BLOCK_DEVICE_CONTRACT,
                            KCOMP_BLOCK_DEVICE_ABI, &block);
/* rc == 0 后，普通真实 Task 可调用；composer 另行 grant send rights。 */
struct kcomp_call_result result = kcomp_block_read(&block, lba, buffer, len);
```

两语言 facade 都只保存固定 endpoint，非零512倍数与 last-LBA checked overflow 在提交前验证；
多块拆分成512字节消息。业务错误与传输错误分开，后续扇区失败不回滚先前完成的 I/O。
Provider 只实现 generated Provider 的业务方法，发布 IPC Endpoint 并启动 Server；
VirtIO/RAM 的 Server 复用 SDK `block::server::serve`。DLL式函数表、业务 Gate dispatcher 均不再需要。

保留的同步诊断使用 `KcompCallFrame`（定义见 [component.toml](../../abi/component.toml)）：
三个指针/长度对由 Core 跨 AS 验证搬运；它不再是普通 Block/FS 协议的兼容通道。
这些真实硬件诊断必须保留，直到 IsolatedNative 持久 Task/IPC 替代通过。

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

已实现支持面：K 链接普通 Core C ABI；I 白名单由 `isolated_load::SUPPORTED_IMPORTS`
限定，Task/IPC 经 `isolated_api::on_core` 切到真实 Core root/栈；U 白名单由
`sandbox::IMPORTS` 限定，loader 将符号解析到 USER RX thunk。
U thunk 使用 t0 放调用编号，保留 a0..a7，因此覆盖现有七/八参数 C ABI，
不新增业务 wire 或传输描述符。`export/sandbox.rs` 调用已有 Core API。
I/U 拒绝共享 Core heap、设备/DMA/IRQ 与组件创建 import；U 也拒绝 Gate。
私有 heap 使用各实例 image 中的 HeapState 与本域 backing。

### 6.2 text / data / bss / instance-state 的 VA 与重定位

- 现状（KernelNative）：**每次 instantiate 都重新放段 + 重新应用重定位**（`loader.rs`，返回 `LoadedComponent`，存进本组件的 `ComponentRecord`）；段放置只提供段内对齐、**没有页级权限分离**；import 对每次放置各解析一次；`create` / `destroy` / `service_dispatch` 是绝对 `usize`，运行时被 transmute 成函数指针（`os/core/src/component/containment.rs:906,913`）。`.text` / `.rodata` 物理去重尚未做（未来 loader / MM 优化）。
- **Isolated 生命周期 + 跨域 service Gate + 失败/重启矩阵已接线**：`os/core/src/component/isolated_load.rs` 按域重新放段——每个 ALLOC 段拿到**自己的页对齐范围**（text = R+X、rodata = R、data/bss = R+W），并按域 base 重新应用重定位（复用 `loader.rs` 的私有 ELF API，绝不复用 KernelNative 放段结果）；`os/core/src/component/isolated_lifecycle.rs` 创建私有 AS、落镜像、预置**组件栈 + 实例内存窗口**，经 跨 AS trampoline 执行 `kcomp_instance_create` / `kcomp_instance_destroy` 与 `kcomp_service_dispatch`（KernelNative caller → Isolated provider），已声明后的失败退役 AS + 保留预置窗口至显式 reclaim + `Failed`。由 ArchTest 在 RV64 + RV32 QEMU 端到端证明（`isolated-lifecycle` / `isolated-lifecycle-fail` / `isolated-service` / `isolated-service-fault`）：组件在私有 AS 里跑过、ABI 交窗口（args / config / out_state，串行布局见下）正确、新上下文以 `tp == 0` 进入（`tp` 是普通架构 / 任务执行状态，不是组件运行时指针）、窗口只在该实例 AS 里可达、destroy 入口真的执行、Core AS 每次切换后恢复；跨域调用的扁平帧**直接**交付（caller 是 KernelNative，共享 Core 映射让 `frame` / args / input / output 在 provider 的 AS 里 same VA → same PA 直接有效——无拷贝、无中间页）、provider 原地读写 caller 缓冲、provider 故障被 普通 Core trap 路径收敛（caller 拿到类型化错误、实例 `Failed` + AS 退役）。
- **失败 / 重启矩阵**：失败后 Failed、AS 退役、Endpoint 永久失效；已发布窗口与 image 保留至显式 reclaim。destroy 故障不重试。ArchTest 验证失败窗口先留驻、reclaim 后旧 AS handle 不再存在。重启获得全新 ComponentId/AS/backing；Task/IPC import 已接，设备/DMA/IRQ 与共享 heap 继续拒绝。
- **Isolated 的内存路径（Core 预置窗口，窄 import 面）**：Core 为每个实例预置一块**实例内存窗口**（Core backing、零初始化、只映射在该实例的私有 AS），以固定串行布局交付 create args / `out_state` / config——`+0` `KcompCreateArgs`、`+32` `out_state`(`usize`)、`+64` config payload（`WINDOW_CONFIG_MAX = 256`）、`+320` 域 `MemoryView`（24 B）、`+352` runtime 部署描述符（RV64 24 B / RV32 16 B），窗口大小 4096。旧 `+64` 的 64 字节 "runtime context block" **已删除**；全新同步进入的 Isolated 上下文观察到 `tp == 0`（`tp` 只是架构 / 任务执行状态，见 `docs/modules/arch.md`）。Core 以 **`kcore_memory_view` 编码**（`kind = LOCAL_VA`、`base/len` = 本窗口）把该域视图预交付给实例。表示是**实例内 VA**（`view.base/len` 只在那个 AS 里有意义，归属由该实例的页表承载；绝不出现物理地址 / Core 私有 VA），ArchTest `isolated-lifecycle` 断言编码与「窗口只在该实例 AS 里可达」。`kcore_memory_acquire/release` 已加入 Isolated import 面：按调用身份取得实例 AS，动态 backing 放在 `0x23000000..0x2f000000`，release 只接受该窗口内的精确 acquire 映射。SDK runtime 在业务 create 前初始化，使同一个工件的分配自动选择 K 共享堆或 I 私有堆（见 `memory-and-heap.md` §6.1）。Isolated 仍是 S-mode 普通直接调用；ecall 属于后置的 SandboxedNative。
- **跨域 service 传输**：K caller 保留共享 Core 映射下的直接帧/缓冲交付。I caller 经 `isolated_call` 检查范围权限、搬运 args/input/output、切到独立 Core 栈与挂起的 Core root，再按 provider 域分派；返回后恢复 caller root，传输成功时写回 output/status。payload 是不透明字节，嵌套指针由 SDK adapter 编码；provider 不得保留本次借用。没有固定长度上限，资源不足返回 ENOMEM。
- **同一 artifact 能否按域重定位？** 可以，但**必须按域重新放段 + 重新解析 import**（新 `base`、新 import 目标）。Isolated 侧已具备"按域重新放段 + 重定位 + **支持面 import 解析**"（支持面 = 诊断 / 只读查询、panic、私有 backing 与 endpoint API，解析到共享的低别名，运行时是普通 C-ABI 调用、`satp` 不变）；KernelNative 侧每次 instantiate 重新放段 / 重定位、完整导出面。
- **text 何时可跨域共享？** 只有当**重定位后的 text 字节完全相同**时才能共享可执行页，即：**same VA**（同一段虚拟地址）+ **same import-target VA**（import 在两端解析到**同一 VA**）。共享 Core 映射已让支持面 import 在**每个 Isolated AS 里解析到同一低别名 VA**（same VA → same PA），因此支持面 import 的 same import-target VA 条件天然成立；更宽的 import 面若引入按域不同的目标，才需要额外的固定 VA 机制。做不到这两条，就必须按域各自放段 / 重定位，**不能共享 text**。
- **instance-state**：`kcomp_instance_create` 返回的 opaque state 是**实例**私有、不是 image 共享；它由该实例自己的分配器分配，backing 以 **region 粒度**由 Core **提供**（Core 不记 owner、无账本；backing 经 `kcore_memory_acquire/release` 交付。契约见 `memory-and-heap.md`）。**Component 身份 = `ComponentId` + `ComponentRecord.instance_state`**，不与分配器 / runtime 状态合并。堆是 **runtime / deployment 策略**、不是组件一等资源：KernelNative 可共享 Core 内核堆，私有执行域可在自己的可写 `.data` / `.bss` 保留私有分配器。

> **不要声称一个 `mode` 字段就能实现这些。** 一个字段只表达"意图"；上面每一条都需要真实机制（§7 的依赖排序缺口）。

### 6.3 依赖排序的缺口清单

已接通 load → private Task → Core IPC adapter → own Server Task → stop/force →
显式 CPU-only reclaim。剩余先补一般 Graceful 通知/drain、OOM/竞态矩阵与精确保留
核算，再考虑 remote TLB shootdown、RV32 U 和设备部署；路由仅设计，不是前置。

## 7. 已有 / 目标 / 缺口（逐条，带 `file:line`）

> 以下事实来自已完成的源码审计，**不软化**。

### 7.1 执行域与隔离

| 项 | 已有 | 剩余边界 / 源码 |
|---|---|---|
| 私有 AS | I: RV64/RV32；U: RV64；独立 image/heap/stack、页权限、AS teardown | `isolated_load.rs`、`isolated_lifecycle.rs`、`memory/address_space.rs`；I 是可信 S-mode |
| Task 调度 | 每 Task Core 栈和私有业务栈；AS 从唯一 ComponentRecord 查询 | `task/mod.rs`、`isolated_api.rs`；RiscvContext 不存 satp，yield/park 先回 Core root |
| U-mode | 复用普通 arch UserContext/trap；ecall adapter、10ms timer 返回 Core | `sandbox.rs`、`export/sandbox.rs`；RV32 U 仍 ENOTSUP |
| TLB | ASID=0；每次跨域进入/返回全量 sfence | 无远端 shootdown；reclaim 要求全局私有域安全点，否则 EBUSY |
| 物理回收 | 私有 CPU-only backing/页表/Task 栈显式归还 | `reclaim.rs`；K backing、DMA、不可追踪裸引用不承诺 |

### 7.2 重新放段与入口

| 项 | 已有 | 目标 | 缺口 / 证据 |
|---|---|---|---|
| 每次 instantiate 重新放段 + 重定位 | **已实现**：KernelNative 每次 instantiate 走 `loader.rs`，Isolated 走 `component/isolated_load.rs`（按域放段 + 页级权限分离 + 按域重定位，create / destroy / **可选 `kcomp_service_dispatch`** 都解析成实例域 VA），`isolated_lifecycle.rs` 消费它（ArchTest 在 RV64/RV32 证明）；同一 artifact 可并存多个组件，各自独立 backing / AS | 更宽的按域 import 面 | KernelNative：`loader.rs`（入口是绝对 `usize` transmute 成 fn 指针，`containment.rs:906,913`）；Isolated：`isolated_load.rs` / `isolated_lifecycle.rs`、ArchTest `isolated-image` / `isolated-perm-*` / `isolated-lifecycle` / `isolated-service*` |

### 7.3 部署 / 模式字段

`ComponentRecord.execution_domain/address_space` 仍是唯一部署真相。
K/I/U 共用 ComponentId、LoadedComponent 和 staged publication；无 Image/Instance 二级表。
请求超出平台/白名单能力返回 ENOTSUP，不退化到 KernelNative。

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
- **稳态 IPC**：普通typed前端使用生成client/dispatcher，在真实owner Server Task处理；不退回function table。保留同步诊断单独验证Direct/Gate。
- **RV32 / RV64 布局与宽度一致**：`KcompCallFrame` `size_ptrs = 6`（32/64 同布局）；kcore ABI 宽度规则（`overview.md` §5）。

### 9.3 执行域验证

CoreTest 使用同一 Echo artifact/生成 Contract/Handler，RV64 覆盖九格、RV32 覆盖
四格真实 IPC。另有 private bad-buffer、fault、1000 次 load/IPC/stop/reclaim/stale、
U 非协作忙循环与远端 CPU 停止测试。ArchTest 保留原 K/I Gate、RX/NX、fault/restart
等硬件回归，并检查失败窗口保留到显式 reclaim。host 不作为隔离证据。
完整失败/OOM、所有 IPC 退出竞争、跨 CPU copy/unmap、精确常驻 metadata 核算仍待补齐。

### 9.4 性能

分四段量，**不要混成一个数**：

| 段 | 含义 |
|---|---|
| **bind cost** | 一次绑定的验证 + 提交（含 ABI 比较） |
| **call cost** | 每次调用的固定开销 |
| **AS-switch cost** | 跨域切换地址空间 / 特权级的开销（Gate / syscall 才有） |
| **data-transfer cost** | 参数 / 负载搬运（frame 打包、跨域拷贝） |

普通业务报告当前Native IPC基线；旧裸function table/Gate数字只作为历史对照，不能
因它更快重新增加普通业务Direct。私有域性能分离AS切换与copy，当前尚无新I/U IPC数据。

---

## 10. 实现状态

| 能力 | 当前支持 | 边界 / 文件 |
|---|---|---|
| K | 可信共享 S/AS，普通 Task/IPC | KernelNative backing 不批量回收 |
| I | RV64/RV32 S/MMU `.kcomp` create/destroy、持久 Task、私有 heap、IPC | `isolated_lifecycle.rs`、`isolated_api.rs`；不隔离特权，不能强杀不让出的 S Task |
| U | RV64 S/MMU `.kcomp` relocation、USER 段/栈/thunk、create/destroy、Task/IPC | `sandbox.rs`、`export/sandbox.rs`；timer 返回 Core；设备/DMA/IRQ/Gate 不支持 |
| IPC | RV64 K/I/U 九格、RV32 K/I 四格 | `export/ipc.rs`、`access.rs`；验证输出并 pin AS 后提交与 copy |
| Graceful | live Task/inflight/Direct 时 EBUSY；无 live Task 才执行一次 destroy | 目前 Echo 由业务 STOP 先退 Server；一般 Core 通知/drain 尚缺 |
| Force | 逻辑撤销、skip destroy、cancel saved Task；实际运行未离场 EBUSY | `reclaim.rs`；U timer 有真实 SMP 回归；K/I 无有限时间强杀保证 |
| Reclaim | 私有 CPU-only 已证明静止时撤 root、表页、独占 extents、Task 栈 | 全局私有域安全点；重试幂等；保留 Component/Endpoint tombstone |
| DMA/Device | 保持已有 Quarantine | 本轮不新增设备静默/reset/IOMMU 能力 |

I import 不授予对抗隔离：它仍可改 Core、satp、stvec。U 私有页权限是真实硬件边界，
但本轮不宣称完成恶意组件安全审计。ASID 与远端 TLB shootdown 尚未实现。
失败的已发布 private image/窗口统一保留到同一 reclaim 证明，不再故障时提前 free。
已完成 IPC 副本可在 provider backing 释放后由存活 caller collect；旧 Endpoint 永久失效。

| caller → provider（普通 IPC） | K | I | U |
|---|---|---|---|
| K | RV64/RV32 | RV64/RV32 MMU | RV64 MMU |
| I | RV64/RV32 MMU | RV64/RV32 MMU | RV64 MMU |
| U | RV64 MMU | RV64 MMU | RV64 MMU |

## 11. 相关文档

已执行补丁、实际门禁与剩余任务见 [Runtime 交付](../development/component-runtime-consolidation.md#7-授权后的生产实现与验证)。
生命周期语义见 [component-lifecycle](component-lifecycle.md)，回收证明见
[memory-and-heap](memory-and-heap.md)，控制面仅设计见 [路由 ADR](../development/ipc-routing-service-discovery-adr.md)。
