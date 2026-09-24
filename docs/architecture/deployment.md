# 部署与绑定：部署决定调用机制（deployment.md）

> 本文件是**"部署（deployment）决定调用机制"**的设计契约：谁提议部署、Core 验证什么、`(caller domain, callee domain)` 如何选出调用机制、binding 携带什么、不支持的组合如何拒绝。
> 它是**设计契约，不是进度快照**。当前**只有 KernelNative 一种部署真实存在**，跨域机制**全部未实现**（§10 是实现状态表，§7 是逐条缺口）。
> 与 `docs/architecture/component-lifecycle.md` 在"同一份组件代码能否跨执行域原样运行"上冲突时，**以本文件为准**：`component-lifecycle.md` §9（第 237 行）"共享 text 是未来的 loader 优化，不是 ABI 承诺"的结论**被本文件取代**（见 §6、§8）。本文件不是要否认它的现状描述，而是把目标写清楚，并把缺口显式登记。

---

## 1. 五个分离概念与推荐架构图

"部署决定调用机制"要把五件事分开，任何一件混进另一件都会重造框架或伪造边界：

| 概念 | 回答什么 | 身份 / 载体 | 代码锚点 |
|---|---|---|---|
| **Contract** | 这个服务**语义**是什么 | 接口名 + exact ABI fingerprint + typed `#[repr(C)]` function table | `abi/block.toml`、`os/components/kcomp-sdk/src/block.rs:68-75` |
| **ComponentImage** | **一份加载并重定位后的代码** | `ComponentImageId`；`base` + `create` / `destroy` / `service_dispatch` | `os/core/src/component/image.rs:52-71` |
| **ComponentInstance** | **一个跑起来的实例** | `ComponentId`；`state` + 资源归属（device / irq / dma / task / publication） | `os/core/src/component/registry.rs` |
| **Endpoint** | provider **发布的服务点** | `EndpointId`（provider + port_name + contract） | `os/core/src/component/endpoint.rs` |
| **Transport / deployment** | 这次调用**怎么跨过去** | 由 Core 在 **bind 时**按 `(caller domain, callee domain)` 选定 | 本文件 §2、§3 |

**核心判断（不可违背）：**

- **Core owns the execution-domain truth。** 组合器（composer / profile）**提议**部署（哪个组件跑在哪个执行域），Core **验证并提交**。Core **不硬编码信任级策略**（不写"所有组件都必须走同一套重型机制"）。
- **同一份组件业务代码 + 服务契约不得按部署重写。** 按域入口点、SDK import、transport adapter 是**运行环境**的事，不是业务代码的事。
- **Binding 以调用者的执行域为作用域，不是可搬运的 POD。** 合法机制取决于**两端**（caller domain **且** callee domain），**绝不**只看 provider 的部署标签。
- **Core 在 bind 时一次性选定机制**，运行期**不按调用重新决策**。SDK **实现**每种机制，但**不得选择**机制；否则组件可能悄悄降级到 native，这是**禁止**的。不支持的部署 / 绑定必须**显式拒绝**。

```text
                        组合器 / Profile（composer）
                              │ propose：组件图 + 每个组件的部署域
                              ▼
┌─────────────────────────────── Core ────────────────────────────────┐
│ owns execution-domain truth                                         │
│ validate：owner / liveness / exact ABI / trust + platform capability│
│ commit  ：绑定记录（EndpointId, caller domain, callee domain）      │
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

   Contract（语义）        ComponentImage（加载代码）      ComponentInstance（运行实例）
     block.device            .kcomp → ET_REL                 ComponentId
     exact ABI + repr(C)     base + create/destroy           state + owner(device/irq/dma/task)
          │                  pinned-until-reboot             │
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
     输出：Direct | Gate | syscall-IPC | rejected
     规则见 §3 模式矩阵

④ Core 提交绑定记录（记录 trace）
     binding 携带：
       - Direct：provider 的 repr(C) function table 指针 + opaque ctx
                 （endpoint 记录上的 `api` / `ctx`，Core 只存不解引用）
       - Gate  ：Core call-gate handle（不透明 EndpointId；provider principal +
                 per-call service stack + panic containment 由 Core 拥有）
       - 另记：caller domain / callee domain / mechanism（Core 真相）

⑤ 不支持的组合 → 显式拒绝
     绝不静默降级。例：跨域绑定绝不返回裸 function table；平台无 U-mode / 无私有 AS
     时请求 Sandbox 部署 → 拒绝（`-ENOTSUP` / `-EINVAL` 一类），不是"当成 native 跑"。
```

