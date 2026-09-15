# 测试策略（testing.md）

## 1. 开发者优先（Developer-First）原则

> **Core 中所有与硬件无关的 truth logic 必须 host-testable；Core 与 Arch / Hardware 的真实契约通过 QEMU / CoreTest / 真机验证。**

如果某段 Core 逻辑只能通过启动整个 OS 来测试，第一反应应该是：**它是不是和 Arch 耦合得太深了？**
正确姿势是把与硬件无关的 truth logic（任务状态机、所有权、handle 生命周期）做成纯逻辑，在宿主上直接 `cargo test`。

### Host Test（与硬件无关的 truth logic）

```text
帧所有权 / 任务状态机 / handle 生命周期 / ResourceDomain
组件生命周期 / 权限验证 / 策略验证（policy validation）
```

### QEMU / CoreTest / 真机（与硬件相关的真实契约）

```text
context switch 是否真的保存寄存器 / 页表是否真的生效 / TLB flush 是否正确
IRQ 是否真的 delivery / timer 是否真的触发 / trap entry 是否正确
```

架构原则：

> 如果某个 Core 功能必须启动整个 OS 才能测试，首先检查它是不是和 Arch 耦合得太深。

## 2. 测试金字塔

```text
              Real Hardware（真实硬件测试）
              QEMU CoreTest（板上集成测试，黑盒组件身份）
            QEMU ArchTest（白盒内核 selftest，直接验证硬件契约）
          Concurrency Exploration（并发探索，未来）
        Model Checking（模型检查，未来）
      Property Test（属性测试，proptest）
    Host Test（宿主单测 —— 主体，日常主力）
```

- **Host Test**：Core 与硬件无关的一切真相逻辑（帧所有权、任务状态机、handle 生命周期、资源权限、组件生命周期、依赖解析器）都在宿主上测；RISC-V 的纯算法（重定位、Sv32/Sv39 页表编码与 walk）同样 host 测生产实现；
- **Property Test**：对 Core 的不变式做随机化验证（已引入 proptest，dev-dependency、仅 host profile：AddressSpace 随机序列四不变式 + parser never-panic）；
- **Model Checking / Concurrency Exploration**：未来用 Kani / Loom 类工具（见 references.md）；
- **QEMU ArchTest（系统级内核 selftest）**：feature-gated 的 test kernel，跑在**完整 `core::init` + runtime VM 之后**（device MMIO 已映射），直接验证 Arch/HAL 与真实 CPU/设备的契约——trap/scause、页表权限生效（RO/NX/未映射 fault）、context switch 寄存器保存、时钟与**外部中断**实际投递；每 case 单独 QEMU 进程；
- **QEMU CoreTest**：验证 Core 与 Arch / Machine Discovery 之间的真实契约（寄存器保存、页表生效、IRQ/timer 实际触发等），以普通 .kcomp 组件身份运行（无 god-mode）；
- **Real Hardware**：最终在真机上验证。

## 3. CoreTest 组件（os/components/core_test/）

CoreTest 是特殊的测试组件，运行在 QEMU / 真实硬件上，验证 Core 与 Arch 的**真实行为**。

### 验证清单（Core truth）

- 内存区域占用（region ownership）：分配、归属、释放
- 任务状态转换（task state transitions）：所有合法路径
- 地址空间映射（address-space mapping）
- 定时器（timer）
- IRQ（中断分配、mask、dispatch）
- handle 生命周期（handle lifetime）：创建、使用、过期
- 资源回收（resource revocation）：组件停止时 ResourceDomain 完整回收

### C6 IRQ 测试现状（2026-09）

