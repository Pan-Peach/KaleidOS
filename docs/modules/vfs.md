# vfs（os/components/filesystems/vfs/）

> 现状描述。职责契约见 `docs/interfaces/filesystem.md`；组件生命周期见
> `docs/architecture/component-lifecycle.md`。本文不冻结 VFS API 或 ABI。

## 当前状态

只有 Rust `.kcomp` 骨架。已接入组件构建、fmt 与 clippy；create / destroy 返回
`-ENOTSUP`，内部操作入口返回 `Error::Unsupported`。有表槽位的借用形状，但没有
实际表管理、endpoint、任务或 I/O。`load vfs` 会创建失败，不提供可用服务。

源码里的 Rust 类型与签名是内部草案，不跨组件边界。第一阶段 ABI 声明已放在
`abi/vfs.toml`，生成 C / Rust 布局；逐方法编码见 `docs/interfaces/vfs.md`。
SDK `vfs.rs` 提供 Contract / provider trait / Binding 占位；bind 与操作返回 ENOTSUP，
尚无 Direct/Gate adapter。

## 职责与代码

| 文件 | 职责 / 当前落点 |
|---|---|
| `src/lib.rs` | 模块入口；内部错误草案，保留权限拒绝、共享冲突、delete-pending 的区别 |
| `src/provider.rs` | FS incarnation、节点元数据 / 原生权限；root / lookup / 引用 / readlink / 枚举 / 流 / read_at / cleanup / close 占位 |
| `src/name.rs` | Bytes / UTF-16 表示、调用者缓冲区、目录匹配规则提示 |
| `src/stream.rs` | 默认 / 命名流选择、StreamRef、流大小快照；无物理 extent |
| `src/namespace.rs` | mount 槽位、路径位置 / 遍历限制；attach / detach / retain / release / resolve / 失效占位 |
| `src/file.rs` | open / stream / delete 槽位、用户 / I/O / mapping 引用、share 计数与删除状态；操作占位 |
| `src/runtime.rs` | 生命周期入口；实例创建与清理的待实现位置 |

目标是由 VFS 实例保存 mount、目录项、打开引用与共享访问状态。Core 管组件、
任务和执行域生命周期；personality 管 fd / HANDLE、cwd、进程语义和错误表示。
Namespace 与 File service 先保持在同一组件的普通模块中。

## 状态形状与关系

| 记录 | 保存什么 | 不代替什么 |
|---|---|---|
| NodeInfo | provider 的 kind / link_count / 目录名字规则 / 原生权限快照 | 物理块映射、VFS 打开引用 |
| StreamInfo | node 范围内的流 token、逻辑大小和可选原生大小属性 | ExtentList、每 inode 的强制 ADS map |
| PathRef | mount / entry / node 的路径位置；引用必须显式 retain / release | 单独 node ID、打开实例 |
| OpenFile | stream、provider handle、游标、access/share、引用与生命周期 | fd / HANDLE、硬链接计数 |
| StreamState | 同一流的独立 open share 计数 | 用户复制句柄数 |
| PendingDelete | 删除目录项或命名流的目标、时机、请求者与状态 | 普通 inode 上一个 DELETE_PENDING 位 |

| 场景 | 待实现的状态转换 / 协调 |
|---|---|
| 独立 open | 新 OpenFile、新游标；双向 share 检查与提交不能分开 |
| retain / dup | 增加同一 OpenFile 的 handles；不增加 share.opens / link_count |
| 最后用户引用关闭 | Live → Handleless；已有请求处理后 cleanup，再进入 Cleaned |
| mapping / I/O 排空 | 最终 provider close / 对象退役；不承诺物理 backing 回收 |
| 删除（未来写支持） | 区分名字移除和延迟删除；文件删除检查所有流上的 share |
| provider 失败 | 引用逻辑失效；OpenState / DeleteState 记录失败，不重绑到新 incarnation |

这些枚举只是未实现的内部状态位置。删除计数范围、取消 / commit 原子性、权限检查
与跨 personality 冲突仍未定稿；当前 readonly ABI 不暴露删除操作。

## 手写实现顺序

1. 定下 FS 实例、节点、目录项与打开引用的有效期和失效规则。身份要区别
   provider 重启 / 重新挂载；多个路径入口共享同一底层对象。
2. 补齐 provider 契约所需的 root / lookup / node_info / 引用 / readlink / 枚举与偏移读取。
   当前 `abi/filesystem.toml` 已补 root / 单段 lookup / node_info，FatFs 与 C/Rust SDK
   已接线；littlefs 对节点操作返回 ENOTSUP。节点保活、readlink、枚举与 read_at
   尚未实现，VFS provider 适配仍为占位。
3. 实现 per-instance 表与遍历；名字编码、匹配和 symlink / mount 边界须显式定义。
4. 实现只读打开与读取；验证独立 open 的游标、复制引用的共享状态、短读 / EOF、
   provider 失败后旧引用失效。共享状态和锁的覆盖范围必须明确。
5. 实现 SDK 第一阶段 ABI 适配器，定下组合配置、补构造失败清理，再发布 endpoint；集成编排放到
   `os/components/tests/core_test/`，只走真实 SDK / Core API。

Gate 服务回调当前禁止 park / 调度切换；unpark 只允许同 owner。需要等待磁盘或
跨组件完成通知时，先定等待与完成机制，不能在 service stack 中直接 park。

写支持后再补原子追加、排他创建、rename/unlink、共享访问 / delete-pending 与
落盘保证。权限模型、错误 wire 编码、跨 personality 冲突规则仍需人类定稿。
缓存、异步 I/O 与完整 NT 对象 namespace 留到真实需求出现后。

## 骨架验证

在仓库根运行；显式 target 仅用于验证两个现有 RISC-V 后端，正式镜像仍由
`.config` 经 Makefile 选择 target。

```sh
cargo fmt --manifest-path os/components/filesystems/vfs/Cargo.toml -- --check
cargo clippy --manifest-path os/components/filesystems/vfs/Cargo.toml --target riscv64gc-unknown-none-elf -- -D warnings
tools/build-kcomp.sh os/components/filesystems/vfs riscv64gc-unknown-none-elf build/vfs-rv64/vfs.kcomp build/vfs-rv64/target
tools/build-kcomp.sh os/components/filesystems/vfs riscv32imac-unknown-none-elf build/vfs-rv32/vfs.kcomp build/vfs-rv32/target
```

packer 验证 ELF / 生命周期符号 / import / 重定位；这些检查不证明 VFS 行为。