> **验证的四个维度缺一不可。** 今天的真实缺口：`kcore_endpoint_lookup` **没有 abi 参数**，其实现只比较 contract（`os/core/src/component/export.rs:648-677` 调 `endpoint.rs:378-403` 的 `discover`）；真正做 abi 校验的 `EndpointRegistry::lookup`（`endpoint.rs:358-373`）**从 C ABI 不可达**。所以 **exact ABI 这一维当前在组件消费路径上根本没做**（§7）。

**机制选择只发生在 bind 时。** 运行期的每次 `block.read(...)` 只走绑定已经选定的那条路，**不再**判断 caller / callee 域，也**不再**问 Core。SDK 的调用后端只**实现**机制，不**选择**机制。

---

## 3. 调用双方模式矩阵

行 = caller 的执行域，列 = callee 的执行域。单元格 = **合法机制**。`rejected` 表示该组合**必须被 Core 显式拒绝**（当前所有跨域格子都是"目标，未实现"）。

| caller ↓ \ callee → | KernelNative（S，共享 AS） | IsolatedNative（S，私有 AS） | SandboxedNative（U，私有 AS） |
|---|---|---|---|
| **KernelNative** | **Direct**（可选 **Gate**） | **Gate** | **Gate** |
| **IsolatedNative** | **Gate** | **Direct**（同域）/ **Gate** | **Gate** |
| **SandboxedNative** | **syscall-IPC** | **syscall-IPC** | **Direct**（同域）/ **syscall-IPC** |

读法：

- **同域（K↔K、I↔I、S↔S 同一 AS）**：`Direct` 合法（同地址空间、同特权级，就是普通函数调用）。**Gate 也合法**——同一部署可以选择**受控绑定**。
- **跨域**：`Direct` **非法**，必须是 `Gate`（同特权、跨 AS）或 `syscall-IPC`（跨特权）。任何跨域组合都**不得**返回裸 function table。
- **跨特权**：S caller → U callee 走 `Gate`（Core 经 `sret` 进入 U，provider 经 `ecall` 返回）；U caller → S/kernel callee 走 `syscall-IPC`（`ecall` 进 Core，Core 分派）。
- **能力不足**：`rejected`。平台没有对应能力时，Core 拒绝该部署或该绑定，而不是假装能跑。
- **执行模型 / ISA / runtime（native machine code vs Wasm）不在本矩阵**：它与执行域是**正交维度**——`KernelNative` / `IsolatedNative` / `SandboxedNative` 都可以承载 Wasm runtime，`SandboxedNative` 也都可以是 native code。把 Wasm 放进 `ExecutionDomain` 是把两个正交维度揉到一起。Wasm 是未来 Component 的一种**执行后端**（`AGENTS.md`），需要**单独的维度**表达，**不是第四个执行域**（登记见 §10）。

**为什么 native binding 绝不能跨域传递：**

Direct binding 携带的是 `(api, ctx)` 两个**裸指针**，只在 provider 的地址空间里有意义。把它交给另一个执行域的 consumer：

1. **地址无意义或更糟**：在另一个 AS 里，那两个 VA 可能未映射（fault），也可能指向**别的东西**（静默错误）。私有 AS 的前提就是 VA 不共享。
2. **绕过 callee 域的入口与强制**：直接调 function table 跳过了 callee 域的进入 / 退出、provider principal、per-call service stack、panic containment。U-mode 的强制边界（页表 + 特权级）在裸指针下**完全失效**。
3. **作用域错配**：binding 是"**以调用者执行域为作用域**"的；跨域复制它，等于把 A 域的访问窗口塞给 B 域。

因此 **binding 不是 POD**：Core 必须**按 caller 域重新解析**，产出该域合法的机制，而不是把 A 的绑定拷贝给 B。

**Direct 买到速度，但买不到两件事（写清楚，别高估）：**

| 能力 | Direct | Gate |
|---|---|---|
| 无 Core 通用分派、无栈分配、无消息打包、零 per-call 重决策 | 是 | 否 |
| ambient owner 切换（调用期把归属切到 provider） | **否** | 是 |
| "A 在 B panic 后仍能继续"的承诺（provider panic 收敛、caller 存活） | **否** | 是 |

