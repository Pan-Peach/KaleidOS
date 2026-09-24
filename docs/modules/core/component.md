# component（os/core/src/component/）

> Core 里最大的一组模块：**组件身份与生命周期**、常驻**镜像**、**endpoint 绑定**、**导出 ABI**、ELF **加载流水线**、**panic containment**、失败/停止编排。
> 组件生命周期与入口 ABI 的冻结契约在 `docs/architecture/component-lifecycle.md`；本页只描述代码位置与边界。

## owns 什么真相

- **实例身份与生命周期**：`ComponentId`（即实例 ID）、`ComponentState`、`Registry`；状态机 `Declared → Resolved → Starting → Ready → Stopping → Stopped`，任意 → `Failed`。`ComponentId` 永不复用。`ComponentState::is_live()` 区分活跃实例与终态 tombstone（Isolated 的同 image 并发门禁用它）。
- **镜像身份**：`ComponentImageId` / `ImageTable`——一份常驻加载代码 = 一个 image，pinned-until-reboot。image 记录**部署域**（`domain`）与 Isolated 的**按域段规划**（`placement`）：跨域复用显式拒绝；同域复用（逻辑重启）只在没有活跃实例时放行。
- **Contract / Endpoint 真相（唯一绑定真相）**：`EndpointRegistry` 记录谁在哪个端口发布了哪个契约（`endpoint.rs`）；`EndpointId` 单调、绝不回收 / 重定向，provider 停止 / 失败 → 它的全部 endpoint 永久 `Invalid`。`bind` 是 Core 选定调用机制（Direct / Gate）的唯一选择点，并落 `TraceEvent::EndpointBind`。
- **导出 ABI**：`kcore_*` 白名单（40 项）的实现与解析。
- **加载编排**：cpio store 解析 → ELF 段放置 / 重定位 → 入口校验。
- **失败与退出**：`fail_component`（mark + revoke + quarantine）与 `stop_component`（`Stopping` / `Stopped`）。

## 暴露什么机制

