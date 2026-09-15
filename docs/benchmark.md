# Benchmark（性能基准）

> 本阶段是 **measurement phase**：先拿到可靠、低侵入、可测量的数据，再决定优化。
> 与 correctness test 严格分离：正确性看断言，性能看趋势。
> 相关：`docs/testing.md`（测试策略 / Trace）、`os/core/src/bench/`（host harness）、
> `os/components/kbench/`（目标端组件，monitor `load kbench` 触发）。

## 1. 运行

```sh
make bench          # host release，手动跑；不进 CI
```

`make bench` 固定使用 **trace 关闭** 的 profile（`--no-default-features
--features supervisor,vm-mmu`）：`CONFIG_TRACE` 的探针正好落在被测路径上，
开着会污染数字。这也顺带验证了"关掉即零成本"。

harness 会把 git commit 带进报告（`KALEIDOS_GIT_COMMIT`，由 Makefile 传入），
这样数字和代码版本能对上。

目标端跑 `kbench`（monitor：`load kbench`）：它走真实的 `rdtime` 与真实导出
调用路径，输出与 host 同一套 `BENCH-ENV` / `BENCH <name>` / `key=value` 约定，
但 `unit=timebase-ticks`（换算成时间需要 `timebase_hz`）。目标端现在包含：

- `kbench.clock_read` / `kbench.free_pages_query`：无 authority 的导出调用成本；
- `sched.yield_roundtrip`：**两个组件自有任务**的完整 A→B→A 调度交接（走既有
  任务/调度导出，无 benchmark 特权；协议与诚实声明见 §3.5）；
- `irq.uart_trigger_to_handler`：**合法持有的** ns16550a 自触发 → 组件 handler
  入口（见 §3.6；设备已被其它组件持有时如实报 blocked，不索取特权）。

`sched.yield_roundtrip` 的**权威数字应在 `CONFIG_TRACE=n` 的构建上采集**：trace
的 emit 正落在被测路径上。`BENCH-ENV` 会如实报告 trace 状态（`trace_mask` /
`trace_capacity` / `trace_records`）——非 0 掩码表示数字包含 emit 成本；
掩码为 0 表示被测路径只有"标记 + 分支"的过滤成本（编译期 `CONFIG_TRACE=n` 与
运行时掩码全关在组件可观测的 ABI 上同形，`trace_records=no` 不足以区分两者）。

## 2. 输出格式

第一阶段只输出**机器可解析的纯文本**（不做可视化、不做总分）：一次运行先打印
一行环境，然后每个 primitive 一段 `key=value`。

```text
BENCH-ENV arch=host xlen=64 platform=undetected timebase_hz=0 privilege=supervisor vm=mmu trace=off preempt=off clock=std::time::Instant mode=release commit=f00a5eba82a9
BENCH interface.direct_call
method=batch
unit=ns
sample_unit=batch_total
operations_per_batch=4096
rounds=5
batches_per_round=31
iterations=634880
samples=155
clock_quantum=1
clock_probe_reads=1024
clock_zero_deltas=0
clock_backwards=0
clock_observed_min_delta=230
clock_median_read_delta=250
clock_bracket_min=236
calibration_target=23600
batch_cap=1000000
calibration_retries=0
min=29906
median=30853
mean=31412
p95=31376
max=90813
total=4868951
round_0_median=30627
round_1_median=31018
round_2_median=30393
round_3_median=31091
round_4_median=30893
below_floor_batches=0
resolution_limited=no
baseline=paired_null
baseline_min=10497
baseline_median=10529
baseline_p95=10581
baseline_max=29265
baseline_total=1660616
baseline_paired_diff_median=20295
status=ok
```

- **`BENCH-ENV`**：没有这一行数字不可比。`platform` 如实写 `undetected` ——
  Core / 组件没有运行时的板级/QEMU 探测，**QEMU 与真机的区分必须由 runner 记录**；
  加速器（TCG/KVM）、`config_hash`、`run_id` 同样由 runner 补充，组件不猜。
  目标端额外报告 trace 状态：`trace_mask`（运行时使能掩码；非 0 = 被测路径可能
  真的 `emit`）、`trace_capacity`、`trace_records`（是否真的读到过记录）。
- **`unit`**：目标端是 `timebase-ticks`（`rdtime`，换算需要 `timebase_hz`），
  host 是 `ns`。两者不可直接比较。所有时间量都是**原始整数**；小数换算由
  报告工具做：`ns/op = d * 10^9 / (K * f)` —— **先乘后除**，测量端不截断
  （`f` = `timebase_hz`；host 单位已是 ns，退化为 `d / K`）。