- Direct 下 provider 与 caller 同特权、同地址空间：**没有** Core 拥有的边界来切换 owner，也**没有**可恢复的上下文来收敛 provider 的 panic。B panic = 进程级 abort 的一部分，Direct **不**承诺 caller 存活。
- Gate 下 provider 跑在 Core 拥有的 per-call service stack 上，principal = provider 自己，provider panic 被 Core 收敛（标 `Failed`、撤销 authority、永久失效其 endpoint），**caller 存活且不变**。

**`inflight` 只计 Gate 调用。** Core 的 endpoint `inflight` 计数只在**经 Core call gate** 的调用上递增；Direct 绑定**完全绕过 Core**，Core 看不到。因此：

> **"inflight == 0 → 可以 stop（否则 -EBUSY）"永远不能证明"所有调用都已结束"。** 它只证明"没有正在进行的 Gate 调用"。若存在 Direct 绑定，consumer 手里那张 function table 的调用对 Core 不可见——这正是 KernelNative 无隔离 + 物理驻留的直接后果。teardown 的正确性不能建立在 `inflight` 上。

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

- 六个字段**全部指针宽**（`size_ptrs = 6`），因此 **32/64 位布局一致、可跨执行域搬运**；**没有嵌套 raw pointer**，标量参数编码在 `args` 的扁平字节区里。
- `method` / `port` 是独立标量参数（`kcomp_service_dispatch`），Core **从不解释**语义。
- Direct 路径**不用这个 frame**：直接调 function table，参数就是普通的 C 参数。这正是 Direct 快的原因。

---

## 6. artifact / import 可移植方案

四层要分开看，**不要**把它们压成一层：

| 层 | 是什么 | 现状锚点 |
|---|---|---|
| **artifact 文件** | `.kcomp` = ET_REL，语言无关；UNDEF 只允许 `kcore_*` | `tools/kcomp-link.sh`、`os/core/src/component/loader.rs` |
| **加载并重定位后的镜像** | 一次放段、一个 `base`、import 只重定位一次 | `os/core/src/component/image.rs:52-71`；`loader.rs:156,317-372` |
| **运行实例** | `ComponentId` + `state` + 归属 | `os/core/src/component/registry.rs` |
| **按域的映射（目标）** | 同一 image 放进某执行域时的 VA 布局 + import 解析 | **未实现** |

### 6.1 `kcore_*` import 在三种部署下如何解析（目标）

```text
Native    ：直接符号地址（现状：loader 重定位到 export 白名单解析出的函数地址）
            → loader.rs:337-341（export::resolve）
Isolated  ：Core call gate handle（Core 拥有的 trampoline，不是裸 Core 地址）
            → 未实现
Sandbox   ：syscall stub（自有稳定 wire ABI，ecall 进 Core）
            → 未实现（wire ABI 设计见 driver-model.md §6.4）
```

### 6.2 text / data / bss / instance-state 的 VA 与重定位

- 现状（KernelNative）：**单一 load base**（`image.rs:57`），段放置只提供段内对齐、**没有页级权限分离**（`loader.rs:156-187`）；import 只重定位**一次**（`loader.rs:317-372`）；`create` / `destroy` / `service_dispatch` 是绝对 `usize`（`image.rs:59-64`），运行时被 transmute 成函数指针（`os/core/src/component/containment.rs:906,913`）。
- **Isolated 按域放段已落地（increment 4，仍是 inactive path）**：`os/core/src/component/isolated_load.rs` 把一份 `.kcomp` 按域重新放段——每个 ALLOC 段拿到**自己的页对齐范围**（text = R+X、rodata = R、data/bss = R+W），并按域 base **重新应用重定位**（复用 `loader.rs` 的私有 ELF API，绝不复用 KernelNative 放段结果）。由 ArchTest 在 RV64 + RV32 QEMU 证明：真实 `.kcomp` 在私有 AS 里执行、段数据可读、**页表真的按段权限强制**（写 R+X text → scause 15；取指 R+W data → scause 12）、Core 专属映射不可达。**按域 import 解析 / trampoline 仍未实现**（import 包络 = 空集）。
- **同一 artifact 能否按域重定位？** 可以，但**必须按域重新放段 + 重新解析 import**（新 `base`、新 import 目标）。Isolated 侧已具备"按域重新放段 + 重定位"；KernelNative 侧仍是单 base、单次重定位。
- **text 何时可跨域共享？** 只有当**重定位后的 text 字节完全相同**时才能共享可执行页，即：**same VA**（同一段虚拟地址）+ **same import-target VA**（import 在两端解析到**同一 VA**）。后者的可行做法是 **per-domain fixed-VA trampoline**：把每个 `kcore_*` import 解析到该域一个**固定 VA** 的 trampoline（trampoline 本体按域不同，但地址相同）。做不到这两条，就必须按域各自放段 / 重定位，**不能共享 text**。
- **instance-state**：`kcomp_instance_create` 返回的 opaque state 是**实例**私有、不是 image 共享；它经该实例自己的 `HeapState`（per-instance runtime context）分配，backing 以 **region 粒度**由 Core **提供**（Core 不记 owner、无账本；backing 经 `kcore_memory_acquire/release` 交付。契约见 `memory-and-heap.md`）。

