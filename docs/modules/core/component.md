# component（os/core/src/component/）

> Core 里最大的一组模块：**组件身份与生命周期**、常驻**镜像**、**endpoint 绑定**、**导出 ABI**、ELF **加载流水线**、**panic containment**、失败/停止编排。
> 组件生命周期与入口 ABI 的冻结契约在 `docs/architecture/component-lifecycle.md`；本页只描述代码位置与边界。

## owns 什么真相

- **实例身份与生命周期**：`ComponentId`（即实例 ID）、`ComponentState`、`Registry`；状态机 `Declared → Resolved → Starting → Ready → Stopping → Stopped`，任意 → `Failed`。`ComponentId` 永不复用。
- **镜像身份**：`ComponentImageId` / `ImageTable`——一份常驻加载代码 = 一个 image，pinned-until-reboot。
- **Contract / Endpoint 真相（唯一绑定真相）**：`EndpointRegistry` 记录谁在哪个端口发布了哪个契约（`endpoint.rs`）；`EndpointId` 单调、绝不回收 / 重定向，provider 停止 / 失败 → 它的全部 endpoint 永久 `Invalid`。`bind` 是 Core 选定调用机制（Direct / Gate）的唯一选择点，并落 `TraceEvent::EndpointBind`。
- **导出 ABI**：`kcore_*` 白名单（40 项）的实现与解析。
- **加载编排**：cpio store 解析 → ELF 段放置 / 重定位 → 入口校验。
- **失败与退出**：`fail_component`（mark + revoke + quarantine）与 `stop_component`（`Stopping` / `Stopped`）。

## 暴露什么机制

- `mod.rs`：`ComponentId`、`ComponentState::can_transition`、`is_failed`、`may_run`。
- `registry.rs`：`Registry`、`InstanceRecord`、`RegistryError`；`declare` / `resolve` / `begin_start` / `finish_start` / `begin_stop` / `finish_stop` / `mark_failed` / `record_instance_state`；全局 `get_registry`。
- `image.rs`：`ImageTable`、`ComponentImage`、`ComponentImageId`、`ImageError`；`register` / `find` / `get`、全局 `get_images`。
- `abi.rs`：`InterfaceAbi`（exact fingerprint）、`InterfaceKind`（生成物 re-export）。
- `load.rs`：`ComponentLoadError`、`current_component()`、`load_and_start(name, kind)`、`create_component(name, args, kind)`。`kind` 是**部署请求**：创建入口**按域分派**（`KernelNative` 走现有创建链；`IsolatedNative` 走**拒绝门禁 + 私有 AS 准备**——能力不足 / 含 `kcore_*` import / 复用 KernelNative image 任一命中即 `-ENOTSUP`，**不执行组件**；`SandboxedNative` 是 `todo!()` 占位）——按域装载 / 入口分派仍是 TODO（见 `deployment.md` §6.2/§7.1）。
- `loader.rs`：`load_component()`、`LoadedComponent`、`LoaderError`；解析 ELF、放置段、应用重定位、解析 `kcomp_instance_create` / `kcomp_instance_destroy` / `kcomp_abi`。
- `elf.rs`：架构中立 ELF ET_REL 解析（`ElfObject`、`ElfError`、`ElfClass`、`Section`、`Symbol`、`Relocation`）。
- `store.rs`：内嵌 `.initpkg` cpio store（`CpioEntry`、`EmbeddedStore`、`parse_entries`、`init`、`get_component_store`）。
- `containment.rs`：panic containment（`CallOutcome`、`EscapeKind`、`EscapeInfo`、`call_component_create` / `call_component_destroy` / `call_component_service`、`enter_task` / `enter_anchor`、`panic_escape`、`with_irq_scope`、祖先遍历的 `scheduling_forbidden` / `irq_in_chain` / `provider_in_active_chain`）。
- `endpoint.rs`：Contract / Endpoint 真相（`ContractId`、`EndpointId`、`EndpointState`、`EndpointRecord`、`EndpointRegistry`）；`stage_publish` / `commit_pending` / `discard_pending` / `resolve` / `lookup` / `discover` / `invalidate_endpoint` / `invalidate_provider`。
- `call.rs`：`kcore_endpoint_call` 的 Core 实现（`CallError`）：service-call 执行边界接线（`call_component_service`）、re-entry 门禁、IRQ 祖先门禁、`complete_call` / `handle_provider_panic` 收尾。
- `export.rs`：`kcore_*` 导出 ABI 实现（40 项）与 `resolve(name) -> Option<usize>`。
- `failure.rs`：`fail_component`、`revoke_authority_and_unbind`。
- `exit.rs`：`stop_component`、`ComponentStopError`。
- `runtime_slot.rs`：每实例 **runtime slot**（`RuntimeSlotTable`、`RuntimeSlot`）；`install` / `clear` / `get`、全局 `get_slots`。Core 只存 / 取组件运行时自有的 opaque 指针（RISC-V `tp`，切换边界安装；`0` = 无 slot），**从不解释** —— 执行状态，不是内存记账（`docs/architecture/memory-and-heap.md` §5）。
- `isolated.rs`：私有 AS 切换的 Core 侧准备（`PreparedTransition`、`prepare`、`enter`、`ComponentFault`、`FaultPolicy`、`install` / `register_fault_policy`）。increment 3：机制已落地但**组件生命周期尚未调用**（inactive path，ArchTest 直接驱动）；`prepare` 在锁内校验并取出 `Copy` 描述符，`enter` 在锁外只把描述符搬给 `arch::riscv::gateway` 汇编。
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
| `os/core/src/component/isolated.rs` | 私有 AS 切换的 Core 侧准备 + 窄故障策略（increment 3；无生命周期调用方） |
| `os/core/src/component/generated/exports.rs` | 生成的 `EXPORTS: [Export; 40]` 表 |