- **统计对象是 batch 总时长**，不是单次操作：`d` 是 `K = operations_per_batch`
  次操作夹在**恰好两次读钟**之间的总用时。`min/median/p95/max/total` 全部是
  batch 样本；单次操作的分母是 `K`。**batch p95 ≠ 单次操作 p95。**
- `iterations = operations_per_batch × samples`：本次测量覆盖的总操作数。
- `round_N_median`：每轮 31 个 batch 的中位数（看轮间散布；散得厉害 = 有干扰）。
- `below_floor_batches`：**最后一轮**里低于 `calibration_target` 的 batch 数；这些
  观测的原始值照常计入统计，**不静默丢弃**。
- `calibration_retries`：因 below-floor 采集而执行的翻倍重采次数（0–3，有界；
  最终 K 见 `operations_per_batch`）。重采会替换该轮样本，报告只写最后一轮。
- `resolution_limited`：`calibration_target > batch_cap`，或**重试已用尽**而最后
  一轮仍有 below-floor 观测 —— 即本次运行没有达到"够用"的批时长，读者不应把
  小数位当成有效精度。
- `baseline_*` / `baseline_paired_diff_median`：同一 K 下交替测的 null baseline
  （只有循环 / `black_box`，没有实际工作）。差值小或为负 = 增量成本无法分辨。
- `status`：`ok` / `clock_unusable`（时钟不前进或倒退，直接不测、如实上报）；
  另有 primitive 专属的 `scheduler_unavailable` / `task_setup_failed` /
  `handshake_mismatch`（见 §3.5）与 `mmio_not_owned` / `lease_failed` /
  `mmio_window_too_small` / `trigger_timeout`（见 §3.6），含义都在同一行写明。
- **`sched.yield_roundtrip` 附加 key**：`handoff_count`（正式采样窗口内完成的
  A→B→A 往返数，无 mismatch 时 == `iterations`）、`handoff_total`（含
  warmup/pilot/重试的总往返数）、`handoff_mismatches`（不是"恰好一次 B 激活"的
  body 次数，> 0 = 数字不可信）、`trace_validation`（独立正确性验证结果：
  `ok` / `masked` / `lost_records` / `mismatch` / `read_failed`）；紧随一个
  `BENCH sched.yield_roundtrip.verify` 块给出 `sched_run` 状态、两个任务的终态、
  `b_activations` 与 `handoff_total` 的终态对账。
- 目标端**不打印 `mean`**（组件内不做除法）：host 工具用 `total / samples` 换算；
  host harness 因为可以直接除，仍会打印 `mean`。

## 3. 方法论

### 3.1 批量计时（为什么不是"逐次夹钟取最小"）

逐次 `t0; op; t1` 得到 `d = op + 端点量化 + 读钟开销`。10 MHz timebase
（1 tick = 100 ns）上短操作的 `d` 可能只有个位数 tick：量化误差占比极大，
而且**可能向下取整**；同时每次操作都摊上两份读钟开销。因此：

- 把 `K` 次操作夹在**恰好两次读钟**之间，`ticks_per_op = d / K`；
  端点量化被摊薄到约 `q/K`；
- 统计对象是 batch 总时长（原始整数）；换算与小数格式化全部移到 host。

批量计时修正的是"读钟粒度 + 计时开销占比"，**不**消除宿主抖动 / 循环开销 /
bias —— 这四项要分开说，不能拿一个 min 全包了。

### 3.2 时钟刻画与 K 校准

1. **有界探测**：最多读钟 `clock_probe_reads` 次，记录零 delta、倒退次数、
   最小正 delta、正 delta 中位数。最小正 delta **包含读钟调用开销**，
   不是分辨率的证明；中位数才是 back-to-back 读钟成本的量级。
   时钟整段不前进 → `status=clock_unusable`，终止测量（绝不硬报数字）。
2. **bracket 开销**：空 batch（只有 `black_box`）的最小时长。
3. **先温热**：校准**之前**跑固定 batch 数的 K=1 warmup。冷启动样本（首次调用 /
   首次缺页 / 冷分支）能把 K=1 的 pilot 中位数抬到 target 之上，制造假
   overshoot 把 K 钉死在 1（那样 153/155 个 batch 都会低于 floor，测量报废）。
