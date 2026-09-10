# 路线图（roadmap.md）

## 0. 当前进度（截至 2026-09，v0.2）

### 已完成

```text
Boot 全链：_start → FDT discovery → MachineInfo → core::init → Core Monitor（QEMU 验证，RV64/RV32 双 profile）
MMU：Sv39（RV64，identity + 高半区双映射 + high-half 交接）与 Sv32（RV32，identity）均已落地，
     KernelAddressSpace（Core 语义 ledger + AddressSpaceBackend contract）+ Sv39PageTable/Sv32PageTable
     （buddy 回调分配页表页，map/unmap/translate/激活，mid-map 失败回滚）
内核地址空间分层（boot policy 从 arch 移出）：boot crate `vm/`（layout.rs = linker symbols
     唯一解释者；bootstrap.rs = 临时静态页表；runtime.rs = 长期 buddy 页表骨架），
     arch `riscv/mmu` 只留 Sv39/Sv32 翻译机制
组件加载链（Linux insmod 模式教学版，全部 QEMU 端到端验证）：
  .kcomp（ELF32/ELF64 ET_REL，no_std Rust）→ make init.kpkg（cpio + manifest）
  → .initpkg 内嵌 → store::init（cpio 解析）
  → loader::load_component（段表/符号表解析、ALLOC 段放置、
     重定位由 `arch/riscv/elf.rs` 的 RiscvRelocator 提供：R_RISCV_CALL/CALL_PLT
     + PCREL_HI20/LO12_I + R_RISCV_32/64；host 测试直接测该实现）
  → registry（declare → resolve → start 状态机：Declared → Resolved → Ready，
     Resolved = requires 全部绑定；Failed 吸收态）
  → monitor `load <name>` → call_init（kcomp_init）
导出白名单（EXPORT_SYMBOL 教学版，os/core/src/component/export.rs）：
  10 条 kcore_*，按稳定 ABI 分类：
    Runtime/shared heap：kcore_heap_alloc / kcore_heap_dealloc（共享堆，契约 = GlobalAlloc）
    Logging：console_write_byte / log_line
    Machine query：machine_boot_hart / machine_cpu_count / machine_has_hart
    System query：free_page_count / task_count / component_count
  —— 组件只能调白名单；未导出符号（含组件间 flat ELF 符号）→ UnresolvedSymbol 整次加载失败
Component Interface Registry（os/core/src/component/interface.rs，本轮新增骨架）：
  InterfaceId / InterfaceVersion / InterfaceKind（Device/Service/Policy）/ BindingId
  publish / resolve / resolve_by_id / unbind / unbind_provider
  —— 组件→组件 依赖只走 Interface binding（逻辑 binding + versioned vtable），
     不建立 flat ELF symbol 全局符号表；provider 重绑后 consumer 无需 ELF reload
内存粒度定案：ALLOC_GRANULE（物理分配）与 AddressSpaceBackend::GRANULE（VM 映射）解耦
RISC-V trap 按特权级拆分：trap/supervisor.rs（S-mode 机制）/ trap/machine.rs（M-mode 骨架）
测试体系（自动化，见 docs/testing.md）：
  make check（fmt/clippy/host 单测/RV64 构建/RV32 check）
  make test-qemu（RV64+RV32 boot smoke + 自动执行 core_test 组件并判定 PASS）
  make test-arch（ArchTest 白盒 selftest：mapping / context switch / illegal instr /
    load fault / store-readonly fault / execute-NX fault，每 case 独立 QEMU，精确 scause 判定）
  180+ 个 host 单测（含 proptest 属性测试、parser 对抗测试与 Interface Registry 状态测试）
```

QEMU 验证输出（真实）：

```text
core> load kcomp_smoke
[smoke] hex=12            ← 组件通过白名单调用内核（component_count + free_page_count）
!load kcomp_smoke: OK (id=1, entry=0x81a00000)
```

### 缺口地图（按依赖顺序）

```text
P0 地基：
  ✅ C1  Sv39 启动（已完成：identity + 高半区双映射，RV32 另配 Sv32；ArchTest 验证 fault/权限）
  ✅ C2  启动地址去硬编码（已完成：内存布局由 FDT/链接脚本符号决定，不写死 QEMU 布局）
P1 任务系统打通：
  ✅ C3  context_switch 实机验证（ArchTest：A→B→A 双上下文 s0-s11/sp 保留，RV64+RV32）
  C4   scheduler_rr 组件接线 + monitor 调度命令（未开始）
P2 中断/驱动雏形：
  C5   timer（sbi/虚拟 CLINT）+ 时钟中断
  C6   IRQ/PLIC + 驱动模型（virtio 等）—— MmioHandle/IrqHandle 实战入口
P3 组件化进阶：
  ✅ C7  区域分配（alloc_pages(order) 已落地：MetadataHeap + MemoryLease，含失败回滚语义）
  C8   MemoryRegion lease + Core 验证的原子 region ownership transfer
  C9   任务化组件（kcomp_task + TaskTable）+ kcomp_exit / 卸载协议（逻辑层先行）
P4 执行域/隔离（推迟，触发器 = 第三方/对抗组件、硬故障隔离、可执行回收成为需求）：
  C10 Core AddressSpaceManager + 每域 Sv39 根 + ASID + U-mode（见 §10 性能模型）
```

