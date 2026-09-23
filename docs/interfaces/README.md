# 接口契约（interfaces/）

> 本目录存放**组件 / 驱动 / 服务对外暴露的契约**：ABI 形状、语义、错误约定、生命周期义务。
> 与 `architecture/` 的区别：`architecture/` 讲**系统怎么搭**；`interfaces/` 讲**一个接口长什么样、调用者该期待什么**。

## 当前

| 文档 | 内容 | 状态 |
|---|---|---|
| `filesystem.md` | 文件系统抽象边界契约（分层 / 职责划分 / open state 归属 / 原子性 / capability / 缓存单位） | 设计契约已成文；§13.2 的未决问题待人类定稿 |

## 相关契约现居何处

interfaces/ 尚未成体系前，多数对外契约暂时写在架构文档里，且各自有单一权威：

| 契约 | 权威位置 |
|---|---|
| **生成式 ABI 声明**（`kcore_*` / 组件入口 / 边界 struct / `Errno` / function table 的 C 与 Rust 镜像） | **唯一来源**是 schema `abi/*.toml`；生成器 `tools/kabi/kabi_gen.py`；产物清单见 `modules/core/generated.md`。**改 ABI = 改 schema + `make abi-gen`**，勿手改生成物 |
| Core Export ABI（`kcore_*` 白名单、返回值形状、错误码） | `architecture/overview.md` §5；组件 ABI 根见 `architecture/component-lifecycle.md` §4 |
| 组件入口 `kcomp_instance_create` / `kcomp_instance_destroy` | `architecture/component-lifecycle.md` §4 |
| Interface（Device / Service / Policy）与 binding | `architecture/component-model.md` §2 |
| 设备语义侧：device claim / IRQ / DMA 的 API 形状与 errno | `architecture/driver-model.md` §6 |
| 组件侧 SDK（Rust adapter / C 头 / C 运行时）与第三方库移植层（`kport`） | `modules/components.md`；移植契约见 `architecture/porting.md` |

> **不要**在本目录重复上面已有的内容；新接口文档应引用它们，而不是复制一份会漂移的副本。