> **不要声称一个 `mode` 字段就能实现这些。** 一个字段只表达"意图"；上面每一条都需要真实机制（§7 的依赖排序缺口）。

### 6.3 依赖排序的缺口清单

按依赖顺序，前者不成立后者无从谈起：

1. **执行域字段 + 私有 AS 运行时**：`execution_kind`（当前不存在）、私有地址空间、`satp` 切换、ASID、U-mode、`ecall` 处理。
   （increment 3 已落地**私有 AS 切换机制**：双映射 assembly gateway + Core 侧准备 + 窄故障分派，见 §10；**ASID / U-mode / `ecall` 仍未实现**，且组件生命周期尚未接入该机制。）
2. **loader 按域放段 / 按域 import 解析**：按域放段已落地（increment 4，`component/isolated_load.rs`：页级权限分离 + 显式拒绝 + 按域重定位，仍为 inactive path）；按域 import 解析未实现（import 包络仍是空集）。KernelNative 侧仍是单 base 硬编码，跨域复用不可能。
3. **per-domain 本地入口 / adapter**：`kcomp_service_dispatch` 之外，Direct 的按域 function table 与 Gate 的按域 trampoline。
4. **组件支持范围元数据**：manifest **没有**任何字段声明组件支持哪些部署（`os/core/src/component/store.rs` 只解析 `manifest` 文本 + 组件条目）。
5. **平台能力声明 + 拒绝语义**：MMU / IOMMU / 特权级 / 私有 AS 是否具备，以及"不具备就拒绝"的路径（`driver-model.md` §11 的能力诚实表）。

---

## 7. 已有 / 目标 / 缺口（逐条，带 `file:line`）

> 以下事实来自已完成的源码审计，**不软化**。

### 7.1 执行域与隔离

| 项 | 已有 | 目标 | 缺口 / 证据 |
|---|---|---|---|
| 私有地址空间 | **机制已落地、未接线**（increment 3）：`KernelAddressSpace` 生命周期 + **双映射 assembly gateway**（`satp` 切换 / trap 往返 / 恢复 / 放弃 + 窄故障分派钩子），由 ArchTest 在 RV64+RV32 QEMU 驱动；组件生命周期**尚未调用**，Isolated 仍不执行组件 | 每域私有 AS 接入生命周期 | gateway 代码：`os/arch/src/riscv/gateway/`；Core 准备：`os/core/src/component/isolated.rs`；service call 仍是**同一内核 AS 内的进程内上下文切换**：`os/core/src/component/containment.rs:746-792`（`run_isolated`），切换点 `:773` |
| 上下文切换 | 只存 `ra/sp/s0-s11` | 含 `satp` 切换 | `os/arch/src/riscv/cpu.rs:24-28`（`RiscvContext` 字段），**无 satp**；私有 AS 的切换走独立汇编路径（`gateway_enter` / `gateway_trap_entry`，不把 satp 塞进 `RiscvContext`） |
| `activate()` | 写 satp + sfence，**运行期无人调用** | 按域激活 | `os/arch/src/riscv/mmu/mod.rs`；boot 直接构造 `Sv39AddressSpace`，ASID 硬编码 0。运行期切换由 `arch::riscv::gateway` 汇编完成（ArchTest 驱动，无生产调用方） |
| U-mode / MPP | **无**（`mstatus` 只设 MIE） | U 域 | `UserEnvCall` 已解码（`os/arch/src/riscv/trap/mod.rs:66,97`）但 **panic**（`os/arch/src/riscv/trap/supervisor.rs`） |