### 下一个里程碑：M0.5 —— Sv39（✅ 已完成，2026-09；本节保留作历史规划）

> 已完成交付：boot_vm 恒等+高半区双映射、high-half 交接、Sv39PageTable/Sv32PageTable
> 动态后端、KernelAddressSpace（Core ledger + backend contract）、ArchTest 权限/trap/上下文验证。
> 下方 Phase T/B/C 与阅读计划是当时的执行记录，不再作为未来工作。

### 下一阶段方向（2026-09 架构重构后）

```text
DeviceTable / MMIO / IRQ     → 第一个 Driver Component
                             → QEMU RV32 M-mode NoMMU（trap/machine.rs + nommu.rs 骨架就位）
                             → 真实 MCU
```

- Interface Registry 已为驱动/服务提供 binding 机制；DeviceRecord 未来含
  `owner: ComponentId` + `generation`，并支持 `revoke_owner(ComponentId)`
  （见 component-model.md §3.2）——本轮不实现完整 DeviceTable。
- 设备发现链（未来方向，本轮不做）：FDT/board description → DeviceRecord →
  MMIO/IRQ/DMA authority → Driver Component → Device Interface → Service Component。

#### Phase T —— 最小 S-mode trap（0.5-1 天）

| 决策点 | 结论 |
|---|---|
| 形式 | `global_asm!` trap.S（同 entry.S 风格） |
| stvec | Direct 模式；**FDT 解析前安装** |
| 保存 | 全部整数寄存器 + 原 sp + sepc + sstatus（16 字节对齐）；先取 scause/stval |
| sscratch | 保持 0、不换栈（无用户态/备用栈） |
| 行为 | 全部 fatal：SBI 直打（不走 printk 锁）→ halt；解码 illegal(2)/access(1,5,7)/ecall(8,9)/page fault(12,13,15) |

验收：正常 boot 到 `core>`；临时 `unimp` 探针 → 打出 illegal-instruction+sepc 后停机；
撤探针 make check 绿。
陷阱：保存自减后的 sp（应存原 sp）；Rust 调用前破坏 16 字节对齐。

#### Phase B —— 粗粒度恒等映射（0.5-1 天）

- 根表 = `.bss` 静态缓冲（entry.S 已清零），**不引入 early allocator**
- 时机：MachineInfo 后、core::init 前（分配器在 core::init 里才活）
- 映射：RAM **4×1GB 大叶**（V|R|W|X|A|D）+ 低 1GB MMIO 叶（RW/NX，QEMU virt 设备区）
- **A/D 静态置 1**（像 xv6）：翻译学习与 A/D 行为解耦、可移植无 Svadu 机器；硬件置位以后单独实验
- ASID=0；激活序：写完 PTE → sfence.vma → 写 satp(MODE=8) → 再 sfence.vma
- 恒等映射下 PC/栈/全局不变；仍会挂于：PC/栈/stvec/引用数据落在未映射叶

验收：satp 前后 console 都活；monitor + `load kcomp_smoke` 照常；
故意读 0x40000000（未映射）→ cause 13；NX 处执行 → cause 12。

#### Phase C —— 4KiB 三级遍历（拆两步，共 1.5-3 天）

- 纯 Sv39 逻辑放 `os/arch/src/riscv/mmu/sv39.rs`（PTE 编解码/walk）；
  CSR/TLB 操作留 `riscv/mmu/mod.rs`
- 页表页来源：map 接口收零页分配回调（bootstrap 注入 region allocator），避免 arch→core 反向依赖；当前只服务 KernelPageTable，不提前引入通用 AddressSpaceHandle
- API 只暴露 `map_range / translate`（unmap 推迟：表回收/shootdown 未到）
- 新根在 buddy allocator 活后建（Phase B 兜底），预映射全部 RAM（4G ≈ 8MiB 页表页）
- 权限分段：.text=RX / .rodata+.initpkg=R / .data+.bss+栈+页表=RW/NX；
  **组件池暂时 RWX（loader 正从那里执行代码！设 NX 会静默弄坏现有加载器）**

