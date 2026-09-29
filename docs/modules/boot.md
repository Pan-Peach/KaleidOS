# boot（os/boot/riscv/）

> 成品镜像层（crate 名 `bootstrap`）：`_start` → FDT discovery → `MachineInfo` → `core::init` → Core Monitor。
> 拥有**启动期 / 物理布局那一半**；`core::init` 之后资源真相全归 Core。职责分离、装载合一（链接成 `kaleidos.elf`）。

## owns 什么真相

- CPU 入口汇编与低地址 trampoline（`entry64.S` / `entry32.S` / `entry32-nommu.S`）：BSS 清零、临时 early root、`satp` 激活、栈设置。
- **启动期临时页表**（分配器存在之前）：`vm/bootstrap.rs` 的静态池（`BOOT_ROOT` / `KERNEL_L1` / `KERNEL_L0S`），只活到 `kernel::init()` 完成之前。
- **FDT discovery → `MachineInfo` 归一化**（内存 / CPU / 设备），以及 timer / PLIC 的机器侧接线。
- **链接符号的唯一解释者**：`vm/layout.rs`（`KernelLayout`、`kernel_layout()`），固定 `KERNEL_VMA = 0xffff_ffc0_8020_0000` 与 `HIGH_HALF_OFFSET`。
- **high-half 交接**（`bootstrap::enter_high_half`）与存活其上的 `BootContext`。
- **长期内核地址空间**的构建 / 校验 / 激活：`vm/runtime.rs`（`RuntimeVm`、`build` / `verify` / `activate` / `init`）。
- 内嵌组件归档 `.initpkg`（`INITPKG` static + 链接段符号）。
- `#[panic_handler]`（含组件 containment 逃逸报告）与 boot console shim（转发给 `arch::ConsoleImpl`）。

## 暴露什么机制

- RV64：`bootstrap_main(hart_id, dtb_pa, kernel_pa)`（`main64.rs`）→ `bootstrap_high(context_ptr)`（高半区别名进入）→ `kernel::init(&context.info, &context.reserved)` → `runtime::init(...)` → `kernel::component::store::init(pkg)` → `CpuImpl::enable_irq()` → `smp::start_secondaries(&info)` → `kernel::monitor::run()`。
- **SMP（RV64）**：boot 在 `kernel::init` + 长期地址空间 + 全局中断之后调 `crate::smp::start_secondaries`（SBI HSM `hart_start` 启动 AP）。**归一化不变式**：discovery 后把 cpu 顺序调整为 **boot hart = 逻辑 CPU0**（OpenSBI 抽签使 boot hart 不一定是 DTB 首个 CPU；不归一化会让 PLIC 外部路由 / per-CPU 表指向错误的 hart）。AP 入口 `secondary_main`（`src/smp.rs`）+ 物理 trampoline `_secondary_start`（`secondary64.S`）；AP 目前完成本地初始化后 `wfi`（尚未进入 Core 调度，见 `docs/modules/arch.md` SMP 章节）。
- RV32：`bootstrap_main`（`main32.rs`）→ `discover(dtb_pa, hart_id)` 返回 `MachineInfo` → `kernel::init` → store init → `monitor::run()`。
- `selftest` feature：走 `crate::selftest::run(&info)`（ArchTest，白盒 selftest，返回 `!`）。
- 关键内部符号：`layout::kernel_layout()`、`bootstrap::init` / `install_identity_alias` / `install_kernel_alias` / `root_pa` / `enter_high_half`、`runtime::RuntimeVm`、`print_linker_layout()`、`configure_machine_timer` / `configure_interrupt_controller`。

## 明确不做

- **不持有资源真相**：`core::init` 消费并校验 boot 提议的 `MachineInfo` + `reserved`（ELF 镜像范围）后才提交。
- **不管帧分配器**：长期页表通过 `kernel::memory::vm_page_alloc`（buddy）取页；分配器归 Core。
- **不做 BSS 清零以外的 Core 初始化**：BSS 清零是启动路径职责，core 不再负责。
- 不做 `#[cfg]` 之外的平台判断、不把板卡名渗透进资源语义（platform quirks 只是 escape hatch）。

## 代码在哪

| 路径 | 内容 |
|---|---|
| `os/boot/riscv/src/main.rs` | profile 选择、`compile_error!` 守卫、模块接线 |
| `os/boot/riscv/src/main64.rs` | RV64 启动：`bootstrap_main` / `bootstrap_high` / `core::init` / monitor |
| `os/boot/riscv/src/main32.rs` | RV32 启动：`bootstrap_main` / `discover` / `core::init` / monitor |
| `os/boot/riscv/src/entry64.S` / `entry32.S` / `entry32-nommu.S` | `_start` trampoline、early root、high-half 交接 |
| `os/boot/riscv/src/vm/{mod,bootstrap,layout,runtime}.rs` | 启动页表 / 布局解释 / 长期地址空间 |
| `os/boot/riscv/src/console.rs` | boot console shim |
| `os/boot/riscv/src/selftest.rs` | ArchTest selftest 入口（`run`）；含 `smp-boot`/`smp-ipi`/`smp-percpu` |
| `os/boot/riscv/src/smp.rs` / `secondary64.S` | RV64 SMP：`ApBoot` 描述符、AP 栈、`start_secondaries`、`secondary_main`、AP 物理 trampoline |
| `os/boot/riscv/linker.ld` | RV64 高 VMA / 低 LMA 布局（`KERNEL_VMA` 等符号） |
| `os/boot/riscv/linker32.ld` | RV32 identity 布局 |
| `os/boot/riscv/.cargo/config.toml` | 按 target 指定链接脚本；**无默认 target**（由 Makefile 传 `KCFG_TARGET`） |
| `os/boot/riscv/Cargo.toml` | crate `bootstrap`，独立 workspace，`panic = "abort"` |

> `linker.ld` 的 `HIGH_OFFSET` / `KERNEL_VMA` 必须与 `vm/layout.rs` 及 `arch/src/lib.rs` 的 `HIGH_HALF_OFFSET` 保持一致。
