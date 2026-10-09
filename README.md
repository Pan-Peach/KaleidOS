# KaleidOS

组件化、多架构操作系统，面向学习、实验与个人创作。

> A small mechanism-first core beneath a composable graph of operating-system components.
> Build the machine, compose the system, run your own world.

KaleidOS 探索的是：用一个提供基础机制的小型 Core，支撑可组合的操作系统组件图。
调度策略、驱动、文件系统与系统语义都可以成为组件；同一个底座，通过不同的
组件组合与部署方式，形成不同的系统形态。POSIX 是其中一种可选方向。

## 核心特色

> **Core owns truth. Components own policy and semantics.**
> **Policy proposes, Core validates and commits.**

- **少即是多，能力默认外置。** Core 管理资源的存在性、状态、所有权与生命周期。
  RR 调度算法、VirtIO 协议、文件系统格式与 POSIX 语义由组件实现，
  Core 提供支撑它们所需的机制。
- **策略可以替换，资源真相由 Core 裁决。** 调度器提出“下一步运行哪个任务”，
  Core 验证任务状态、owner 与 CPU 归属后才提交，并记录结构化 trace。
  这让实验不同策略时仍有明确的不变式与可观察的提交过程。
- **部署形态本身就是安全策略。** KernelNative 与 Core 同特权、同地址空间，
  适用于受信组件；IsolatedNative 使用私有地址空间。驱动向 Core 认领设备后，
  在本执行域的访问窗口内操作硬件。访问边界取决于真实执行域及硬件能力。
- **服务语义与调用机制分开。** 组件通过 Endpoint 发布和绑定服务；接口描述
  “提供什么”；新普通服务使用 Endpoint Request/Reply，Core 复验授权与执行域。
  历史 Direct/Gate 尚有消费者，收敛状态见 STATUS §3.29。
  同一份组件业务代码与服务契约可以用于不同部署组合。
- **组件是独立程序，也有独立实例。** `.kcomp` 使用窄 C ABI，支持 Rust 与
  freestanding C 前端；加载同一工件两次得到两个组件，各自拥有可写镜像状态、
  资源与生命周期。SDK 和第三方库随组件私有携带。
- **Core 与机器发现解耦。** Arch 提供 ISA 原语，boot 将机器描述归一化为
  `MachineInfo`，再交给 Core 验证与初始化。纯资源逻辑可在 host 测试，
  CPU、页表和中断的真实行为通过 QEMU 与硬件验证。

设计原则与边界见 [核心哲学](docs/philosophy/core-philosophy.md)、
[部署契约](docs/architecture/deployment.md) 和 [驱动契约](docs/architecture/driver-model.md)。

## 架构与组合

**OS = Resource Core + Component Graph + Profile。**

```text
Applications / System Personality
              │
        Services / Devices
              │
     Components（策略 / 服务 / 驱动）
              │
         Resource Core
              │
    Arch / Machine Discovery
              │
           Hardware
```

bootstrap 与 Core 职责分离，链接成一个内核镜像；组件独立构建为 `.kcomp`
（ELF 可重定位程序），打包为 `init.kpkg`（cpio + manifest），由组件管理机制装载。
启动组件负责组合调度器、驱动与服务。Cargo 依赖描述编译关系，运行时组件图由
组件管理与启动编排建立。

## 现在可以运行什么

当前组件系统的主要开发与 QEMU 测试路径是 RV64/Sv39 与 RV32/Sv32。
以下链路已经接通，具体支持范围与缺口以 [STATUS.md](STATUS.md) 和模块文档为准。

| 能力 | 已接通的链路 |
|---|---|
| 启动与交互 | 机器发现 → Core → init → 驱动 / FAT 根盘 → ksh；monitor 支持观察机器、任务与组件，以及手工 load/unload |
| 真实驱动与文件系统组件 | driver_prober 发现设备，virtio_blk 提供块服务，FatFs / littlefs 通过组件接口提供文件系统服务 |
| 组件执行与调度 | scheduler_rr 提供策略，Core 验证并切换任务；RV64 支持固定 CPU 的协作式 SMP 与跨 CPU park/unpark |
| 部署与服务绑定 | 受限的 IsolatedNative 私有地址空间、私有堆与 K/K、K/I、I/K、I/I 服务调用组合 |
| 组件失败处理 | 独立栈上的协作式 panic containment、实例逻辑失效与新实例重启；已发布 backing 保守驻留 |
| 普通用户程序 | RV64 静态 ELF 的 U-mode 执行，最小 POSIX fork/exec/wait，以及 FAT → ksh exec 流程 |

IsolatedNative 当前仍是受限、协作式的执行边界；KernelNative panic containment
只保证相应逻辑失效。SandboxedNative 组件后端、通用 VFS 文件 fd/libc 启动与完整物理回收
仍待接通。各 ISA 的能力见 [Arch 模块](docs/modules/arch.md) 与
[boot 模块](docs/modules/boot.md)。

## 启动

先按 [构建指南](docs/development/building.md) 准备 Rust、QEMU 与镜像工具，
然后在仓库根目录运行：

```sh
git submodule update --init --recursive
make qemu_rv64_defconfig
make qemu
```

默认启动 `init`，组合驱动与 FAT 根盘后进入 `ksh`。输入 `help` 查看命令，
`cat 0:/HELLO.TXT` 读取示例文件，`exit` 回到 Core Monitor。
QEMU 用 `Ctrl-A`、`X` 退出。切换 RV32、进入 monitor 与配置说明见构建指南。

## 开发与测试

| 命令 | 用途 |
|---|---|
| `make check` | 格式、lint、ABI/Kconfig 检查、host 测试与交叉构建 |
| `make test-host` | 宿主逻辑测试 |
| `make test-qemu` | RV64/RV32 CoreTest 与 init/ksh 用户流程 |
| `make test-arch` | RV64/RV32 硬件白盒测试，含 RV64 SMP |
| `make test` | 上述三条测试通道，与默认 CI 门禁一致 |

CoreTest 是组件与系统集成测试的统一编排者，以普通组件身份调用真实 Core API。
纯逻辑在 host 验证，寄存器、页表与中断等硬件契约由 ArchTest 验证。
测试边界、SMP 门禁与新增用例方法见 [测试指南](docs/development/testing.md)。

## 阅读与贡献

| 入口 | 内容 |
|---|---|
| [docs/README.md](docs/README.md) | 按任务找文档，以及契约的权威归属 |
| [核心哲学](docs/philosophy/core-philosophy.md) / [架构总览](docs/architecture/overview.md) | Core 与组件的分工 |
| [模块地图](docs/modules/README.md) | 源码位置与模块职责 |
| [STATUS.md](STATUS.md) | 当前能力、缺口与路线图 |
| [AGENTS.md](AGENTS.md) | Agent 工作约定与必须遵守的边界 |

OS 源码位于 `os/`；测试组件位于 `os/components/tests/`；外部依赖位于
`third_party/`，使用 git submodule。文档组织规则见
[文档指南](docs/development/docs-guide.md)。