- **Host Test**：`handle::irq` 表语义（grant/get/revoke/release/holds_line/delivery）、
  `claim_derived`（从 `MmioHandle` 推导同台设备的 IRQ、独占、`DeviceHasNoIrq`/
  `LineBusy`/`MmioHandle(WrongOwner|Stale)` 优先级）、`release`（撤销并清子标记）、
  `irq::route`（只投递给「live slot + 已注册 delivery」，revoke 后立刻截断）全部
  host 覆盖；`machine::nth_compatible` 纯枚举（ordinal/`NoSuchOrdinal`、一条描述符
  命中多个 compatible 只计一次、`NoMachineInfo`）与 `handle::mmio::claim_device`
  精确认领（越界/`NotMmio`/`DeviceBusy`/release 后新 handle + 旧 token stale）、
  child-aware `mmio::release`（live IRQ/DMA 子项 → `HasChildren`）、失败 quarantine
  （`fail_component` 后设备 `-EBUSY`）同样 host 覆盖。
- **QEMU CoreTest**：`irq-line-enable` —— 组件先 `kcore_mmio_claim` 认领 UART
  的 MMIO root，再 `kcore_irq_claim(uart_mmio_handle, ...)` 派生**同台设备**的
  中断线 → `kcore_irq_register` → `kcore_irq_enable`，再把 **PLIC 当设备 claim 进来
  读回 enable bit**，证明「Core 宣布成功」之外硬件真的被写（RV64 + RV32）；`irq-release`
  再用 `kcore_irq_release` 真正关断该线。
- **QEMU ArchTest**：ArchTest 已在**完整初始化之后**运行（`core::init` + runtime VM，
  device MMIO 已映射）。新增 `external-irq` 用例——用 UART 的 **THRE** 中断作触发源
  （打开 `IER.THRE` 即拉线，无需 runner 注入输入），验证
  `UART → PLIC → sie.SEIE → trap → dispatch_external → claim/complete` 整条链路；
  `timer` 用例继续覆盖时钟投递。踩坑记录：**先 claim 再关设备源**——UART 是电平触发，
  先关 IER 会让 PLIC pending 随电平撤销，claim 会取到 0。

### 对抗性测试（adversarial tests）—— 重点

> 核心目标：**即使 Component 是错的，Core invariant 仍然不能被破坏。**

- double free（重复释放）
- wrong owner（错误的所有者尝试操作资源）
- stale handle（过期 handle 使用）
- invalid task transition（非法任务状态转换）
- duplicate claim（重复声明资源）
- illegal map（非法映射）
- invalid scheduler proposal（无效调度提案，如调度不存在/非 Runnable/已在别的 CPU 的任务）

### 约束：没有 god-mode

- CoreTest **不得**拥有任意修改 Core 私有状态的"神权"；
- 正常测试只通过真实 Core API；
- 最多允许一个只读的 `TestInspector`（观察内部状态用于断言，不能写）。

> 理由：如果测试组件能直接改 Core 私有状态，那么"Core 不可破坏"这个结论就没被真正验证过。CoreTest 必须和任何其他组件一样受限。

## 4. Trace 与 Invariant

### 最小结构化 Trace（从早期就提供）

至少覆盖这些事件：

- task switch（任务切换）
- block / wake（阻塞 / 唤醒）
- resource grant / revoke（资源授予 / 回收）
- component lifecycle（组件生命周期变化）
- IRQ（中断事件）
- fault（故障）
- policy proposal（策略提案）
- Core rejection（Core 拒绝）

### Trace 的用途

- 调试：复现问题时的证据链；
- CoreTest：断言事件序列符合预期；
- 未来发展方向：Flight Recorder（飞行记录器）、Causal Debugger、Hunt Scheduler（确定性重放）、Fault Injection、Replay、Performance Attribution。

### Invariant 支持

Core 内提供 invariant check 机制：在关键路径断言不变式（如"同一帧最多一个 owner"、"Task 状态机合法"）。
第一阶段只需要最简单的 assert 级检查，配合 Trace 记录违规点。

### 实现现状（2026-09）

- `os/core/src/trace/`：`TraceEvent`（task switch / policy proposal·accepted·rejected /
  component state / resource grant·revoke / interface bind·refresh / IRQ
  enter·dispatch·ack）+ 固定容量 ring（`emit` 热路径 O(1)、无分配、`seq` 单调，
  ring 满逐出的条数由 `TraceStats::overwritten_total` 显式计数，序号耗尽时停止
  记录、绝不回绕）。
