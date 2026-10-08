# KaleidOS 文档索引

从任务入口开始阅读。首次了解项目可按核心哲学 → 架构总览 → 模块地图的顺序；
构建与测试直接查开发指南。当前进度统一见 [STATUS.md](../STATUS.md)。

## 1. 目录结构

```text
docs/
├── philosophy/    稳定原则、判断标准与参考资料
├── architecture/  分层、资源、组件、部署与配置契约
├── interfaces/    组件对外 ABI 与语义契约
├── modules/       代码事实、模块职责与源码位置
├── development/   构建、测试、性能、文档维护与改进建议
└── notes/         历史与归档，不作为现行契约
```

## 2. 各部分职责

| 部分 | 回答的问题 | 权威性 |
|---|---|---|
| philosophy | 为什么这样设计、什么进 Core | 稳定原则 |
| architecture | 系统是什么、分层与边界如何约束实现 | 设计契约；改变契约需显式修改文档 |
| interfaces | 组件对外提供什么 ABI 与语义 | 接口契约；布局/数值以 ABI schema 为准 |
| modules | 模块做什么、代码在哪里、哪些能力尚未接通 | 描述现状，不发明行为 |
| development | 如何构建、验证、维护；哪些整理建议待实施 | 操作指南或明确标注的建议 |
| notes | 过去怎样推理、哪些方案已被替代 | 非权威；归档页标明现行替代文档 |

## 3. 权威归属（冲突时谁赢）

同一主题只保留一个权威来源，按主题判定，不以更新时间判定。
实现与契约不一致时应修正实现，或显式调整契约；实现进度不从设计愿景推断。

| 主题 | 权威来源 | 冲突规则/说明 |
|---|---|---|
| 状态归属、什么进 Core | [核心哲学](philosophy/core-philosophy.md) | 判断标准与不变式 |
| 组件身份、生命周期与组件 ABI | [组件生命周期](architecture/component-lifecycle.md) | 冻结契约；优先于 component-model |
| 设备认领、IRQ/DMA、执行域与 teardown | [驱动契约](architecture/driver-model.md) | 相关细节优先于 overview |
| Memory/Heap、region 归属、访问窗口与回收 | [内存与堆](architecture/memory-and-heap.md) | 内存/堆最终契约；与 device claim 同形 |
| 部署、binding 作用域与调用机制 | [部署契约](architecture/deployment.md) | 补充 lifecycle §9 的代码共享目标，不取代生命周期承诺 |
| 分层与 ResourceDomain/ExecutionDomain 概念 | [架构总览](architecture/overview.md) / [组件模型](architecture/component-model.md) | 细节服从上述主题契约 |
| 调度、CPU 归属与 SMP 提交 | [调度契约](architecture/scheduling.md) | Core 与策略分工 |
| 构建配置 | [Kconfig 契约](architecture/kconfig.md) | resolved .config 是本次构建真相 |
| 第三方库移植、adapter 与 SDK glue | [移植契约](architecture/porting.md) | 候选不等于已集成 |
| 设备/文件系统/服务对外语义 | [接口索引](interfaces/README.md) / [文件系统](interfaces/filesystem.md) | 接口语义契约 |
| VFS wire 与生命周期 | [VFS](interfaces/vfs.md) + [abi/vfs.toml](../abi/vfs.toml) | 布局/数值以 schema 为准；实现状态见模块页 |
| 网络服务、wire 与等待 | [网络](interfaces/network.md) + [abi/network.toml](../abi/network.toml) | 调用方代理与 provider 状态分开 |
| 模块真实行为与代码位置 | [模块地图](modules/README.md) | 查源码与模块页，不从目标推断能力 |
| 测试职责与运行方法 | [测试指南](development/testing.md) | 构建步骤见 [building](development/building.md) |
| 性能验证 | [benchmark](development/benchmark.md) | 基准与回归策略 |
| 现状、里程碑与路线图 | [STATUS.md](../STATUS.md) | 进度的单一入口；事实以代码和验证证据为准 |
| 参考系统与论文 | [参考资料](philosophy/references.md) | 阅读清单 |

`notes/` 不参与权威判定。旧 resource-model-review 中的 Handle/Slot/authority 模型
已被替代；需要现行资源与生命周期契约时查 architecture。

## 4. 我该看哪份？

| 任务 | 文档 |
|---|---|
| 准备工具、构建、启动、切换配置 | [构建指南](development/building.md) |
| 运行测试、决定用例归属 | [测试指南](development/testing.md) |
| 了解配置/测试 cleanup 调研与待实施清单 | [cleanup](development/cleanup.md)（建议，非架构契约） |
| Core 收敛职责审计、变更与验证证据 | [Core convergence](development/core-convergence.md)（审计记录，非契约） |
| 理解项目与寻找源码 | [核心哲学](philosophy/core-philosophy.md)、[架构总览](architecture/overview.md)、[模块地图](modules/README.md) |
| 理解 scheduler 与 SMP | [调度契约](architecture/scheduling.md)、[sched 模块](modules/core/sched.md) |
| 写驱动、认领设备 | [驱动契约](architecture/driver-model.md)、[组件地图](modules/components.md) |
| 获取内存、选择部署与调用机制 | [内存与堆](architecture/memory-and-heap.md)、[部署契约](architecture/deployment.md) |
| 装载/停止组件 | [组件生命周期](architecture/component-lifecycle.md)、[component 模块](modules/core/component.md) |
| 组合启动与操作 shell | [init](modules/init.md)、[ksh](modules/ksh.md) |
| 接第三方库 | [移植契约](architecture/porting.md) |
| 写文件系统与 VFS | [文件系统接口](interfaces/filesystem.md)、[VFS 模块](modules/vfs.md) |
| 写网络服务 | [网络接口](interfaces/network.md)、[netstack 模块](modules/netstack.md) |
| 写 POSIX 与用户程序 | [POSIX 模块](modules/posix.md)、[用户 ELF 指南](development/userspace.md) |
| 跑 libc-test 宿主兼容性参考 | [compat-testing](development/compat-testing.md) |
| 加构建开关、文档、性能基准 | [Kconfig](architecture/kconfig.md)、[文档指南](development/docs-guide.md)、[benchmark](development/benchmark.md) |

## 5. 维护约定

- 新文档先按问题选择位置，注明是契约、现状、操作指南还是建议，再补本索引入口。
- README 负责上手与导航，AGENTS 保留稳定禁错规则，STATUS 负责进度；契约只在对应主题页维护。
- 移动或重命名文档时，同步更新全仓库引用，包括源码注释、构建脚本与 CI。
- 契约演进原地替换，不使用版本后缀；过期推理归档到 notes 并链接现行契约。
