# boot（os/boot/riscv/）

> 成品镜像层（crate 名 `bootstrap`）：`_start` → 无堆早期内存 pass（arena + `early_init`）→ FDT discovery → `MachineInfo` → `core::init` → profile 选中的启动组件 → monitor 调度锚点。
> 拥有**启动期 / 物理布局那一半**；`core::init` 之后资源真相全归 Core。职责分离、装载合一（链接成 `kaleidos.elf`）。

## owns 什么真相

- CPU 入口汇编与低地址 trampoline（`entry64.S` / `entry32.S` / `entry32-nommu.S`）：BSS 清零、临时 early root、`satp` 激活、栈设置。
- **无堆早期内存 pass（Phase 4a）**：在分配器存在之前，从已校验的 firmware/boot 记录里选出唯一一段连续、页对齐、已排除镜像（含 boot 栈 / 静态页表 / 内嵌包 / staging buffer）与 live/reserved 区间（RV/AArch64：保留的 FDT 整体、header reservation map、`/reserved-memory`；x86_64：PVH `hvm_start_info` + memmap、整个 MB2 信息块或保留的 RSDP）的 RAM **arena**，然后 `kernel::memory::early_init(arena)`（一次性）。RV64/RV32 的搜索窗口收在镜像的 ±2 GiB PC-relative 重定位可达范围内（KernelNative 组件镜像由 Core 堆分配）。
- **保留的原始固件源（Phase 4c）**：`MachineInfo.firmware`（`FirmwareInfo`）指出已校验的原始描述字节在哪里、有多大——归一化视图不是硬件描述的终点，未来驱动可据此读 vendor 数据。各 arch 保留什么：RISC-V = 入口给出的**原始 DTB 物理区间**（`Fdt { phys, size }`，fdt parser 已验证的 totalsize）；AArch64 = **staging 副本**（`Fdt`，重写为实际复制前缀的 totalsize；绝不保留原 PA-0 地址）；x86_64 = 校验通过的 RSDP（`Acpi { rsdp }`，PVH `hvm_start_info.rsdp_paddr`（offset 32）/ MB2 ACPI RSDP tag 内嵌副本）；没有有效 RSDP 或没有受支持固件描述 = `Static`（不是"校验失败但继续"）。保留区间**永久**排除出 arena（本阶段没有 reclaim）；RV runtime root 对落在 `memory_regions` 之外的保留 FDT 建显式 Core 只读映射（identity，只进内核 root——不提供 isolated-component PA 查询）；早期分配之后 boot 重读保留字节自检（FDT magic + totalsize / RSDP 完整复核）。
- **启动期临时页表**（分配器存在之前）：`vm/bootstrap.rs` 的静态池（`BOOT_ROOT` / `KERNEL_L1` / `KERNEL_L0S`），只活到 `kernel::init()` 完成之前。
- **FDT discovery → `MachineInfo` 归一化**（内存 / CPU / 设备 + 完整中断资源），以及 timer / PLIC 的机器侧接线。RV64 在 high-half entry 里、`early_init` 之后才做完整 discovery；低地址阶段只做内存 pass（不构造 `MachineInfo`、不分配）。
  - **设备中断资源（Phase 4d）**：`interrupts-extended` 每条 tuple（phandle + 该控制器 `#interrupt-cells` 个 cell）与 `interrupts`（经继承 `interrupt-parent` 解析）都被完整保留成 `InterruptResource`（`specifier` = 控制器 + cells；`line` = 逻辑外部 IRQ 号）。malformed / unresolved controller → **发现错误并终止 boot**（不静默丢弃）；缺 IRQ 属性 = 空列表。
  - **PLIC 逻辑线绑定（RISC-V）**：只把属于已配置 PLIC（第一个 PLIC-compatible 节点）且 source 在 `riscv,ndev`（缺省退回后端上限）范围内的 specifier 绑定成 `line`；CPU-local（cpu-intc）与其它控制器保持 `line: None`。AArch64 无 GIC 路由、x86_64 无 PIC/IOAPIC 路由，资源保留但 `line: None`。