### 7.2 镜像复用与入口

| 项 | 已有 | 目标 | 缺口 / 证据 |
|---|---|---|---|
| 跨域复用 image | **不可能（KernelNative 侧）**；**Isolated 按域放段已落地（increment 4，未接线）**：`component/isolated_load.rs` 按域重新放段 + 页级权限分离 + 按域重定位，ArchTest 在 RV64/RV32 证明 | 按域 import 解析 | KernelNative 侧：复用按 **artifact 名**（`image.rs:122-127`）、**单一 load base**（`image.rs:57`）、import **只重定位一次**（`loader.rs:156,317-372`）、入口是绝对 `usize` transmute 成 fn 指针（`containment.rs:906,913`）；Isolated 侧：`isolated_load.rs`、ArchTest `isolated-image` / `isolated-perm-*` |

### 7.3 部署 / 模式字段

| 项 | 已有 | 目标 | 缺口 / 证据 |
|---|---|---|---|
| 部署字段 | **无** | 组件实例 / 部署域记录 | `deployment\|deploy` 在 `os/ abi/ tools/` **零匹配**；`ExecutionDomain` **只出现在注释**（`os/core/src/resource/device.rs:207`、`os/core/src/task/mod.rs:49`、`os/core/src/component/export.rs:106`）。`mode` 字段是**真实缺口**，不是虚构（`overview.md:400`、`driver-model.md:120` 已承认） |

### 7.4 consumer ABI 校验

| 项 | 已有 | 目标 | 缺口 / 证据 |
|---|---|---|---|
| 组合期 exact ABI | **未做** | consumer ABI 与 provider 逐位相等才交付 | `kcore_endpoint_lookup` **没有 abi 参数**（`abi/core.toml:755-783`）；其实现调 `discover`，**只比较 contract**（`endpoint.rs:378-403`、`export.rs:664-669`）。真正查 abi 的 `EndpointRegistry::lookup`（`endpoint.rs:358-373`）**从 C ABI 不可达**。声称已校验的注释（`export.rs:688-689`、`abi/core.toml:794-796`）**是错的** |

### 7.5 ABI 文档与重入

| 项 | 已有 | 目标 | 缺口 / 证据 |
|---|---|---|---|
| `kcore_endpoint_call` 文档 | **过时** | 与代码一致 | `abi/core.toml:797-802` 仍写"没有执行边界 / service stack / re-entry 检测 / panic containment（下一阶段）"，而代码**已全部具备**；生成物镜像了这段过时文字 |
| 重入 | 仅**链成员**门禁 | 可选嵌套深度上限 | 只有链成员判断（`os/core/src/component/call.rs:157`）；**无** `MAX_` / 深度上限 |

### 7.6 DMA 归属（三条不同规则）

| 操作 | 归属规则 | 证据 |
|---|---|---|
| `alloc` | ambient caller | `os/core/src/resource/dma.rs:273,280` |
| `map` | 锚在 **device owner** | `dma.rs:304,320-329` |
| `unmap` | **不解析 caller** | `dma.rs:339-348` |

### 7.7 冻结文档冲突

`docs/architecture/component-lifecycle.md:237` **明确否认**"一个链接好的 KernelNative 二进制能在任何执行域原样运行"是 ABI 承诺。本文件的方向与该结论冲突，**在此显式取代它**（不改其现状描述；其 loader 现状事实仍有效）。

### 7.8 组件的部署支持范围（无元数据可声明）

| 组件 | 能否 Sandbox | 原因 | 证据 |
|---|---|---|---|
| `virtio_blk` | **否（按现状）** | 需要裸 MMIO | `os/components/drivers/virtio_blk/src/lib.rs:302,392`（volatile 读写寄存器） |
| `core_test` | **否（按现状）** | 需要裸 MMIO | `os/components/core_test/src/runtime/resource.rs:56,61,187-188` |
| `kbench` | **否（按现状）** | 需要裸 MMIO | `os/components/kbench/src/irq.rs:104` |
| `driver_prober` | **否（无 Core 侧 broker 时）** | 需要 Core-authority 操作：组件加载 / 任务创建 / `device_nth` | `os/components/driver_prober/src/runtime.rs:149,178,249` |
| `scheduler_rr` | **原则上可以** | 纯逻辑，只走服务 | `os/components/scheduler_rr/src/lib.rs` |
| `fatfs` | **原则上可以** | 纯逻辑，只走服务 | `os/components/filesystems/` |