4. **选 K**：从 `K=1` 开始翻倍；每个 K 跑 **3 个 pilot batch 取中位数**
   （避免用一次宿主停顿决定 K），直到批时长达到
   `calibration_target = max(200*q, 100*bracket)`（`q` = 标称量子），
   或触及 `batch_cap`（1 ms）/ K 上限。两个系数与 cap 都是**报告出来的 policy
   参数**，不是普适常数。**K=1 的 pilot 不参与停止判定**：只有从 K=2 起，
   pilot 达 target / 超 cap 才停止向上探测（K=1 的假 overshoot 不可信）。
5. **冻结 K 采集**：warmup（按 K 成批，不计入）后收集
   `rounds × batches_per_round = 5 × 31 = 155` 个有界 batch 样本；
   全部保留，百分位覆盖整个测量（没有"只统计前 N 个样本"的截断）。
   若本轮低于 floor 的样本 > 0，且 `2K` 不超过 K 上限与 `batch_cap`，
   翻倍 K 重采（最多 3 次）；`operations_per_batch` / `calibration_retries`
   报告的是最终 K 与重试次数。只有重试用尽（或 `target > cap`）才标
   `resolution_limited=yes`。
6. **打印、排序、校准全部在所有计时区间之外**；每轮之间不打印。

**没有任何 K 能保证"不被打断"。** 本 harness **不关中断**、不制造"干净数字"：
`below_floor_batches` / `resolution_limited` 就是如实暴露它的地方。

### 3.3 baseline 纪律

- 被测体的结果消耗（`black_box`）在计时区间**内**；
- 同一 K 交替测 work 与 null baseline（顺序逐批交替，抵消单调漂移）；
- 原始 work 与 baseline 都先报告；配对差值中位数只是附加信息；
- **不**把独立挑出的最小值相减，**不**把负差值 clamp 成 0，不做"修正"。

### 3.4 其他既有纪律

- **拆 primitive**（借鉴 lmbench 的思路）：一次只测一个基本操作，不把
  "proposal + validate + commit + switch" 混成一个数字。
- **hot path 与 control path 分开报**：`bind` / `publish` / `derive_lease`
  是一次性成本，绝不能和"每次调用都要付"的成本混在一起。
- **不造总分**（借鉴 UnixBench 的"重复运行形成 baseline"，但不学它的综合分）：
  先报告原始数据，趋势由人判断。
- 必须避免的错误用法：把被测体优化掉（`black_box` 兜住返回值）、在测量循环里
  打印、不做 warmup、迭代太少、关中断凑数。

### 3.5 `sched.yield_roundtrip`：真实任务交接对

**协议**（全部走既有导出，无 benchmark 特权）：

1. `kcore_interface_available("scheduler", Policy, ABI)` 确认调度配置；没有就正常
   加载 `scheduler_rr`（与 core_test 同一条链）；两者都不可用 → `scheduler_unavailable`。
2. `kcore_task_create` 建两个组件自有任务 A/B（entry 必须落在本组件镜像内），
   `kcore_task_start` 启动，`kcore_sched_run` 从锚点进入调度。
3. **body = 一次 A→B→A 往返**：A `kcore_task_yield()` → Core propose→validate→
   commit→`__switch` → B 运行、计数、`kcore_task_yield()` → A 恢复。
   `operations_per_batch` 就是每批往返数；warmup / K 校准 / 5×31 采样 /
   配对 null baseline 与其它 primitive 完全同一套 `measure` 流程。
4. **启动/退出在计时之外**：B 的首次激活落在 K=1 warmup（不计入样本）；
   A 在采样与打印全部结束后才 `task_exit`，B 看到 `A_DONE` 后退出。
5. **时钟只在 A 的 batch 边界读**（`measure::batch` 在调用者上下文）；B 从不读钟。

**诚实声明（必须连同数字一起读）**：

- 这是**两条调度路径 + 两次 handoff** 的端到端成本：yield 调用、RR policy 选择、
  Core 验证、状态 commit、`__switch`、以及 harness 的 handshake 计数/循环。
  **不是**裸寄存器 save/restore 延迟。
- 已有 `TaskSwitch` trace 的 timestamp **不能**测纯切换延迟：它在状态 commit
  **之后**、真正切走**之前**记录，落在路径中间而不是两端——本 primitive 不派生
  自它，只用 `rdtime` 在 A 的 batch 边界夹住整段。
