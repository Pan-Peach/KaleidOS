# KaleidOS 文档索引（docs/README.md）

> 本文件是 `docs/` 的**入口与权威路由表**。任何"该看哪份文档""两份文档冲突时谁赢"的问题，先看这里。
> 阅读顺序建议：先哲学（为什么）→ 架构（是什么）→ 模块（每个模块在干嘛）→ 开发（怎么干活）。

## 1. 目录结构

```text
docs/
├── README.md                  ← 本文件：索引 + 权威归属
├── philosophy/                为什么这么设计：稳定原则、判断标准、参考来源
│   ├── core-philosophy.md
│   └── references.md
├── architecture/              系统是什么：分层、Core、组件、驱动、配置的契约
│   ├── overview.md
│   ├── component-model.md
│   ├── component-lifecycle.md   （已冻结的组件生命周期契约）
│   ├── driver-model.md
│   ├── kconfig.md
│   └── porting.md               （第三方库移植 / 调包能力：设计契约 + 候选地图）
├── interfaces/                组件/驱动对外契约（ABI、设备语义、文件系统语义）
│   └── filesystem.md            （文件系统抽象边界契约；未决问题待人类定稿）
├── modules/                    每个模块在干嘛：真相 / 机制 / 不做什么 / 代码在哪
│   ├── README.md               （模块地图）
│   ├── core/<module>.md
│   ├── arch.md
│   ├── boot.md
│   └── components.md
├── development/               怎么在上面干活
│   ├── testing.md
│   ├── benchmark.md
│   ├── roadmap.md
│   └── docs-guide.md
└── notes/                     历史与归档：非权威，不代表现状
    ├── resource-model-review.md
    └── arch-context.md
```

## 2. 各部分职责

| 部分 | 回答的问题 | 权威性 |
|---|---|---|
| `philosophy/` | **为什么**这样设计；什么进 Core、什么不进 | 稳定原则。代码可以重写，哲学不要漂移 |
| `architecture/` | 系统**是什么**：分层、契约、边界 | 设计契约。实现与之冲突 = 实现错，或显式改文档 |
| `interfaces/` | 组件/驱动**对外暴露什么** ABI 与语义 | 契约。跨组件兼容性的唯一依据 |
| `modules/` | 每个模块**在干嘛**（`os/` 下逐模块） | 描述性：写"代码事实在哪、边界是什么"，**不发明行为** |
| `development/` | **怎么**构建、测试、量性能、排里程碑 | 操作指南。与代码同步的最新事实 |
| `notes/` | 历史推理、归档调研 | **非权威**，明确标注 HISTORICAL/归档，勿据以实现 |

## 3. 权威归属（冲突时谁赢）

同一件事只应有一处真相。冲突时按下表判定，而不是"哪份更新"：

| 主题 | 权威文档 | 说明 |
|---|---|---|
| 状态归属 / 什么进 Core | `philosophy/core-philosophy.md` | 判断标准与不变式 |
| 组件生命周期、组件 ABI、image/instance 拆分 | `architecture/component-lifecycle.md` | **已冻结**；与 `component-model.md` 冲突以它为准 |
| 驱动、device claim、IRQ/DMA、执行域、teardown | `architecture/driver-model.md` | 驱动与执行域细节最终契约 |
| 分层总览、ResourceDomain / ExecutionDomain 概念 | `architecture/overview.md` | 与 driver-model 细节冲突时以 driver-model 为准 |
| 构建配置（Kconfig / `.config`） | `architecture/kconfig.md` | 唯一配置真相的来源 |
| 第三方库移植 / 调包能力（kport、候选库、统一 host 接口） | `architecture/porting.md` | 设计契约 + 候选地图；候选不等于已集成 |
| 组件对外契约（设备/FS/服务） | `interfaces/` | 接口语义契约；文件系统抽象见 `interfaces/filesystem.md` |
| 每个模块的真实行为与代码位置 | `modules/` | 描述现状；不确定就写"未实现/目标"，不臆造 |
| 测试策略 | `development/testing.md` | —— |
| 性能基准 | `development/benchmark.md` | —— |
| 里程碑与进度 | `development/roadmap.md` | 现状快照会随时间变化 |
| 参考系统借鉴 | `philosophy/references.md` | 设计阅读清单 |

> **`notes/` 不参与权威判定。** 其中 `resource-model-review.md` 是对**已删除的**旧 Handle/Slot/authority 模型的审查记录，其中所有 `Handle` / `Lease` / `authority` 词汇**均属废弃模型**；`arch-context.md` 是归档的开源调研。需要理解现状请走 `architecture/`。

## 4. 我该看哪份？

| 我想…… | 看 |
|---|---|
| 理解项目为什么长这样 | `philosophy/core-philosophy.md` + `architecture/overview.md` |
| 知道某个 Core 模块在干嘛、代码在哪 | `modules/README.md` → `modules/core/<module>.md` |
| 写一个驱动 / 认领设备 | `architecture/driver-model.md` + `modules/components.md` |
| 加载 / 停止一个组件 | `architecture/component-lifecycle.md` + `modules/core/component.md` |
| 把第三方成熟库（FS / net / TLS / runtime）接成组件 | `architecture/porting.md` |
| 新加一个构建开关 | `architecture/kconfig.md` |
| 加测试 | `development/testing.md` |
| 加文档 | `development/docs-guide.md` |
| 找参考论文 / OS | `philosophy/references.md` |

## 5. 维护约定

- 新增文档先读 `development/docs-guide.md`，决定它属于哪一部分、权威性如何。
- 移动 / 重命名文档时，**必须**同步更新仓库内所有引用（源码 `//!` / `///` 注释、`README.md`、`AGENTS.md`、`Makefile`、`scripts/`、`tools/`、CI）。引用断裂 = 契约指针失效。
- 版本后缀（`V1` / `_v2`）不用于文档命名，契约演进靠原地替换（见 `AGENTS.md`）。
