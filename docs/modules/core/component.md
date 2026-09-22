# component（os/core/src/component/）

> Core 里最大的一组模块：**组件身份与生命周期**、常驻**镜像**、**接口绑定**、**导出 ABI**、ELF **加载流水线**、**panic containment**、失败/停止编排。
> 组件生命周期与入口 ABI 的冻结契约在 `docs/architecture/component-lifecycle.md`；本页只描述代码位置与边界。

## owns 什么真相

- **实例身份与生命周期**：`ComponentId`（即实例 ID）、`ComponentState`、`Registry`；状态机 `Declared → Resolved → Starting → Ready → Stopping → Stopped`，任意 → `Failed`。`ComponentId` 永不复用。
- **镜像身份**：`ComponentImageId` / `ImageTable`——一份常驻加载代码 = 一个 image，pinned-until-reboot。
- **接口绑定真相**：`InterfaceRegistry` 记录谁提供什么接口、当前绑到谁（typed `#[repr(C)]` function table + exact ABI fingerprint）。
- **导出 ABI**：`kcore_*` 白名单（38 项）的实现与解析。
- **加载编排**：cpio store 解析 → ELF 段放置 / 重定位 → 入口校验。
- **失败与退出**：`fail_component`（mark + revoke + quarantine）与 `stop_component`（`Stopping` / `Stopped`）。

## 暴露什么机制

- `mod.rs`：`ComponentId`、`ComponentState::can_transition`、`is_failed`、`may_run`。
- `registry.rs`：`Registry`、`InstanceRecord`、`RegistryError`；`declare` / `resolve` / `begin_start` / `finish_start` / `begin_stop` / `finish_stop` / `mark_failed` / `record_instance_state`；全局 `get_registry`。
- `image.rs`：`ImageTable`、`ComponentImage`、`ComponentImageId`、`ImageError`；`register` / `find` / `get`、全局 `get_images`。
- `interface.rs`：`InterfaceRegistry`、`InterfaceAbi`、`InterfaceId`、`BindingId`、`BindingView`、`InterfaceKind`、`InterfaceError`；`stage_publish` / `commit_pending` / `discard_pending` / `bind` / `refresh` / `unbind` / `unbind_provider`。
- `load.rs`：`ComponentLoadError`、`current_component()`、`load_and_start()`、`create_component()`。
- `loader.rs`：`load_component()`、`LoadedComponent`、`LoaderError`；解析 ELF、放置段、应用重定位、解析 `kcomp_instance_create` / `kcomp_instance_destroy` / `kcomp_abi`。
- `elf.rs`：架构中立 ELF ET_REL 解析（`ElfObject`、`ElfError`、`ElfClass`、`Section`、`Symbol`、`Relocation`）。
- `store.rs`：内嵌 `.initpkg` cpio store（`CpioEntry`、`EmbeddedStore`、`parse_entries`、`init`、`get_component_store`）。
- `containment.rs`：panic containment（`CallOutcome`、`EscapeKind`、`EscapeInfo`、`call_component_create` / `call_component_destroy`、`enter_task` / `enter_anchor`、`panic_escape`、`with_irq_scope`、`in_irq_context`）。
- `export.rs`：`kcore_*` 导出 ABI 实现（38 项）与 `resolve(name) -> Option<usize>`。
- `failure.rs`：`fail_component`、`revoke_authority_and_unbind`。
- `exit.rs`：`stop_component`、`ComponentStopError`。
- 重导出：`panic_escape`、`ComponentStopError`、`stop_component`、`fail_component`、`ComponentImage`、`ComponentImageId`。

## 明确不做

- **组件间不建 flat ELF 符号表**：consumer 只经 Interface binding 拿到逻辑 binding（`api` / `ctx` / `generation`），provider 替换后 `refresh` 即可，无需 ELF reload。
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
| `os/core/src/component/interface.rs` | `InterfaceRegistry`、staged publish / bind / refresh / unbind |
| `os/core/src/component/load.rs` | 生命周期编排：declare → create → commit → ready |
| `os/core/src/component/loader.rs` | ELF 段放置 / 重定位 / 入口校验 |
| `os/core/src/component/elf.rs` | 架构中立 ELF ET_REL 解析 |
| `os/core/src/component/store.rs` | 内嵌 cpio `.initpkg` store |
| `os/core/src/component/containment.rs` | panic containment、执行边界、IRQ 作用域 |
| `os/core/src/component/export.rs` | `kcore_*` 导出 ABI 实现 + `resolve` |
| `os/core/src/component/failure.rs` | `fail_component`、归属撤销 |
| `os/core/src/component/exit.rs` | `stop_component`（`Stopping` / `Stopped`） |
| `os/core/src/component/generated/exports.rs` | 生成的 `EXPORTS: [Export; 38]` 表 |