- **handshake 是逐次断言**：每次 yield 前读 B 的激活计数、恢复后必须恰好 +1，
  否则记 mismatch；任何 mismatch → `status=handshake_mismatch`（原始数字照打，
  但明确标注不可信）。`handoff_count` 只统计正式采样窗口，无 mismatch 时恒等于
  `iterations`。
- **独立的正确性验证**（计时之外）：trace 的 TaskSwitch 事件使能时，A 先做 16 次
  往返，再按 Core 的 `TaskSwitch` 事件流验证 32 次切换严格 A→B / B→A 交替 →
  `trace_validation=ok`；掩码关闭 → `masked`；ring 逐出造成缺口 → `lost_records`；
  pattern 不符 → `mismatch`（并让主块 `status=handshake_mismatch`）。这是"用
  tracing 验证真实 handoff 数"的独立通道（另一份计数来源）。
- batch 在 QEMU TCG 上跑：只作**同环境相对趋势**，不做真机预测。数字包含什么
  trace 成本由 `BENCH-ENV` 的 `trace_mask` 决定（§1）。
- **分摊怎么做（host 侧）**：`baseline_paired_diff_median` 是每批的增量成本；
  单次 A→B→A 往返（两次 handoff）的摊销 = `baseline_paired_diff_median /
  operations_per_batch`，单次 handoff 再除以 2。**这个除法由 host 做**（组件内不
  允许除法）；分子包含 policy 选择 + Core 验证 + commit + `__switch` + harness
  计数，**不是**裸寄存器 save/restore。
- **门禁建议（两次运行）**：`CONFIG_TRACE=n` 的构建采权威数字（此时
  `trace_validation=masked` 是预期值，表示"没有 trace 可用"）；`CONFIG_TRACE=y`
  的构建专门跑独立正确性验证，要求 `trace_validation=ok` 且主块
  `status=ok`。`irq.uart_trigger_to_handler` 需要一台**未被认领**的 ns16550a：
  `load kbench` 之前不要 `load core_test`（否则如实报 `mmio_not_owned`）。

### 3.6 `irq.uart_trigger_to_handler`：owned 设备自触发

**合法性边界**（不制造任何 benchmark 特权）：`device_nth` 找 ns16550a →
`mmio_claim` 认领**确切设备** → `irq_claim` 派生同设备中断线 → `irq_register`
正常注册 → `mmio_lease` 派生裸指针 → `irq_enable` 开线。触发是写自己设备的
IER/THR（TX-empty 中断），handler 用同一 owned device 的 lease 指针清 source。
没有 raw PLIC 访问、没有全局中断控制、**不关中断**。

**寄存器编址（为什么 IER 走 lease 字节写）**：QEMU virt 的 ns16550a 没有
`reg-shift`（`serial_mm_init(..., regshift=0, ...)`），寄存器按**字节**编址：
THR@0x00、IER@0x01。`kcore_mmio_write_u32` 的 offset 是字节偏移且要求 4 字节
对齐，表达不了 IER；因此 IER 的置位/清除只走 `kcore_mmio_lease` 派生的裸指针
字节写（Core 校验过一次的 KernelNative 快路径）。设备窗口长度取 FDT 的
`reg`（ns16550a = 0x100，**不是一整页**）——曾把"需要一整页"当成 UART 的属性，
把 Core 已经派生成功的 lease 本地误判为 `lease_failed`。

**它是什么、不是什么**：

- 单次样本 = **触发写之前（`rdtime`）→ handler 入口（`rdtime`）** 的原始 tick；
  一次触发一个样本（`method=one_shot`），不是 batch；warmup 不计入。THR 占位写
  在 `rdtime` 之前（准备，不计入），区间从 `IER=THRE` 的触发写开始。
- 区间包含：触发写自身的 lease MMIO 写（KernelNative 快路径）、UART/PLIC 设备
  模型、CPU trap 入口、PLIC claim、Core `route`，以及 trace 打开时 `IrqEnter`
  的 emit 成本——**第一个事件的记录工作就在被测量区间内**。**不是**"中断投递延迟"。
- 已有 `IrqEnter` / `IrqAck` 事件**不能**当延迟用：`IrqEnter` 在 PLIC claim
  **之后**、`IrqAck` 在控制器 complete **之前**——那是"claim 后→complete 前"的
  插桩软件区间。本 primitive 不派生自它。
