# 文档指南（docs-guide.md）

> 本文件说明 `docs/` 的组织方式，以及**新文档应该放到哪里**。目录职责与权威归属见 `docs/README.md`。

## 1. 选位置：按"这份文档回答什么问题"

```text
为什么这样设计 / 什么进 Core / 判断标准        → docs/philosophy/
系统是什么 / 分层 / 契约 / 边界                → docs/architecture/
某个接口对外长什么样 / ABI / 设备语义          → docs/interfaces/
某个模块在干嘛 / 真相 / 机制 / 代码位置         → docs/modules/
怎么构建 / 测试 / 量性能 / 排里程碑            → docs/development/
历史推理 / 归档调研（勿据以实现）              → docs/notes/
```

判断口诀：**规则放 philosophy，结构放 architecture，契约放 interfaces，现状放 modules，操作放 development，过期放 notes。**

几条边界：

- **`modules/` 写现状，不写设计愿景。** 模块页回答四件事：owns 什么真相 / 暴露什么机制 / 明确不做什么 / 代码在哪。未实现的目标要显式写"目标 / 未实现"，不能当现状写。若不确定某模块行为，去读代码，不要臆造。
- **同一事实只留一处真相。** 不要在这里复制 `architecture/` 的契约；需要时链接过去。例如执行域细节只在 `architecture/driver-model.md`，模块页只说"见 driver-model §2"。
- **历史不进权威目录。** 被取代的设计、废弃模型的审查记录一律进 `notes/`，并在文件顶部标注 HISTORICAL 与现行替代文档。
- **`interfaces/` 不重复已有契约。** 新的接口文档应引用现有权威，不复制会漂移的副本。

## 2. 文件命名

- 用小写 kebab-case（`component-lifecycle.md`、`docs-guide.md`）。
- **禁止版本后缀**（`V1` / `_v2` / `-v2`）：契约变了原地替换，不留旧名（`AGENTS.md` 原则）。
- Core 模块页放在 `docs/modules/core/<module>.md`，文件名与 `os/core/src/` 下的模块目录 / 文件同名。
- 架构总览固定叫 `architecture/overview.md`（它解释"整个系统是什么"），不叫 `architecture/architecture.md`。

## 3. 写作风格

沿用本仓库既有中文技术文档风格：

- 中文为主，术语保留英文原词（Core / Component / Interface / owner / quarantine ...）。
- 密集、直接、不注水；不用 emoji。
- 关键原则用 `>` blockquote 突出。
- 对比、枚举、状态用表格 / fenced code block（`text` / `rust` / `c`）。
- 引用其他文档时写**完整新路径**（`docs/architecture/component-lifecycle.md`），不要写裸文件名或旧路径。
- 引用章节用 `§N`（如 `driver-model.md` §6）。

## 4. 新增一份文档的步骤

1. 按 §1 选目录与文件名。
2. 写清它的**权威性**（是契约？是现状描述？是历史？）。若是权威契约，在 `docs/README.md` §3 的权威表里登记一行。
3. 在 `docs/README.md` 的目录树与"我该看哪份"表里补入口。
4. 若它取代旧文档：把旧文档移进 `notes/` 并在顶部标注 HISTORICAL，指向新文档；**不要静默删除**。
5. 若移动 / 重命名了现有文档：按 §5 更新全仓库引用。

## 5. 移动 / 重命名文档 = 必须更新引用（强制）

移动文档会**静默打断**源码注释里的契约指针，必须同步更新。改动范围（仓库内全量）：

```text
Rust 文档注释：src/**/*.rs 的 //! / /// 中出现 docs/... 的行
根文档：README.md、AGENTS.md
其他文档：docs/**/*.md
构建：Makefile、scripts/**、tools/**
CI：.github/workflows/**
```

验证（应无输出；`docs/interfaces/filesystem.md` 由其他工作流负责，见下）：

```sh
grep -rnE "docs/[a-z-]+\.md" \
  --include="*.rs" --include="*.md" --include="*.toml" --include="*.py" \
  --include="*.sh" --include="*.yml" --include="*.yaml" --include="Makefile" . \
  | grep -vE "^\./(target|\.git|third_party|build)/"
```

对每一条命中确认目标文件真实存在。已知例外：`os/core/src/memory/mod.rs` 里对
`docs/09_debug/buddy-allocator-scan-drift.md` 的引用是 **MangoCore 外部文档**，不在本仓库。

## 6. 已知跨工作流例外

`docs/interfaces/filesystem.md` 由另一条工作流维护，其内部指向旧路径的引用由该工作流更新；本仓库文档重构不触碰该文件（见 `docs/README.md` §5 与 `notes/` 说明）。