- **启动期外部输入校验**：bootloader / firmware 提供的数据一律当**不可信输入**——先校验，失败即拒绝 + log，绝不盲信指针 / 计数。
  - **x86_64**：MB2 校验 `total_size` 上限（64 KiB）、每个 tag 必须完整落在信息块内、mmap tag 先证明 16 字节条目头存在再计算条目数（`8 <= tag_size < 16` 时直接 `tag_size - 16` 会下溢），条目读取全程 checked arithmetic；PVH 校验 `version >= 1`、`memmap_paddr != 0`、`memmap_entries <= 4096`、`memmap_paddr + entries*24` 不溢出且 < 4 GiB。校验失败返回空表，由调用方以 "no usable RAM region" fail-closed。**RSDP**：PVH 的 `rsdp_paddr`（offset 32）与 MB2 的 ACPI RSDP tag（15 new 优先、14 old 兜底）都先校验签名 `"RSD PTR "`、revision 对应长度（v1 20B / v2+ 36B 且 `length == 36`）、第一 + extended 校验和、保留区间地址算术；**只认证 RSDP 本身**，RSDT/XSDT 下游表由未来消费者读取时各自校验。无有效 RSDP → `FirmwareInfo::Static`（当前静态 BSP/UART 平台合法地是 `Static`）。
  - **aarch64**：复制前校验 FDT header 的 magic / version(16·17) / `totalsize` 上限 / struct、strings 与 rsvmap 边界（rsvmap 走到 16 字节零对终结符；全部 checked arithmetic），`used` 覆盖 rsvmap 且 ≤ `DTB_COPY_CAPACITY`；header 不自洽即拒绝（`None` + log），**绝不"修复"坏 blob**，只重建 `totalsize` = 实际复制前缀的归一化副本。**QEMU `-kernel <elf>` 的 PA-0 DTB quirk** 用有界、经校验的探测处理：x0 → PA 0 → 1 MiB 低 RAM 窗口，每步只接受 header 完全自洽的候选；MMU 关闭 + identity mapping 下按物理地址做 raw volatile 读，不构造引用、不假设对齐。
- **链接符号的唯一解释者**：`vm/layout.rs`（`KernelLayout`、`kernel_layout()`），固定 `KERNEL_VMA = 0xffff_ffc0_8020_0000` 与 `HIGH_HALF_OFFSET`（**boot 本地布局常量**，不是对 arch 公共 HAL 的固定偏移承诺）。
- **high-half 交接**（`bootstrap::enter_high_half`）与存活其上的 `BootContext`。
- **长期内核地址空间**的构建 / 校验 / 激活：`vm/runtime.rs`（`RuntimeVm`、`build` / `verify` / `activate` / `init`）。
- 内嵌组件归档 `.initpkg`（`INITPKG` static + 链接段符号）。
- `#[panic_handler]`（含组件 containment 逃逸报告）与 boot console shim（转发给 `arch::ConsoleImpl`）。

## 暴露什么机制

- RV64 低入口用四张 L0 页表覆盖最多 8 MiB 镜像窗口，链接时拒绝超出，避免 per-CPU 栈增加后填表越界写坏代码。
- RV64：`bootstrap_main(hart_id, dtb_pa, kernel_pa)`（`main64.rs`；低地址阶段 = FDT 解析 + 无堆内存 pass + 静态 bootstrap 页表 → `BootContext`）→ `bootstrap_high(context_ptr)`（高半区别名进入）→ `memory::early_init(arena)` → `discover(&tree, hart_id)` → `kernel::init(&info, &context.reserved)` → `runtime::init(...)` → `kernel::component::store::init(pkg)` → `CpuImpl::enable_irq()` → `smp::start_secondaries(&info)` → `composition::start()` → `kernel::monitor::run()`。
- **SMP（RV64）**：boot 在 `kernel::init` + 长期地址空间 + 全局中断之后调 `crate::smp::start_secondaries`（SBI HSM `hart_start` 启动 AP）。**归一化不变式**：discovery 后把 cpu 顺序调整为 **boot hart = 逻辑 CPU0**（OpenSBI 抽签使 boot hart 不一定是 DTB 首个 CPU；不归一化会让 PLIC 外部路由 / per-CPU 表指向错误的 hart）。AP 入口 `secondary_main`（`src/smp.rs`）+ 物理 trampoline `_secondary_start`（`secondary64.S`）；AP 交给 Core 完成 per-CPU 初始化后进入本 CPU 的组件任务调度，无工作时休眠（见 `docs/architecture/scheduling.md`）。
- RV32：`bootstrap_main`（`main32.rs`）→ FDT 解析 + 无堆内存 pass → `memory::early_init(arena)` → `discover(&tree, hart_id)` 返回 `MachineInfo` → timer/PLIC 接线 → `kernel::init`（两条 profile 共用）→ 长期 root + store init → `selftest::run` / `composition::start()` → `monitor::run()`。
- **新 ISA（x86_64 / aarch64）现状**：x86_64 经 Multiboot2 / PVH 发现可用 RAM，CPU 只报告 BSP（无 ACPI MADT），`timebase_frequency = None`（**显式未知**：TSC 频率不可发现，**不伪造** 1 GHz）；aarch64 读 `CNTFRQ_EL0`，非零时发布 `Some(Hz)`、为零时同样诚实报 `None`。两者都只跑 `selftest` 镜像（`boot` 用例），timer 投递分别诚实报告 `Unsupported` / `DeliveryUnavailable`（控制台靠 Core 轮询）；aarch64 非 selftest 的 Core Monitor 入口仍是 `todo!()`（`os/boot/aarch64/src/main.rs`）。
- **aarch64 boot 栈 = 64 KiB**（`linker.ld` `.bss.stack`）：`resource::init` 会在 ~10 KiB 调用深度上构造 8 KiB 的 `IrqTable` 栈临时量；16 KiB 栈会溢出到 `__boot_stack_bottom` 之下（`.data` 末尾的全局堆 / 已提交 `MachineInfo`）并静默破坏 live statics。
- `selftest` feature：走 `crate::selftest::run(&info)`（ArchTest，白盒 selftest，返回 `!`）。
- 普通 RISC-V 入口在 store / IRQ 就绪后调用 `composition::start()`，按
  `CONFIG_BOOT_COMPONENT` 装载一个 KernelNative artifact；空串跳过，失败记录诊断后
  回 monitor。默认 [`init`](init.md) 负责 scheduler / driver / filesystem / ksh，boot 不编排图。