- 超时（handler 未到）与倒退（entry < start）的样本**丢弃并计数**；丢弃有硬上限，
  触发路径不可用 → `status=trigger_timeout`（绝不无界自旋）。设备被其它组件持有
  （如 core_test 先跑并认领 UART）→ `status=mmio_not_owned`；Core 拒绝 lease
  派生 → `status=lease_failed`（带 `error=`）；派生成功但 FDT 窗口不覆盖要碰的
  寄存器 → `status=mmio_window_too_small`（带 `lease_len=`）：缺的是"一台空闲的
  ns16550a"，不是去要 PLIC/god-mode。
- `baseline=none`：IRQ 触发没有等价 null baseline（制造一个 = 抑制触发/关中断，
  禁止），因此不做减法。`below_floor_samples` 的 floor = 一次读钟成本
  （`clock_median_read_delta`），低于它 = 量化主导，`resolution_limited=yes`。

## 4. 已实现的 primitive

| 名称 | 测什么 |
|---|---|
| `mmio.raw_volatile` | 裸 `read_volatile`（无验证）—— fast path 的上限 |
| `mmio.checked_read_u32` | 表锁 + slot/generation/owner/生命周期 + bounds + align + volatile |
| `mmio.lease_read_u32` | lease 派生后直访（fast path 收回了多少） |
| `handle.validate_ok` | 纯 handle 验证（成功路径） |
| `handle.reject_stale` | 失效 handle 的拒绝路径 |
| `mmio.derive_lease` | 一次性 lease 派生（control path） |
| `interface.direct_call` | 直接 Rust 调用（基线） |
| `interface.table_call` | 经 `#[repr(C)]` function table 调用（Interface 的 steady-state 成本） |
| `interface.bind` / `interface.refresh` / `interface.publish` | control path |
| `registry.bind.n1/n8/n32/n128` | Interface Registry 规模趋势（线性扫描是否成为问题） |
| `handle.get.n1/n32/n256` | handle 数量增长时的 hot path（验证成本） |
| `handle.revoke_regrant.n1/n32/n256` | handle 数量增长时的 control path（`revoke_owner` 的 O(N)；含一次重新 grant） |
| `task.lookup.n1/n32/n256` | task 数量增长（`BTreeMap` lookup） |
| `component.lookup.n1/n32/n256` | component 数量增长（`Vec` 线性扫描） |
| `alloc_free.order0..3` | buddy alloc/free 往返，按 order 分档 |
| `sched.pick_next` | `resolve_policy`（锁 + bind）+ 提议 + Core 验证 |
| `sched.task_transition` | 任务状态转换的验证 + 落笔（commit 成本） |
| `address_space.validate` | Core 语义 ledger 的纯验证（私有函数，仅 crate 内可测） |
| `address_space.map_unmap` | 完整路径：validate + backend + ledger commit + unmap |
| `loader.elf_parse` / `loader.relocations` | 解析 / 收集重定位表（不碰 VM） |
| `loader.load_component.min` / `.core_test` | 完整加载（parse + place + alloc + copy + relocate + 解析入口） |
| `kbench.clock_read` / `kbench.free_pages_query` | **目标端**：真实导出调用路径（host 测不出） |
| `sched.yield_roundtrip` | **目标端**：两个组件自有任务的完整 A→B→A 交接（policy + Core 验证 + commit + `__switch`；含 handshake 与 trace 独立验证，见 §3.5） |
| `irq.uart_trigger_to_handler` | **目标端**：owned ns16550a 自触发 → 组件 handler 入口（合法 authority 链；含超时/倒退丢弃，见 §3.6） |

## 5. 结果解读与诚实声明

### 5.1 必读：这些数字能回答什么

> 批量计时能解决"单次读钟粒度 + 计时开销占比"的问题，但不能把 QEMU TCG 变成
> 真实 CPU。相同环境下可以看相对趋势；真实上下文切换或 IRQ 延迟的绝对性能要在
> 真机测。五轮最小值相同可能只是落在同一个量化格子里，不代表误差接近零。

- `min` 是"观测到的最快 batch"，只是 **best-observed 估计**，不是精度证明。
  五轮最小值相同，也可能是五次都落在同一个量化格子；
- 量化可能向下取整；clock 行为、编译器效应、执行状态不是纯加性干扰，
  所以"最小值一定最接近真值"不成立；
- `baseline_paired_diff_median` 小或为负，说明**增量成本无法分辨** ——
  那是结果，不是需要修正的误差；
- 不关中断、不设"god-mode"、不发明 cycle 计数器测量、不设基于 TCG 最小值的
  CI 性能阈值。

### 5.2 host ≠ 目标端，QEMU ≠ 真机