验收：host tests + make check 绿；boot/monitor/load 正常；写 rodata → cause 15；读未映射 → cause 13。

M0.5 只构建永久的 KernelPageTable：不引入 AddressSpaceHandle、ExecutionDomain
隔离实例、ASID 管理或通用 AddressSpaceManager。运行期地址空间属于 C10；届时
Core 保存 owner、generation、生命周期和语义 mapping ledger，backend 保存 opaque
页表投影，所有 map/unmap/activate/destroy 都由 Core 串行化和提交。

#### M0.5 阅读计划（规范版本固定：RISC-V Ratified Specs Library **20240411 快照，Supervisor ISA 1.13**）

**Phase T 最小集**：
- Vol I：§2.2 指令格式；§7.1 CSR 指令；§7.1.1 CSR 访问序
- Vol II：§10.1.1 sstatus；§10.1.2/§10.1.6-9（stvec/sscratch/sepc/scause/stval）；
  §3.3.2 sret；§3.1.8 委托寄存器（OpenSBI 决定 fault 能否到达 S-mode）
- xv6：kernel/kernelvec.S；kernel/trap.c::trapinithart/kerneltrap（uservec 现在跳过）

**Phase B 最小集**：
- Vol II：§10.1.11 satp；§10.2.1 SFENCE.VMA；§10.3.1 PTE 位/权限/A-D；
  §10.3.2 翻译流程（normative walk）；§10.4 Sv39；Ch14 Svadu
- xv6：riscv.h（PX/PA2PTE/PTE2PA/MAKE_SATP/w_satp/sfence_vma）；
  vm.c（kvmmake/kvmmap/kvminithart）；main.c（kinit→kvminit→kvminithart 顺序）
- ⚠️ 常见误读纠正：xv6 开分页在 vm.c::kvminithart，不是 start.c（start.c 只做 M→S 委托准备）

**Phase C 前**：
- xv6 vm.c::walk/mappages/walkaddr；xv6 书 Chapter 3 "Page tables" 整章

**方法**：跳过 OSDev wiki/博客；spec + xv6 代码/书是最连贯路径；
先在纸上复现 §10.3.2 normative walk，再讨论。

**边界**：只做恒等映射 + 权限位正确；**不做**高半区（下一阶段）、每组件地址空间（C10）。

## 1. 总体阶段

```text
v0（当前）：kaleidos.elf 单镜像（bootstrap + core 链接，职责分离装载合一）
  ↑ bootstrap 阶段：firmware 世界（FDT/MachineInfo）；core 阶段：KaleidOS 世界（资源真相）。
    Bootstrap 与 Core 都只启动一次、永不热替换 → 不放独立装载边界。
    Cargo 依赖图 ≠ 运行时组件图：组件层的 .kcomp/cpio/manifest（Linux insmod 模式）是未来方向。
之后：     组件持久化（Persistent Store）→ Wasm 执行后端 → IPC/隔离 → 多 profile → 多架构 → 热替换
          （组件动态加载 .kcomp 已落地：Linux insmod/cpio/manifest 模式）
```

**v0 的野心很小**：不是"功能完整"，而是"架构骨架立起来，边界验证舒服"。
第一阶段最重要的四件事：

1. Core 的最小词汇表
2. ResourceDomain
3. Policy → Core 验证路径
4. 静态 Component Graph

## 2. M0 —— Boot（当前目标）

```text
QEMU RISC-V 启动
  → bootstrap 阶段（kaleidos.elf）：early console（_start 接收 a0=hartid / a1=dtb）
  → Machine Discovery（FDT backend）：解析 DTB → MachineInfo（内存映射/CPU 清单）
  → core::init(&MachineInfo) → Core 初始化（校验 → 提交资源真相）
  → BOOT DISCOVERY OK / BOOT CORE OK
```

**验收标准**：结构化启动日志（实际输出见 §0 已完成，RV64/RV32 均已满足）：

```text
[bootstrap] FDT magic: OK / BOOT DISCOVERY OK / [core] init OK
[core] BOOT CORE OK（RV64）/ [bootstrap] RV32 CORE OK（RV32）
```

**打包方式（组件层面，Linux 模式，既定方向）**：

