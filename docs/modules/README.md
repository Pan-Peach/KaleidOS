# 模块地图（modules/README.md）

> 本目录回答一个问题：**`os/` 下每个模块在干嘛**——它 owns 什么真相、暴露什么机制、明确不做什么、代码在哪。
> 这是**现状描述**，不是设计契约：与 `docs/architecture/` 冲突时以架构文档为准；未实现的目标会显式标注"目标 / 未实现"。

## 怎么读

每个模块页回答四件事：

1. **owns 什么真相** —— 它独占、且撒谎就会破坏全局不变式的状态。
2. **暴露什么机制** —— 关键公开类型 / 函数（不逐条抄签名）。
3. **明确不做** —— 边界；避免把职责想大。
4. **代码在哪** —— 真实文件路径。

## Core 模块（`os/core/src/`）

| 模块 | 一句话 | 页面 |
|---|---|---|
| `lib` | Resource Core library 入口：`init()` 装配顺序 + 公共模块 + 宏 | [`core/lib.md`](core/lib.md) |
| `task` | 任务身份 / 状态 / owner / 内核栈 / 上下文；不含调度算法 | [`core/task.md`](core/task.md) |
| `sched` | 每 CPU 调度真相 + propose→validate→commit→switch 路径 | [`core/sched.md`](core/sched.md) |
| `memory` | canonical 物理内存机制（buddy）+ Core 堆 + 区域 lease + 地址空间词汇 | [`core/memory.md`](core/memory.md) |
| `resource` | 设备 / IRQ / DMA 的**归属记账**（device/irq/dma/context） | [`core/resource.md`](core/resource.md) |
| `irq` | 外部中断投递入口 + 关中断临界区原语 | [`core/irq.md`](core/irq.md) |
| `timer` | timer 机制状态（ticks / deadline / preempt 初始化） | [`core/timer.md`](core/timer.md) |
| `component` | 组件身份与生命周期 / 已加载程序 / 接口绑定 / 导出 ABI / 加载 / containment | [`core/component.md`](core/component.md) |
| `machine` | 已提交的 `MachineInfo` + 纯设备发现 | [`core/machine.md`](core/machine.md) |

## 观察与诊断（`os/core/src/`）

| 模块 | 一句话 | 页面 |
|---|---|---|
| `trace` | 结构化事件环：`seq` / 掩码 / 固定容量 / 统计 | [`core/trace.md`](core/trace.md) |
| `print` | 核心日志格式化；传输交给 arch `Console` | [`core/print.md`](core/print.md) |
| `monitor` | Core Monitor 交互 shell（`core>`）：命令、行编辑 | [`core/monitor.md`](core/monitor.md) |

## 度量、配置与 ABI

| 模块 | 一句话 | 页面 |
|---|---|---|
| `bench` | host/目标端 benchmark harness + IRQ 延迟抽取 | [`core/bench.md`](core/bench.md) |
| `errno` | 内部错误 → ABI `Errno` 的**唯一翻译点** | [`core/errno.md`](core/errno.md) |
| `build_config` | `build.rs` ↔ Kconfig 的 `TRACE_CAPACITY` 传输契约（test-only） | [`core/build_config.md`](core/build_config.md) |
| `generated` | 由 `abi/*.toml` 生成的 ABI 形状 / `Errno` 枚举 / 导出表 | [`core/generated.md`](core/generated.md) |

## Core 之下的 `arch`、`boot`、组件层

| 模块 | 一句话 | 页面 |
|---|---|---|
| `os/arch` | ISA / firmware backend：`CpuArch` / `Timer` / `InterruptController` / `Console` / `SystemReset` + Sv39/Sv32/NoMMU + 重定位 | [`arch.md`](arch.md) |
| `os/boot` | `_start` → FDT discovery → `MachineInfo` → `core::init` → monitor；镜像布局与启动页表 | [`boot.md`](boot.md) |
| `os/components` | 组件 crates + `kcomp-sdk` + `.kcomp` 构建/打包/加载流水线 | [`components.md`](components.md) |

> 接口契约（ABI / 设备 / 文件系统语义）在 `docs/interfaces/`；此处只描述模块边界与代码位置。