> **今天没有任何元数据能声明一个组件的支持范围。** manifest 没有相关字段（`os/core/src/component/store.rs`）。要 Sandbox 一个需要裸 MMIO 的组件，必须先把它改成经 Core 窗口访问，或加 Core 侧 broker——这属于 §6.3 的缺口 4。

---

## 8. 逐文件迁移清单与分阶段顺序

### 8.1 逐文件

| 文件 | 现状 | 目标动作 |
|---|---|---|
| `os/core/src/component/endpoint.rs` | Endpoint 真相（publish / discover / lookup / bind / invalidate）；`bind` 落 `TraceEvent::EndpointBind` | 补齐 `lookup` 的 C ABI 可达路径 + abi 校验（消费路径） |
| `os/core/src/component/export.rs` | 导出 `kcore_endpoint_*`（旧的 interface 导出面已删除） | 修 `kcore_endpoint_lookup` 增加 abi 校验 |
| `os/core/src/component/call.rs` | service-call 边界（per-call stack + principal + panic containment） | Gate 机制的落点；补嵌套深度策略（可选） |
| `os/core/src/trace/event.rs` | `EndpointBind` 事件（endpoint / provider / mechanism） | **已完成**：旧 interface 绑定事件已随模型一起删除 |
| `abi/core.toml` | `KIND_ENDPOINT_BIND` + 其余 10 个 kind（连续编号）；`kcore_endpoint_lookup` 无 abi；`:797-802` 过时 | 加 abi 参数、更新文档 |
| `os/components/kcomp-sdk/src/abi.rs` | 共享 ABI 值类型（`InterfaceAbi` / `InterfaceKind`）；旧 `Service` / binding 层已删除 | typed 前端 + 调用后端（Direct / Gate） |
| `os/components/kcomp-sdk/src/block.rs` | `BlockDeviceService`（Direct 形状） | 保持 Direct；接调用后端 |
| `os/components/scheduler_rr/src/lib.rs` | 旧的全局名字绑定已删除 | **已完成（step 5）**：发布 `scheduler.policy` **Gate-only** endpoint（无共享 vtable、无全局名字）+ `kcomp_services!` dispatcher；组合方（core_test / kbench / monitor / ArchTest）显式 discover + `kcore_sched_set_policy` 选择 |
| `os/core/src/component/containment.rs` | `run_isolated`（`:746`）为 Gate 服务栈基础 | 跨域需真实 AS 切换（未实现） |
| `os/core/src/component/isolated.rs` | **increment 3**：Core 侧准备（校验 + gateway 页映射 + `PreparedActivation`）与窄故障策略 seam；**无生命周期调用方** | gateway 生命周期接线（后续 increment） |
| `os/core/src/component/isolated_load.rs` | **increment 4**：按域放段 / 页级权限分离 / 逐段映射（`place` / `place_artifact` / `map_into`）；**无生命周期调用方**（ArchTest 直接驱动） | 生命周期接线 / 按域 import（后续 increment） |
| `os/core/src/component/loader.rs` | 单 base 放段 + 单次 import 重定位（KernelNative 路径不变）；私有 ELF API（解析 / 重定位 / 符号 / `kcomp_abi` 校验）被按域装载复用 | 按域 import 解析（未实现） |
| `os/arch/src/riscv/mmu/mod.rs`、`cpu.rs`、`trap/`、`gateway/` | **increment 3**：双映射 gateway 汇编（`gateway_enter` / `gateway_trap_entry`）+ 窄故障分派接缝已落地；`activate()` 仍无人调用；无 U-mode；ASID 恒 0 + 全量 `sfence.vma` | 按域激活接入生命周期 / U-mode / `ecall` / ASID（未实现） |
| `os/core/src/component/store.rs`（manifest） | 无支持范围字段 | 组件支持范围元数据（未实现） |

### 8.2 分阶段顺序