- `mod.rs`：`ComponentId`、`ComponentState::can_transition`、`is_failed`、`may_run`。
- `registry.rs`：`Registry`、`InstanceRecord`、`RegistryError`；`declare` / `resolve` / `begin_start` / `finish_start` / `begin_stop` / `finish_stop` / `mark_failed` / `record_instance_state`；全局 `get_registry`。
- `image.rs`：`ImageTable`、`ComponentImage`（含 `domain` / `placement`）、`ComponentImageId`、`ImageError`；`register(name, loaded, domain, placement)` / `find` / `get`、全局 `get_images`。
- `abi.rs`：`InterfaceAbi`（exact fingerprint）、`InterfaceKind`（生成物 re-export）。
- `load.rs`：`ComponentLoadError`、`current_component()` / `with_current()`、`load_and_start(name, kind)`、`create_component(name, args, kind)`。`kind` 是**部署请求**：创建入口**按域分派**（`KernelNative` 走现有创建链；`IsolatedNative` 走**门禁 + 生命周期接线**——能力不足 / 含 `kcore_*` import / **跨域复用**（`ImageDomainMismatch`）任一命中即显式拒绝；同域复用只在没有活跃实例时放行（**逻辑重启**；并发活跃实例 → `IsolatedInstanceLive`），否则交 `isolated_lifecycle.rs` 真正创建；`SandboxedNative` 是 `todo!()` 占位）。见 `deployment.md` §6.2/§7.1/§10。
- `loader.rs`：`load_component()`、`LoadedComponent`、`LoaderError`；解析 ELF、放置段、应用重定位、解析 `kcomp_instance_create` / `kcomp_instance_destroy` / `kcomp_abi`。
- `elf.rs`：架构中立 ELF ET_REL 解析（`ElfObject`、`ElfError`、`ElfClass`、`Section`、`Symbol`、`Relocation`）。
- `store.rs`：内嵌 `.initpkg` cpio store（`CpioEntry`、`EmbeddedStore`、`parse_entries`、`init`、`get_component_store`）。
- `containment.rs`：panic containment（`CallOutcome`、`EscapeKind`、`EscapeInfo`、`call_component_create` / `call_component_destroy` / `call_component_service`、`enter_task` / `enter_anchor`、`panic_escape`、`with_irq_scope`、祖先遍历的 `scheduling_forbidden` / `irq_in_chain` / `provider_in_active_chain`）。
- `endpoint.rs`：Contract / Endpoint 真相（`ContractId`、`EndpointId`、`EndpointState`、`EndpointRecord`、`EndpointRegistry`）；`stage_publish` / `commit_pending` / `discard_pending` / `resolve` / `lookup` / `discover` / `invalidate_endpoint` / `invalidate_provider`。
- `call.rs`：`kcore_endpoint_call` 的 Core 实现（`CallError`）：**按 provider 执行域路由**（KernelNative → service-call 执行边界 `call_component_service`；Isolated → `isolated_lifecycle::dispatch_service` 的邮箱 + gateway 路径；Sandboxed → 显式拒绝）、re-entry 门禁、IRQ 祖先门禁、`complete_call` / `handle_provider_panic` 收尾。
- `export.rs`：`kcore_*` 导出 ABI 实现（40 项）与 `resolve(name) -> Option<usize>`。
- `failure.rs`：`fail_component`、`revoke_authority_and_unbind`。
- `exit.rs`：`stop_component`、`ComponentStopError`。
- `runtime_slot.rs`：每实例 **runtime slot**（`RuntimeSlotTable`、`RuntimeSlot`）；`install` / `clear` / `get`、全局 `get_slots`。Core 只存 / 取组件运行时自有的 opaque 指针（RISC-V `tp`，切换边界安装；`0` = 无 slot），**从不解释** —— 执行状态，不是内存记账（`docs/architecture/memory-and-heap.md` §5）。
- `isolated.rs`：私有 AS 切换的 Core 侧准备（`PreparedTransition`、`EntryArgs`、`prepare`、`enter`、`ComponentFault`、`FaultPolicy`、`install` / `register_fault_policy`）；进入参数（组件入口 `a0..a3`）由 Core 解释、arch 只搬运。**生产调用方 = `isolated_lifecycle.rs`**（Isolated 的 create / destroy / service dispatch 都经这里）；`prepare` 在锁内校验并取出 `Copy` 描述符，`enter` 在锁外只把描述符搬给 `arch::riscv::gateway` 汇编。
- `isolated_load.rs`：**按域装载**：`PlacedImage` / `PlacedSegment` / `IsolatedLoadError`、`place(blob)` / `place_artifact(name)` / `map_into(handle, image)` / `map_mappings(handle, &mappings)`，以及 image 登记用的 `PlacedImage::mappings()` / `into_loaded_component()`。每个 ALLOC 段拿到**自己的页对齐范围**（text = R+X、rodata = R、data/bss = R+W），import 包络保持**空集**，显式拒绝出窗 / 重叠 / 不可表达权限 / 非 2 的幂对齐 / 非空 import；**可选 `kcomp_service_dispatch` 解析成实例域 VA**（必须落在 R+X 段内）；重定位复用 `loader.rs` 的私有 ELF API（按域 base 重算，绝不复用 KernelNative 放段结果）。ArchTest 在 RV64/RV32 QEMU 直接驱动机制用例，生产消费方是 `isolated_lifecycle.rs`。
- `isolated_lifecycle.rs`：**Isolated 实例生命周期 + 跨域 service dispatch**：`create(name, blob, args)` / `destroy(id, entry, state)` / `dispatch_service(...)` + VA 布局（组件栈 `ISOLATED_STACK_BASE` / **实例内存窗口** `ISOLATED_WINDOW_BASE` / **服务邮箱** `ISOLATED_MAILBOX_BASE` 与窗口偏移）。create = `isolated_image`（已登记同域 → **复用**常驻 backing / 段规划 / 入口 = 逻辑重启；否则按域放段 + 登记）→ 声明 → 私有 AS → 落镜像 + 预置窗口 / 邮箱 → Starting → 装 runtime slot → 写 args / out_state → `prepare` + `enter` 跑 `kcomp_instance_create` → `Ready`；destroy = `prepare` + `enter` 跑 `kcomp_instance_destroy` → 退役 AS（`exit.rs` 按 `execution_domain` 分派；窗口按 phase 1 契约保持驻留）；dispatch_service = 帧容量校验 → 拷贝进邮箱 → `prepare` + `enter` 跑 `kcomp_service_dispatch` → output 拷回 caller / `*out_status`。**失败清理**（create / service 故障 = Core 中止实例）：退役 AS + 解映射并归还预置窗口 / 邮箱 + `Failed`（半成品不留）；**destroy 路径**（成功或入口故障）：只退役 AS，窗口驻留（AS 退役后不可进入）。无私有 AS backend 的构建显式拒绝。内存 / 传输路径选择：**Core 预置窗口 / 邮箱、无 import 面**（trampoline 属后续增量）。
- `isolated_mailbox.rs`：**跨 AS 扁平帧邮箱**（host-testable）：页内布局（描述符 + args / input / output 区）、固定容量（每区 1 KiB）、`check_frame` / `write_frame` / `read_output`；超长显式拒绝（`-EMSGSIZE`），绝不截断。
- 重导出：`panic_escape`、`ComponentStopError`、`stop_component`、`fail_component`、`ComponentImage`、`ComponentImageId`。

