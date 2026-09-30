# component（os/core/src/component/）

> Core 里最大的一组模块：**组件身份与生命周期**、**已加载程序（loaded）**、**endpoint 绑定**、**导出 ABI**、ELF **加载流水线**、**panic containment**、失败/停止编排。
> 组件生命周期与入口 ABI 的冻结契约在 `docs/architecture/component-lifecycle.md`；本页只描述代码位置与边界。

## owns 什么真相

- **组件身份与生命周期**：`ComponentId`（唯一的一等运行时身份）、`ComponentState`、`Registry`；状态机 `Declared → Resolved → Starting → Ready → Stopping → Stopped`，任意 → `Failed`。`ComponentId` 永不复用。
- **已加载程序（loaded）**：`registry::ComponentRecord` 1:1 直接持有 `loaded: LoadedComponent`（`base` / `create` / `destroy` / `service_dispatch` / `text_size` / `memory`（常驻 MemoryLease））与 `name`——每次 instantiate 独立放段 / 重定位，`.data` / `.bss` 私有；没有 `ComponentImageId` / `ImageTable` 二级身份。
- **Contract / Endpoint 真相（唯一绑定真相）**：`EndpointRegistry` 记录谁在哪个端口发布了哪个契约（`endpoint.rs`）；`EndpointId` 单调、绝不回收 / 重定向，provider 停止 / 失败 → 它的全部 endpoint 永久 `Invalid`。`bind` 是 Core 选定调用机制（Direct / Gate）的唯一选择点，并落 `TraceEvent::EndpointBind`。
- **导出 ABI**：`kcore_*` 白名单（44 项）的实现与解析。
- **加载编排**：cpio store 解析 → ELF 段放置 / 重定位 → 入口校验。
- **失败与退出**：`fail_component`（mark + revoke + quarantine）与 `stop_component`（`Stopping` / `Stopped`）。

## 暴露什么机制