```text
阶段 1  design
        本文件定稿；显式取代 component-lifecycle.md:237 的结论。

阶段 2  converge identity（收敛身份）
        Endpoint 取代"全局名字 → 单 binding"；
        kcore_endpoint_lookup 增加 exact ABI 校验；
        trace 事件迁到 Endpoint（EndpointBind：endpoint / provider / mechanism）；
        **已完成**：旧的"全局名字 → 单 binding"模型（Core 模块、C ABI 导出面、
        SDK 层、scheduler 名字绑定）已全部删除（协调替换，无 legacy alias）。

阶段 3  Native path（唯一真实存在的部署）
        typed 前端 + 调用后端（Direct）落地；
        业务代码零 mode 分支；
        验证稳态 direct call **不经** kcore_endpoint_call、**不**分配 service stack。

阶段 4  Isolated / Sandbox gap list（只登记，不实现）
        私有 AS / satp 切换 / U-mode / ecall / 按域 loader / 支持范围元数据。
```

**最后一步已完成：** 旧的"全局名字 → 单 binding"模型——Core 的 interface 模块、`abi/core.toml` 的对应导出面、SDK 的类型化 Service / publish / bind / refresh / available 层——已随本次协调替换全部删除（不保留 legacy alias，不保留旧 ABI 编号）。删除顺序遵守了"**先迁 trace，再删接口模型**"：`TraceEvent::EndpointBind` 由 `EndpointRegistry::bind` 在 Core 选定机制后发射（kind 重新编号保持连续，见 `os/core/src/trace/abi.rs` 的 payload 分配表）。

---

## 9. 验收设计

### 9.1 host（`make test-host`）

- **部署 / 绑定矩阵**：对每个 `(caller domain, callee domain)` 组合断言**合法机制**；**不支持的组合显式拒绝**（返回明确错误码，**绝不静默降级成 Direct**）。
- **exact ABI**：consumer ABI 与 provider 不一致 → 拒绝 bind（含 `kcore_endpoint_lookup` 的消费路径）。
- **staged publication**：`kcomp_instance_create` 期间 publish 只记 pending，create 返回 0 后 Core 原子提交。
- **多实例**：同一 image 两个实例，独立 state / owner / endpoint。
- **invalidation / no-redirect**：provider 停止 / 失败后 endpoint **永久死亡**，**绝不重定向**到新实例。
- **attribution**：DMA `alloc` / `map` / `unmap` 归属按 §7.6 的三条规则。

### 9.2 Native（QEMU，RV64 + RV32）

- **真实 C / Rust 链**：consumer 经 typed 前端调 provider 的**真实**实现。
- **稳态 direct call**：typed 前端直接走 function table，**不经 `kcore_endpoint_call`**，**不分配 service stack**（用 trace / 计数器证明）。
- **RV32 / RV64 布局与宽度一致**：`KcompCallFrame` `size_ptrs = 6`（32/64 同布局）；kcore ABI 宽度规则（`overview.md` §5）。

### 9.3 Isolated / Sandbox（**仅当实现后**）