## 明确不做

- **组件间不建 flat ELF 符号表**：consumer 只经 endpoint 拿到 `EndpointId`（组合期 `lookup` + `bind` 时才由 Core 交付 Direct 的 `api` / `ctx`），endpoint 永不重定向，provider 停止 / 失败即永久失效。
- **不做物理 unload / refcount 回收**：`Stopped` / `Failed` 记录留 tombstone，段内存不回收，`ComponentId` 不复用。
- **无 ABI 版本兼容**：`kcomp_abi` 是 exact 指纹，不自动生成、不做兼容协商；陈旧 `.kcomp` 不保证可加载。
- **失败路径不调 `kcomp_instance_destroy`**（崩溃的模块不值得信任）；`Failed` 只走 `fail_component`。
- 无 `UnexpectedExit` 独立终态（意外退出统一 `Failed`）；无实例退役 / 段内存回收；无 drain variant（有活任务直接拒绝停止）。
- **panic recovery ≠ fault isolation**：KernelNative 组件仍可能写坏 Core 内存 / UB / 持锁死亡，这是协作式 containment，不是对抗隔离。

## 代码在哪

| 文件 | 内容 |
|---|---|
| `os/core/src/component/mod.rs` | `ComponentId`、`ComponentState`、`is_failed` / `may_run` |
| `os/core/src/component/registry.rs` | `Registry`、`InstanceRecord`、生命周期状态机 |
| `os/core/src/component/image.rs` | `ImageTable`、`ComponentImage`、`ComponentImageId` |
| `os/core/src/component/endpoint.rs` | `EndpointRegistry`：Contract / Endpoint 真相、publish / lookup / discover / bind / invalidate |
| `os/core/src/component/call.rs` | `kcore_endpoint_call`：service-call 执行边界 + 门禁 + provider panic 收尾 |
| `os/core/src/component/load.rs` | 生命周期编排：declare → create → commit → ready |
| `os/core/src/component/loader.rs` | ELF 段放置 / 重定位 / 入口校验 |
| `os/core/src/component/elf.rs` | 架构中立 ELF ET_REL 解析 |
| `os/core/src/component/store.rs` | 内嵌 cpio `.initpkg` store |
| `os/core/src/component/containment.rs` | panic containment、执行边界、IRQ 作用域 |
| `os/core/src/component/export.rs` | `kcore_*` 导出 ABI 实现 + `resolve` |
| `os/core/src/component/failure.rs` | `fail_component`、归属撤销 |
| `os/core/src/component/exit.rs` | `stop_component`（`Stopping` / `Stopped`） |
| `os/core/src/component/runtime_slot.rs` | 每实例 runtime slot（`tp`）：install / clear / get |
| `os/core/src/component/isolated.rs` | 私有 AS 切换的 Core 侧准备 + 窄故障策略（由 `isolated_lifecycle.rs` 调用） |
| `os/core/src/component/isolated_load.rs` | 按域装载：页级权限分离的段放置 + 逐段映射 + 可选服务入口解析 |
| `os/core/src/component/isolated_lifecycle.rs` | Isolated 实例生命周期 + 跨域 service dispatch：私有 AS + 按域镜像（同域复用 = 逻辑重启）+ Core 预置窗口 / 邮箱 + `tp`，经 gateway 跑 create / destroy / `kcomp_service_dispatch`；失败清理 / destroy 退役语义 |
| `os/core/src/component/isolated_mailbox.rs` | 跨 AS 扁平帧邮箱：布局 + 容量 + 拷贝方向 |
| `os/core/src/component/generated/exports.rs` | 生成的 `EXPORTS: [Export; 40]` 表 |