- 关键内部符号：`layout::kernel_layout()`、`bootstrap::init` / `install_identity_alias` / `install_kernel_alias` / `root_pa` / `enter_high_half`、`runtime::RuntimeVm`、`print_linker_layout()`、`configure_machine_timer` / `configure_interrupt_controller`；保留源自检：`bootmem::retained_fdt_intact`（RISC-V）/ `discovery::staged_dtb_intact`（aarch64）/ `main::validate_rsdp`（x86_64）。

## 明确不做

- **不持有资源真相**：`core::init` 消费并校验 boot 提议的 `MachineInfo` + `reserved`（ELF 镜像范围）后才提交。
- **不管帧分配器**：boot 只做一次无堆的 arena **选择**并 `early_init` 一次性移交；分配器本体、区域记账与长期页表取页（`kernel::memory::vm_page_alloc`）都归 Core，`core::init` 不重置内存。
- **不做 BSS 清零以外的 Core 初始化**：BSS 清零是启动路径职责，core 不再负责。
- 不做 `#[cfg]` 之外的平台判断、不把板卡名渗透进资源语义（platform quirks 只是 escape hatch）。

## 代码在哪

| 路径 | 内容 |
|---|---|
| `os/boot/riscv/src/main.rs` | profile 选择、`compile_error!` 守卫、模块接线 |
| `os/boot/riscv/src/composition.rs` | profile 选中 artifact 的单次装载、错误回 monitor |
| `os/boot/riscv/src/main64.rs` | RV64 启动：`bootstrap_main` / `bootstrap_high` / `discover` / `core::init` / monitor |
| `os/boot/riscv/src/main32.rs` | RV32 启动：`bootstrap_main` / `discover` / `core::init` / monitor |
| `os/boot/riscv/src/addr.rs` | RV32 boot + 两个 XLEN 的 selftest 共用的 boot 本地链接地址归一化（`linked_to_physical`；RV64 委托 `vm::bootstrap`，RV32 identity） |
| `os/boot/riscv/src/bootmem.rs` | RV64/RV32 共用的无堆内存 pass：`image_bank` / `arena_search_window` / `scan_fdt_exclusions` |
| `os/boot/riscv/src/discovery.rs` | RV64/RV32 共用的 FDT 设备发现：完整中断资源解析 + PLIC 逻辑线绑定 |
| `os/boot/riscv/src/entry64.S` / `entry32.S` / `entry32-nommu.S` | `_start` trampoline、early root、high-half 交接 |
| `os/boot/riscv/src/vm/{mod,bootstrap,layout,runtime}.rs` | 启动页表 / 布局解释 / 长期地址空间 |
| `os/boot/riscv/src/console.rs` | boot console shim |
| `os/boot/riscv/src/selftest.rs` | ArchTest selftest 入口（`run`）；含 `smp-boot`/`smp-ipi`/`smp-percpu` |
| `os/boot/riscv/src/smp.rs` / `secondary64.S` | RV64 SMP：`ApBoot` 描述符、AP 栈、`start_secondaries`、`secondary_main`、AP 物理 trampoline |
| `os/boot/riscv/linker.ld` | RV64 高 VMA / 低 LMA 布局（`KERNEL_VMA` 等符号） |
| `os/boot/riscv/linker32.ld` | RV32 identity 布局 |
| `os/boot/riscv/.cargo/config.toml` | 按 target 指定链接脚本；**无默认 target**（由 Makefile 传 `KCFG_TARGET`） |
| `os/boot/riscv/Cargo.toml` | crate `bootstrap`，独立 workspace，`panic = "abort"` |

> `linker.ld` 的 `HIGH_OFFSET` / `KERNEL_VMA` 必须与 `vm/layout.rs` 的 `HIGH_HALF_OFFSET` / `KERNEL_VMA` 保持一致。这是 **boot 本地布局契约**，不上行到 arch 公共 HAL（见 `docs/modules/arch.md`）。