```text
kaleidos.elf = bootstrap + core（链接，firmware 能启动的外壳：普通 ELF，entry=_start）
                 └ .initpkg（opaque blob，KaleidOS 自己解析，firmware 不理解）
                    → cpio 归档（initramfs 模式）：组件 .kcomp + manifest（文本）
组件 .kcomp = ELF 可重定位文件 + 符号表（.ko 模式：insmod = 放段+重定位+调 init）
manifest   = 文本清单（modules.dep 模式：depmod 生成 / modprobe 读取）
热替换 ≠ 永久安装：embedded init.kpkg（fallback）→ Persistent Store（用户安装）
                → Runtime Graph（真正在跑），manifest 决定选择
开发/发布：开发分开（kaleidos.elf + 外部 init.kpkg）；发布内嵌（重打包 → 单文件）
```

## 3. M1 —— 最小 Resource Core

实现最小的核心词汇表：

```text
TaskId        —— 任务身份
PhysicalRange —— Core 管理的物理内存区域
ComponentId   —— 组件身份
Handle        —— 不可伪造的授权（类型化，如 AddressSpaceHandle）
ResourceDomain—— 组件资源集合（拥有什么、如何回收）
Core 物理内存 —— 帧真相 + canonical 帧分配器机制（`buddy_system_allocator::MetadataHeap`，per-unit metadata O(1) buddy，metadata 自托管前端；区域 `[align_up(__bootstrap_end), RAM 末尾)`，ELF/DTB 天然保留）
```

**验收标准**：host test 覆盖上述类型的创建/存在性/所有权语义；CoreTest 能在 QEMU 上跑基础断言。

## 4. M2 —— 第一批 Component

实现：

```text
RR Scheduler      （轮转调度器）
Logger            （日志组件）
CoreTest          （核心测试组件）
```

**验收标准**：完整走通 **propose → validate → commit** 链路（调度）：

```text
Scheduler 提议运行 Task #7  →  Core 验证（存在/Runnable/未在别 CPU）→ commit
```

物理帧分配是 Core 内部机制，验收标准为 **Core 帧分配测试 / 分配器测试**：

```text
请求者向 Core 要区域 → Core 分配器选择/验证/commit → 返回 memory lease
```

对抗性测试开始建立：double free、wrong owner、invalid scheduler proposal 全部被 Core 拒绝。

## 5. M3 —— 静态 Component Graph

支持最简单的组件图操作：

```text
provides / requires / bind / start / stop
```

- 组件注册是**静态的**（代码里声明，不做动态 ELF / Wasm）；
- 生命周期状态机（Declared → Resolved → Starting → Ready → Quiescing → Stopped → Destroyed）落地；
- Ownership Tree 与 Dependency DAG 两套关系分开维护。

**验收标准**：一个配置好的 minimal profile 能按声明完成 bind → start → stop → destroy 全流程，authority-backed 资源（handle）被 revoke。

## 6. M4 —— 第一个 Device Component

建议选择：**UART** 或**简单 VirtIO block**。

```text
Core Resource Authority（grant MmioHandle / IrqHandle / DmaHandle）
        ↓
Driver Component（驱动组件）
        ↓
Device Interface（如 BlockDevice / UART 设备）
```

**验收标准**：上层组件只通过 Interface 使用设备，从不接触裸地址/裸 IRQ；
驱动可以被 stop → revoke authority（handle）→ 重新 start。走到这里如果边界仍然舒服，说明架构基本成立。

## 7. 第一阶段明确不做（务必遵守）

```text
✗ 真正动态加载          ✗ 真正 hot migration
✗ 复杂 IPC              ✗ 微内核模式
✗ Wasm runtime          ✗ WIT / IDL
✗ 完整 capability 系统  ✗ 完整 POSIX
✗ Linux syscall 兼容    ✗ 复杂 VFS
✗ 复杂 SMP scheduler    ✗ 形式化证明
✗ 完整 driver framework ✗ 完整 dependency solver
```

> 这些属于后续实验。第一阶段目的是**让架构骨架立起来**，不是功能完整。

## 8. M0 之后的方向（v0 之后，按兴趣与需要选择）

- **动态组件**：运行时加载/卸载组件（ELF 或未来 Wasm）—— 需要先有稳定的静态图做基准；
- **Wasm 执行后端**：`scheduler.wasm`、`filesystem.wasm`、`game.wasm` 作为 Component 的一种执行方式（Core/Arch 保持 native）；
- **IPC / 隔离**：UserAddressSpace 执行域，微内核形态 profile；
- **多 profile**：game、unix（POSIX personality）、micro、debug 等；
- **多架构**：x86_64 → aarch64 → loongarch64（Arch 层换实现，Core 不动）；
- **热替换**：在 Phase-1 替换模型（quiesce → stop → unbind → reset → replace → bind → start）基础上，向无感替换演进；
- **验证工具链**：Kani / Loom / Miri / Verus 逐步引入；
- **确定性测试**：Test Scheduler / Hunt Mode（CHESS 思路）；
- **内存回收（未来里程碑）**：完整 buddy、通用 Core heap、panic recovery —— 均推迟到显式未来里程碑；phase 1 只做 authority-backed 资源 revoke，不承诺共享堆字节回收。