- `mod.rs`：`ComponentId`、`ComponentState::can_transition`、`is_failed`、`may_run`。
- `registry.rs`：`Registry`、`ComponentRecord`（`name: Vec<u8>` + `loaded: LoadedComponent`）、`RegistryError`；`declare` / `resolve` / `begin_start` / `finish_start` / `begin_stop` / `finish_stop` / `mark_failed` / `record_instance_state`；全局 `get_registry`。
- `abi.rs`：`InterfaceAbi`（exact fingerprint）、`InterfaceKind`（生成物 re-export）。
- `load.rs`：`ComponentLoadError`、`current_component()` / `with_current()`、`load_and_start(name, kind)`、`create_component(name, args, kind)`。`kind` 是**部署请求**：创建入口**按域分派**（`KernelNative` 走 `loader.rs`；`IsolatedNative` 交 `isolated_lifecycle.rs` 真正创建；`SandboxedNative` 是 `todo!()` 占位）。**每次调用都重新 instantiate**（独立放段 / 重定位），同名 artifact 可并存多个组件；能力不足 / 支持面之外的 import → 装载前显式拒绝。见 `deployment.md` §6.2/§7.1/§10。
- `loader.rs`：`load_component()`、`LoadedComponent`、`LoaderError`；解析 ELF、放置段、应用重定位、解析 `kcomp_instance_create` / `kcomp_instance_destroy` / `kcomp_abi`。
- `elf.rs`：架构中立 ELF ET_REL 解析（`ElfObject`、`ElfError`、`ElfClass`、`Section`、`Symbol`、`Relocation`）。
- `store.rs`：内嵌 `.initpkg` cpio store（`CpioEntry`、`EmbeddedStore`、`parse_entries`、`init`、`get_component_store`）。
- `containment.rs`：panic containment（`CallOutcome`、`EscapeKind`、`EscapeInfo`、`call_component_create` / `call_component_destroy` / `call_component_service`、`enter_task` / `enter_anchor`、`panic_escape`、`with_irq_scope`、祖先遍历的 `scheduling_forbidden` / `irq_in_chain` / `provider_in_active_chain`）。
- `endpoint.rs`：Contract / Endpoint 真相（`ContractId`、`EndpointId`、`EndpointState`、`EndpointRecord`、`EndpointRegistry`）；`stage_publish` / `commit_pending` / `discard_pending` / `resolve` / `lookup` / `discover` / `invalidate_endpoint` / `invalidate_provider`。
- `call.rs`：`kcore_endpoint_call` 的 Core 实现（`CallError`）：**按 provider 执行域路由**（KernelNative → service-call 执行边界 `call_component_service`；Isolated → `isolated_lifecycle::dispatch_service` 的 caller 帧直接交付 + 跨 AS trampoline 路径；Sandboxed → 显式拒绝）、re-entry 门禁、IRQ 祖先门禁、`complete_call` / `handle_provider_panic` 收尾。
- `export.rs`：`kcore_*` 导出 ABI 实现（44 项）与 `resolve(name) -> Option<usize>`。
- `failure.rs`：`fail_component`、`revoke_authority_and_unbind`。
- `exit.rs`：`stop_component`、`ComponentStopError`。
- `isolated.rs`：私有 AS 进入的 Core 侧准备（`PreparedTransition`、`EntryArgs`、`prepare`、`enter`、`ComponentFault`、`FaultPolicy`、`install` / `register_fault_policy`）+ **普通 trap 路径的异常钩子**（`on_exception`：按活动跨 AS 现场归因、默认拒绝恢复、放弃经 trampoline 交回 Core 延续）；进入参数（组件入口 `a0..a3`）由 Core 解释、arch 只搬运。**生产调用方 = `isolated_lifecycle.rs`**（Isolated 的 create / destroy / service dispatch 都经这里）；`prepare` 在锁内校验并取出 `Copy` 描述符，`enter` 在锁外组装 **per-invocation** trampoline 记录（CrossAsContext LIFO 链）。
- `isolated_load.rs`：**按域装载**：`PlacedImage` / `PlacedSegment` / `IsolatedLoadError`、`place(blob)` / `place_artifact(name)` / `map_into(handle, image)` / `map_mappings(handle, &mappings)`，以及组件登记用的 `PlacedImage::mappings()` / `into_loaded_component()`。每个 ALLOC 段拿到**自己的页对齐范围**（text = R+X、rodata = R、data/bss = R+W），import 白名单（诊断 / 只读查询 + `kcore_panic_escape`）解析到共享 Core 低别名，显式拒绝出窗 / 重叠 / 不可表达权限 / 非 2 的幂对齐 / 白名单外 import；**可选 `kcomp_service_dispatch` 解析成实例域 VA**（必须落在 R+X 段内）；重定位复用 `loader.rs` 的私有 ELF API（按域 base 重算，绝不复用 KernelNative 放段结果）。ArchTest 在 RV64/RV32 QEMU 直接驱动机制用例，生产消费方是 `isolated_lifecycle.rs`。
- `isolated_lifecycle.rs`：**Isolated 实例生命周期 + 跨域 service dispatch**：`create(name, blob, args)` / `destroy(id, entry, state)` / `dispatch_service(...)` + VA 布局（组件栈 `ISOLATED_STACK_BASE` / **实例内存窗口** `ISOLATED_WINDOW_BASE` 与窗口偏移）。create = 每次按域放段（`isolated_load::place`）+ 登记（全新私有 backing，**无** same-image 复用）→ 声明 → 私有 AS → 落镜像 + 预置窗口（`+0` args / `+32` `out_state` / `+64` config / `+320` 域 `MemoryView`）→ Starting → 写 args / out_state → `prepare` + `enter` 跑 `kcomp_instance_create` → `Ready`；destroy = `prepare` + `enter` 跑 `kcomp_instance_destroy` → 退役 AS（`exit.rs` 按 `execution_domain` 分派；窗口按 phase 1 契约保持驻留）；dispatch_service = 帧结构 sanity → `prepare` + `enter` 跑 `kcomp_service_dispatch`（caller 是 KernelNative，共享 Core 映射让帧与 args / input / output 在 provider 的 AS 里 same VA → same PA 直接有效——provider 原地读写 caller 缓冲，无拷贝）→ 方法状态写 `*out_status`。**失败清理**（create / service 故障 = Core 中止实例）：退役 AS + 解映射并归还预置窗口 + `Failed`（半成品不留）；**destroy 路径**（成功或入口故障）：只退役 AS，窗口驻留（AS 退役后不可进入）。无私有 AS backend 的构建显式拒绝。内存 / 传输路径选择：**Core 预置窗口、窄 import 面**（支持面 import 是普通 C-ABI 直接调用，`satp` 不变；跨 AS trampoline 只用于 create / destroy / service dispatch 的域切换）。
- 重导出：`panic_escape`、`ComponentStopError`、`stop_component`、`fail_component`。

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
| `os/core/src/component/registry.rs` | `Registry`、`ComponentRecord`（`name` + `loaded: LoadedComponent`）、生命周期状态机 |
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
| `os/core/src/component/isolated.rs` | 私有 AS 切换的 Core 侧准备 + 窄故障策略（由 `isolated_lifecycle.rs` 调用） |
| `os/core/src/component/isolated_load.rs` | 按域装载：页级权限分离的段放置 + 逐段映射 + 可选服务入口解析 |
| `os/core/src/component/isolated_lifecycle.rs` | Isolated 组件生命周期 + 跨域 service dispatch：私有 AS + 每次按域放段（全新私有 backing，无 same-image 复用）+ Core 预置窗口，经跨 AS trampoline 跑 create / destroy / `kcomp_service_dispatch`（trampoline 对全新同步 Isolated 入口显式清零 `tp`）；失败清理 / destroy 退役语义 |
| `os/core/src/component/generated/exports.rs` | 生成的 `EXPORTS: [Export; 40]` 表 |