- host 的 irq-guard 是 no-op（真机多约 2 条 CSR 指令），host 原子操作比 RV64
  本地原子贵；真实 MMIO 访问成本远在验证成本之上（QEMU 设备模型 ~µs，
  真机总线往返 ~几十~几百 ns）。
- QEMU TCG 不是 cycle-accurate，只能做功能验证与**同环境相对比较**，
  不能做延迟预测。
- 因此：**correctness → hard requirement；performance → 先只做信息性/趋势**。
  真机数据成熟之前不设 CI hard-fail 阈值。

### 5.3 时钟：`rdtime` 与 `cycle`（防 trap 备注）

- 本仓库当前只实现并导出 `rdtime`（`kcore_now`），报告里 `clock=rdtime`；
  这是唯一有真实测量路径的时钟。
- 将来若要读 `cycle`：**S-mode 读 `cycle` 受 `mcounteren.CY` 控制**
  （`scounteren.CY` 只管 U-mode 的 `cycle` 访问，管不到 S-mode）。
  不要在目标端用无保护的 `rdcycle` 探测 —— 可能直接 trap；需要时先由 Core
  检查/配置 `mcounteren`，再暴露受控的导出。

## 6. 待实现（roadmap，本阶段未做）

**只能在目标端做（host 无法测）**：
- **context switch**：端到端的 A→B→A 交接已由 `sched.yield_roundtrip` 覆盖
  （QEMU 上可跑；真机数据未做）。剩余未做：把 `resolve_policy`（锁 + bind）与
  提议/Core 验证拆成独立数字；SMP / 抢占下的切换成本。
- **IRQ latency**：两条路都已就位，但都**不是**纯投递延迟：
  1. `bench::collect_irq_latency()`（host 侧纯函数，host test 覆盖）从 trace 记录
     抽取 `IrqEnter -> IrqDispatch -> IrqAck` 三元组 —— 那是 **claim 后→complete
     前**的插桩软件区间；target 侧若要出数只缺"触发 + 调用"，**不得**当作投递
     延迟发布。
  2. kbench 的 `irq.uart_trigger_to_handler`：owned 设备自触发 → handler 入口的
     软件可观测区间（§3.6），合法 authority 链、丢弃规则、trace 成本披露齐备；
     未被 core_test 认领 UART 的独立启动下可跑。剩余未做：真机数据、无 trace
     构建下的对照、以及更接近硬件语义的投递测量（需要新的合法机制，不在本阶段）。
- **真实页表 backend**：`Sv32/Sv39` 的 map/unmap/translate 成本（host 只能测
  Core ledger 与 FakeBackend）。
- **真实 MMIO**：QEMU 设备模型 / 真机总线往返（host 用内存缓冲代替）。

**可以在 host 做、尚未做**：
- **`pick_next` 内部再分段**：目前是整体一个数（`resolve_policy` + 提议 + 验证），
  还没有把 "scheduler interface call" 与 "Core validation" 分成两个数字。
- **create / destroy 的 scaling**：当前 scaling 只测了 lookup 与 `revoke_owner`
  往返，`create` / `destroy` / proposal validation 随 N 的趋势还没测。
- **fragmentation**：allocator 的碎片化基准。

## 7. Filesystem / Storage benchmark —— 只记录，不实现

KaleidOS 目前**没有**稳定的 FileSystem / VFS / Page Cache 路径。现在照 IOzone
写测例，只会迫使架构围绕一个并不存在的 POSIX 模型演化，因此本阶段
**刻意不实现**，只预留分类（未来设计时参考 IOzone 的方法：不同 block size、
不同 working set、sequential/random、read/write/rewrite、cache 影响）：

```text
Filesystem / Storage
- sequential read / write
- rewrite
- random read / write
- metadata create / delete
- fsync-like persistence
- page-cache warm / cold
- BlockDevice throughput
- different transfer sizes
- different working-set sizes
```

## 8. 参考

| 工具 | 借鉴 | 不照搬 |
|---|---|---|
| lmbench | 把系统拆成基本 primitive 分别测延迟/带宽（context switch、syscall-like 边界、memory op、IPC-like op） | 它的 POSIX 测例集 |
| UnixBench | 多种基础 workload 重复运行，形成可对比 baseline，观察整体退化 | 意义不清的综合总分 |
| IOzone | 不同 block size / working set / 访问模式的矩阵式测量（**未来 FS 阶段**） | 现在实现（无 FS） |