- 读侧：`trace::read_one`（O(1) 拷一条）+ `trace::visit_since`（**有界实时遍历，
  不是原子快照**：进入时捕获排他终点；visitor 在锁外调用，可以安全 emit / 查
  stats；覆盖造成的缺口 = `record.seq - 请求的 seq`）+ `inspector::Inspector`
  （只读快照 + trace 遍历，只返回值拷贝，无 god-mode）。组件侧状态经
  `kcore_trace_stats`（`TraceStatsAbi`：capacity / oldest_seq / next_seq /
  overwritten_total / enabled_mask）。
- **开关**：`CONFIG_TRACE`（默认 `y`）。关掉时 `trace::emit` 是内联空操作 ——
  事件参数是纯值构造，会被编译器连同调用一起消除，热路径零成本。
  **跑 benchmark 前应当关掉**（见 `docs/benchmark.md`）。编译期支持与运行时使能
  分开发现：编译期关闭时 `kcore_trace_read` 恒 `-ENOENT`、`enabled_mask == 0`；
  运行时 `enabled_mask` 报告哪些事件 kind 会被记录 —— 12 位掩码，bit i ↔ ABI
  kind i+1（`u64` 视图高位恒 0），默认全开 = `0x0fff`。
- **运行时过滤**：掩码由 Core 管理路径控制（Monitor 命令
  `trace [<category|all> on|off]`，类别 = task / policy / component / resource /
  interface / irq；`trace` 无参数打印状态）。组件没有全局 trace-control
  authority，只能经 `kcore_trace_stats` **读**。`emit` 在**关中断、加锁、读时钟
  之前**查掩码（原子 load + 分支，不是零开销）；被过滤的事件不记录、不消耗
  `seq`，不算丢失。
- **ring 容量**：`CONFIG_TRACE_CAPACITY`（int，默认 1024，`range 64 8192`）→
  `KCFG_TRACE_CAPACITY` → `os/core/build.rs` 校验 → OUT_DIR 常量（**不是** Cargo
  feature）。裸机构建缺值 / 越界直接报错，host 构建（`cargo test` / `clippy`）
  用显式默认；`.config` 仍是唯一真相。
- runtime `trace::clear()` 只清记录与逐出计数、**不回绕 `seq`**：老 reader 的
  游标仍可用，缺口可由 `record.seq - 请求的 seq` 计算。"序号回到 1" 的完整复位
  是 test-only（`trace::reset_for_test`）。
- 事件序列断言：host test 用 `trace::test_support::assert_subsequence` 断言
  **相对顺序**。不要"按 `ComponentId` 过滤后全等"：`ComponentId` 只在单个
  `Registry` 实例内唯一，而 trace ring 是进程全局的，局部 registry 的测试会和
  全局 registry 的测试复用同样的编号。
- 尚未定义的事件：`TaskBlock` / `TaskWake`（Core 还没有 block/wake 路径）、
  `Fault`（异常还在 arch 的 trap/panic 路径）。**有 chokepoint 再加**，不预先定义。

## 5. 测试纪律

- 每个 Core 新功能：先写 host test，再写实现（至少同 PR 提交）；
- 对抗性测试与功能测试同等重要 —— Core 的"拒绝错误提案"行为必须显式测试，不能只测"正常路径能过"；
- Core 的 API 每多一个，就多一份必须验证的承诺 —— 这反过来约束 Core 词汇表保持最小。

## 6. 参考（详见 references.md）

- **CHESS**：确定性并发测试 —— 未来 Test Scheduler / Hunt Mode 的思路来源；
- **Kani / Loom / Miri / Verus**：模型检查 / 并发探索 / UB 检查 / 演绎验证 —— 未来工具链；
- **FSCQ**：文件系统验证与崩溃一致性 —— 对 Component contract 强验证的方法论参考。