## 9. 长期愿景

```text
Power On
  → Own Kernel
  → Own Resource Core
  → Component Graph
  → Graphics / Input / Audio / FS / Network
  → Game Runtime
  → Own Game
```

以后甚至可以：Game → AI Runtime → Small Model。

最终形态：一个用来探索操作系统、体系结构、虚拟机、runtime、驱动、组件化、可靠性和游戏系统的**个人实验平台**。BusyBox / POSIX 兼容从来不是终点，只是一种可选 Profile。

## 10. 执行域性能模型与卸载协议（2026-08 Oracle 咨询结论）

### 10.1 执行域切换成本（数量级；GHz 级硬件 + 热缓存）

| 操作 | 真硬件 | 主要成本 |
|---|---|---|
| KernelNative 直接调用 | 5-100 ns | 普通函数调用 |
| 同 satp 的任务切换 | 0.1-1 µs | 寄存器 + 栈恢复 |
| 域切换（独立 ASID，不 flush） | 0.2-2 µs | trap 路径 + satp 写入 |
| 域切换 + 本地 sfence.vma 全清 | 1-10+ µs | TLB 驱逐 + 页表重建 |
| SMP 跨核 shootdown | 5-50+ µs 尾延迟 | IPI 广播 |

> QEMU TCG 不是 cycle-accurate，ASID 收益在 QEMU 中可能不可见——用 QEMU 做功能验证，
> 不做延迟预测。

**对当前阶段的意义**：现在**零切换成本**（call_init 是普通调用；未来每组件任务共享 satp，
切换只花寄存器+栈）。"切页表"只在**隔离执行域**发生——按触发器推迟（C10）。

**正确形态（若做）**：每域一个 Sv39 根 + **ASID**（切换免 flush）+ 组件 U-mode
（低于 Core 特权；**同特权 S-mode 换页表不是恶意代码边界**）。

**否决项**：单内核页表 + 子集权限（RISC-V 无 MPK、PTE 不按键区分组件；PMP 是 M-mode 专属
且窄——都不是正解）。

### 10.2 "unwind" 澄清（卸载术语表）

| 术语 | 真相 |
|---|---|
| Rust panic unwinding | 与卸载无关；本 panic=abort 下不存在 |
| **cleanup / error-path unwind** | 加载/初始化中途失败时释放已获资源（`free_pages(base,order)` 回滚）——值得做 |
| ELF `.fini_array` / 全局 dtor | 加载器不会自动获得该语义 |
| Linux module unload | 正确参照：阻新用户 → refcount → module_exit → 清状态 → 释放内存 |

**组件失效模型**：panic 恢复的正确姿势 = **边界隔离**（trap → Core 标记 Failed → 杀任务/
隔离上下文），**不是** Rust stack unwinding（跨组件边界展开不安全）。正常回收由组件
自己做（`kcomp_exit` 显式清理 / 组件内部 RAII drop）——参考 Theseus 的
"组件失败 → 状态 Failed → 重启"模型。

### 10.3 最小卸载协议（为未来留，Phase 1 不实现物理回收）

```text
1. Ready→Unloading 原子转换，拒绝该组件新调用/任务/回调/IRQ/新 handle
2. 停止调度 + 等待活跃执行归零（任务到来前 = no-op 钩子）
3. 注销 IRQ/timer + 排空排队投递，确认无回调能进组件
4. 调 kcomp_exit（handle 仍有效，组件语义清理）；非零 → Failed + 保留内存（不冒险回收）
5. 撤销该 ComponentId 所有 handle；已通过 Core 转出的属于接收者，幸存
6. 验证零引用 → 整体释放（一次 free_pages(base, order)）
```

约束：`Registry::unload()` 绝不能直接变成物理释放原语（缺上面的协调协议）。

### 10.4 区域分配改造要留的缝（C7 执行时）

1. 单次幂次分配；记录 `base` + `allocation_order` + `allocation_size`，**`loaded_size` 分开**（slack ≠ 内容）
2. 元数据入 LoadedComponent → registry 记录 → 关联 ComponentId
3. 原始分配记录 **Core-private**（`machine::MemoryRegion` 是机器描述，不是 authority）
4. 内部精确 `free_pages(base, order)` 用于加载失败回滚（构造期清理 ≠ 运行时回收）
5. 校验：取整回绕/非零/对齐/entry 在内容内；不要现在加段权限隔离