- **真实 QEMU** 页表切换 / 栈切换、参数可达性、U-mode `ecall`。
- **明确：host fake 测试与 `activate()` 不算跨域证明。**
  - host fake 上下文后端**不执行**组件入口体，只覆盖边界记账。
  - `activate()`（`os/arch/src/riscv/mmu/mod.rs:56-68`）当前**无人调用**；service call（`containment.rs:773`）**只切 `ra/sp/s0-s11`，没有 satp 切换**。所以任何"已隔离"的结论都必须由真机 / QEMU 上的真实页表与特权级切换证明。

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
| ComponentImage / ComponentInstance 分离 | **已实现** | `image.rs:52-71`、`registry.rs` |
| Endpoint 真相（publish / discover / invalidate） | **已实现** | `endpoint.rs` |
| Gate 调用边界（per-call stack + principal + panic containment） | **已实现** | `call.rs`、`containment.rs` |
| SchedulerPolicy 专用路径（选择 = `kcore_sched_set_policy`；`PolicyCall` 边界；通用调用拒绝保留契约；vtable + 名字绑定已删） | **已实现（step 5）** | `sched.rs`、`component/call.rs`、`containment.rs`、`abi/scheduler.toml` |
| `kcore_endpoint_call` 文档与代码一致 | **未做**（文档过时） | `abi/core.toml:797-802` |
| consumer exact ABI 校验（组合期） | **未做** | `kcore_endpoint_lookup` 无 abi 参数 |
| 部署字段（`InstanceRecord::execution_domain`） | **部分实现**：字段落地 + 创建入口**按域分派**。`KernelNative` 走现有链；`IsolatedNative` 是**拒绝包络 + 私有 AS 准备**（无私有 AS 能力 / 含 `kcore_*` import / 复用 KernelNative image → 装载前 `-ENOTSUP`；设备 / DMA / IRQ / 任务 / 出站调用 → `-ENOTSUP`），**不执行组件**；`SandboxedNative` 是 `todo!()` 占位。只有 KernelNative 可执行 | `registry.rs`、`load.rs`、`export.rs`、`call.rs`、`memory/address_space.rs` |
| 执行模型 / ISA / runtime 维度（native machine code vs Wasm） | **未开始**，且**不属于 `ExecutionDomain`**——与执行域正交，需**单独维度**表达 | 本文件 §3 |
| 按 `(caller, callee)` 域选机制 | **设计完成，未开始** | 本文件 §2、§3 |
| Direct / Gate 作为**绑定机制**分离 | **设计完成，未开始** | 本文件 §1、§4 |
| "inflight 只计 Gate" 的契约约束 | **设计完成** | 本文件 §3 |
| 私有地址空间 / `satp` 切换 / ASID | **部分实现（机制落地、生命周期未接线）**：映射生命周期 / 精确查询 / 退役状态 / 激活描述符（`prepare_activation`）**加上 increment 3 的双映射 assembly gateway**（Core 侧 `prepare` + arch 汇编进入 / trap 往返 / 恢复 / 放弃、窄 Core 故障分派钩子）已落地；ArchTest 在 **RV64 + RV32 QEMU** 证明「Core → 私有 AS → Core 往返」「私有 AS 内时钟中断在 Core AS / Core trap 栈处理后恢复」「组件页故障可恢复 / 可放弃」。**increment 4** 另落地**按域放段**（`isolated_load.rs`：页级权限分离、按域重定位、显式拒绝），ArchTest 证明真实 `.kcomp` 在私有 AS 里执行、段数据可读、页表按段权限强制（写 R+X text → scause 15 / 取指 R+W data → scause 12）、Core 专属映射不可达。**组件生命周期仍未调用**（Isolated 不执行组件），`activate()` 仍无生产调用方，**ASID 恒 0 + 全量 `sfence.vma`**（不实现 / 不声称 ASID 分配复用）；这是**协作式**边界（S-mode 可直接改 satp），不是对抗隔离 | `memory/address_space.rs`、`component/isolated.rs`、`component/isolated_load.rs`、`arch/src/riscv/gateway/`、`arch/src/vm.rs`、`riscv/mmu`、ArchTest `isolated-*` |
| U-mode / `ecall` | **未开始** | `supervisor.rs:63-66`（`UserEnvCall` panic） |
| 跨域 image 复用（按域放段 / import） | **按域放段已实现（inactive path）**：`component/isolated_load.rs` 按域重新放段 + 重定位 + 页级权限分离，ArchTest `isolated-image` / `isolated-perm-text` / `isolated-perm-data` 在 RV64+RV32 证明；KernelNative image 复用仍被拒绝（`load.rs::validate_isolated_load`）。**按域 import 解析未开始**（空集包络） | `isolated_load.rs`、`load.rs::validate_isolated_load` |
| `kcore_*` import 的 Isolated / Sandbox 解析 | **未开始**（Isolated 装载**拒绝任何 `kcore_*` import**，绝不回退到裸 Core 地址；per-domain gate trampoline 未实现） | `load.rs::check_isolated_imports`、`loader.rs` |
| 组件支持范围元数据 | **未开始** | manifest 无字段 |
| 重入嵌套深度上限 | **未开始** | 只有链成员门禁 |

**一句话：** 今天真实存在的是 **KernelNative 单域**；"部署决定调用机制"目前是**契约与缺口清单**，不是可用能力。

---

## 11. 相关文档

- `docs/architecture/overview.md`：分层、ResourceDomain / ExecutionDomain 概念、三个信任域；
- `docs/architecture/component-lifecycle.md`：组件生命周期与实例契约（本文件取代其 §9 第 237 行的结论）；
- `docs/architecture/component-model.md`：Interface / ResourceDomain / 依赖图；
- `docs/architecture/driver-model.md`：执行域模型、能力诚实表、未来 syscall wire ABI；
- `docs/architecture/kconfig.md`：backend 能力来自 Kconfig；部署要求来自组合配置，Core 验证并提交；
- `docs/development/testing.md`：host / CoreTest / QEMU 验证策略；
- `docs/development/benchmark.md`：性能分段与基线。
