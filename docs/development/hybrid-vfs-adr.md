# ADR：一个 VFS 的 Local / Remote 后端

> 状态：**KernelNative 混合 VFS 已实现**。LocalFs、Remote FatFs、独立 VFS Server Task、SDK 与 ksh/exec 已接线；Block 与 littlefs 的统一 IPC、私有域 IPC 尚未实现。现行权威是 [filesystem](../interfaces/filesystem.md)、[VFS wire](../interfaces/vfs.md)、[模块现状](../modules/vfs.md)；本 ADR 记录决策，验证见 STATUS。前置依据见 [参考系统](reference-systems.md)、[源码审计](component-communication-audit.md) 与 [IPC ADR](ipc-request-reply-adr.md)。

## 1. 决策

保留独立 VFS Component，其内部只有一套 Namespace、Path 与 OpenFile。Local Rust FS 与 VFS 静态组合，使用普通对象调用；RemoteFs/RemoteNode 用 IPC 将 Provider 私有对象转为相同内部语义。FatFs/littlefs 优先独立 `.kcomp`，不在 Core 或 VFS 中嵌入这些 C 库。Local/Remote 是部署选择，不能分裂成两套 namespace 或 consumer API。

```text
POSIX / Win32 / WASI / ksh
             │  VFS 协议
        VFS Component
     Namespace → OpenFile
             │ Rust 对象接口
       ┌─────┴─────┐
  Local 只读 FS   RemoteFs / RemoteNode
       │              │ Endpoint Request/Reply
  Rust 内存对象    fatfs.kcomp / littlefs.kcomp
                         │ Block 协议
                    BlockDevice Component
```

参考 Linux 的 inode/dentry/path/file 区分与 DragonOS 的 Arc/Weak 对象组合；拒绝复制它们的完整 cache、巨大 trait、mount propagation/rename 锁框架。Remote 协议借鉴 MINIX 卷内边界返回与 Windows 每次 open 的状态分离，首版仍选单级 lookup，批量 pathwalk 以测量需求为前提。

## 2. 最小内部对象接口

下列是生产内部接口的简化表示；定义见 [provider.rs](../../os/components/filesystems/vfs/src/provider.rs)。都只在同一 VFS 镜像内，trait object / Arc 不进 wire：

```rust
trait FileSystem: Send + Sync {
    fn root(&self) -> Result<Arc<dyn FsNode>>;
}
trait FsNode: Send + Sync {
    fn identity(&self) -> NodeIdentity;
    fn lookup(&self, name: NameRef<'_>) -> Result<Lookup>;
    fn open(&self) -> Result<Box<dyn FsOpen>>;
}
trait FsOpen: Send {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<usize>;
    fn close(&mut self) -> Result<()>;
}
```

方法 object-safe，无泛型业务方法、关联 future 或返回 Self；两层 backend（Node/Open）有用途：Node 是可重复查找的对象，open 是 provider 的一次会话。Local 不需要的状态用极小类型。初版不再建一层动态 plugin registry/trait hierarchy，metadata 随需求添加；Send/Sync 只说明 Rust 同步约束，不保证 C 库可重入或跨 AS 安全。

一次 lookup 的 `Arc` 分配/引用计数与一次虚调用是对象层成本；Remote 再承担 codec、复制与调度。不能承诺 Arc 比现有 token 更快；只在 node/proxy/path/open 需要拥有生命周期时使用 Arc，短借用保留引用。生产 LocalFs 提供独立 FsOpen，OpenFile 持 Path 保活挂载和对象；read_at 不改变公开游标。

## 3. 身份、引用与图

| 对象 | 语义 / 所有权 |
|---|---|
| FS instance | Local 实例或 Remote connection incarnation；同 `.kcomp` 多次加载得到不同实例。不是全局 FS 名字 |
| Node / NodeKey | FS 内同一对象；键 `(FsIncarnation, ProviderNodeId)`，重复查找返回相等键即可，不强求 Arc 指针相同；硬链接可共享 Node |
| Dentry | 某父目录下的一条名字边；多个 Dentry 可指向同 Node，不能把 parent 存成 Node 唯一属性 |
| Mount | namespace 中一个挂载实例与其 covered **路径位置**，root 对象及 FS 引用；同 FS 可挂两处 |
| Path | `(Arc<Mount>, Arc<Dentry>)`；`.`/`..`/beneath/root 以位置判断，NodeKey 不足以判断是否逃逸或命中 mount |
| OpenFile | 一次 open：backend FsOpen、访问模式、当前游标、生命周期状态、Node/FS 的强引用；关闭后仍在途的 read 要先完成/取消 |
| Provider handle | Provider 内的 node lease 或 open session ID；不是权限、不是路径、不是 FIL 指针 |
| 外部 VFS path/file ID | VFS 对 consumer 的协议引用；owner/consumer + incarnation + 表项活性由 VFS 检查，不在 Core 每 inode 建对象 |

Namespace owns mounts/root；Path pins mount+dentry；Dentry pins node；node/proxy pins FS；OpenFile pins backend open+node/FS。反向缓存使用 Weak，避免 fs→root→fs、parent→child→parent 强引用环。生产 dentry 的父边/缓存方式要按实际遍历生命周期确定：只读首版可强父引用、弱子缓存；Mount 的 parent/covered 反向关系也不能成环。Arc 的内存存活和 provider logical-live 是两回事。

当前只读 Dentry 使用弱子缓存合并相同 canonical 名字与 NodeIdentity；每次仍调用后端 lookup，不做磁盘数据、负条目或可写目录缓存。未来 cache 由 FS 的名字比较策略和目录变化版本驱动，negative entry 有失效条件；无依据地按 UTF-8 字节小写键缓存会把 FAT/其他 FS 的语义混掉。Provider 管磁盘树/对象，VFS 只管名字关系与 mount topology，不能再维护必须和 provider 完全同步的磁盘路径树。

普通 unmount 在外部 Path/OpenFile/lease 尚存时 EBUSY；不以 Arc 计数猜全局安全，也不隐式改为 lazy detach。Provider fail 后 mount 可标记失效，旧对象存活但操作失败；显式 detach/restart 建新 connection，旧 Path 不重连。

## 4. Remote 协议与生命周期

RemoteFs 持固定 Endpoint，RemoteNode 持 connection 与挂载期借用 NodeId，RemoteOpen
持独立 OpenLease。实例 identity 包含 Endpoint incarnation，不因 provider 重启重绑。
Core 在 receive 交付 verified ComponentId/TaskId；Provider 在 read/close 检查 open 归属。
VFS 对上层也检查 Path/Open 的 Component/Task，不在 Core 为 inode 建对象。

| 操作 | 当前作用 |
|---|---|
| mount / unmount | 配置显式 Block Endpoint；unmount 需无 open，失效全部 NodeId |
| root / lookup | 返回挂载期借用 NodeId；lookup 只处理单分量与 provider 大小写规则 |
| node_details | kind、size、canonical basename，不传 C/Rust 指针 |
| open_node | 创建独立 read-only OpenLease，不接受 VFS 绝对路径 |
| read_at | 显式 offset，返回 0..count 字节；VFS 管公开 cursor |
| close | 消费一次 open；重复/过期/wrong Task 拒绝 |
| shutdown | 仅配置 control consumer；关闭 opens、unmount、结束 server，再走 Core stop |

**首版选择借用 NodeId，不引入 per-node owning lease。** FatFs 只读且无 rename，已有
canonical path 表能给出稳定挂载期身份；resident 表预算 64，耗尽明确 ENOSPC，unmount
统一失效。RemoteNode Arc 只保活连接，不跨组件 retain/release。这比提前实现 node 回收、
lease identity 与 cache 重用更小；代价是每卷 64 个不同节点上限，后续实际目录负载超过
预算时再设计拥有式节点引用/回收。不能把该受限身份等同于持久 inode 编号。

Open 有真正的 owning lease，创建前预留 8 个 deferred-close 槽之一，未提交返回预算错误；
创建失败释放预留槽。drop 不 park/分配，只把槽改为 Pending，VFS Server Task 在请求后
和 shutdown drain。Core backpressure 尚未提交 close 时保留槽，不能丢记录；方法 close
消费或旧 endpoint 失效时退休槽。整个 VFS runtime 保活 connections 直到 drain；单独使用
RemoteFs 的 Task 必须在丢弃连接前显式 drain，Task 退出另由 provider liveness sweep 兜底。

创建类操作（root/resolve/retain/open）暂记 Undo，reply 返回 ECANCELED 时立即回滚；
FatFs open_node 也在未交付 reply 时 close 新 FIL。首个成功 reply 之后的 cancel 返回
EALREADY，调用者必须收取并释放；若 Task 退出，两层服务在下一次请求通过公开
Task/Component 活性清理。清理是请求驱动，空闲时可能延迟，但表项有界、shutdown 排空。
Core 不解析 handle，不维护 FS Session；不用业务 payload 自报 consumer 授权。

旧 provider failure 后所有旧 handle 只返回 transport unavailable/stale，已有 OpenFile 保留可供 close 的本地状态；本地 close 总可逻辑关闭，remote failure 作为诊断，不“重新打开新 provider 并跳过已读字节”。新实例的 ID 即便数值同旧 ID，也因 connection incarnation 不同而不相等。Remote death 不影响其他 FS mount 或 Local open。

## 5. 路径、名字与批量解析

Namespace 处理 absolute/relative、start/root、`.`/`..`、mount crossing、symlink follow/no-follow、最大链接次数、beneath/no-cross。FS 处理一个目录内的名字查找、大小写与编码。初版只读后端无 symlink：明确报告不支持或返回 link kind 再由 VFS readlink，不能偷偷随 link 穿越 namespace。未实现 readlink 之前不宣称通用 symlink 支持。

NameRef 表示带编码的借用字节，单分量，不强制 Rust `str`。拒绝 NUL、分隔符、空分量交给边界适配；FS 决定可接受编码与 canonical 名字。FatFs 当前只有 ASCII 8.3 子集，不能由 CODE_PAGE=932 推导 Unicode/LFN；Local 测试采用 bytes，UTF-8/UTF-16 与大小写归一化是后续显式能力。整个 path 的长度、组件长度、回复长度都在 decode 处检查。

| 方案 | 代价 | 正确性 / 推荐 |
|---|---|---|
| 单级 lookup | 远程每个目录分量一次调用，易出现 IPC 放大 | VFS 每步检查 mount/root/约束；首版推荐，最少双边状态 |
| FS 内批量 pathwalk | 一个卷内片段减少 roundtrip，但返回停点/消耗偏移/引用更复杂 | MINIX 通过 mount enter/leave/link stop 让 VFS 继续；未来需明确停在 mount、link、`..`/约束边界 |
| 完整绝对路径交 FS | 代码看似少 | 拒绝：Provider 不拥有 Namespace topology，无法决定哪些路径应转到其他 FS |

启用 batch 的前置：仅把确认处于同一 FS/mount 且不会越过已知 mount 的片段交 Provider；返回 consumed byte count、终止原因、owning node/entry 信息；凭据与每级访问检查必须仍覆盖，symlink 目标回 VFS 重新解析并计数。任何不确定边界退回单级 lookup。VFS 改 mount 与并发 walk 的一致性要有版本/读锁规则，不能只依赖 Provider 路径前缀。首版不做磁盘数据 cache/rename，因此不要先造跨服务路径树同步协议。

## 6. C FatFs 与 Block 接入

实际代码事实见 [审计 §4](component-communication-audit.md#4-文件系统与-vfs-事实)。当前 `FIL` 是每次 open 状态，node slot 存 parent/canonical path。复用 mount、block binding、错误映射与 FIL 表，已补 node-based open 与 read_at；node 使用挂载期驻留借用身份；Provider 内部可从 node 解析自己的卷内 canonical path，不能让 VFS 再维护同一 path 表。只读/无 rename 时这是可接受的受限身份实现，并非未来稳定 inode 编号设计。

read_at 可以利用编译存在的 `f_lseek` + `f_read`，由 Fat Server 串行执行保持 FIL 的临时位置一致；VFS 才拥有公开 cursor。不能让 VFS 和 FIL 同时各自推进独立逻辑游标。offset/文件大小超过 FatFs 当前能力明确拒绝，不截断。`f_opendir`/`f_readdir` 可用于未来目录列举，但当前 wrapper 未导出，不为首版 lookup 强加 readdir。

FF_VOLUMES=1、private image globals、diskio active_block：多个 `.kcomp` 实例各自独立；一个 C image 内多 FAT volume 不是当前能力。真实多挂载测试应先用两个独立 Fat Provider 实例，各显式连接不同 Block Endpoint，混合 Namespace 再挂 Local。C adapter 仍需 decode、校验、调用 FatFs、encode，外置不消除 wrapper/FFI。

littlefs 当前节点接口 ENOTSUP，迁移属于 Phase 4；不能把 legacy absolute-path open 留作永久 VFS 后门。ksh `cat` 和 ELF 文件读取已一起迁至 VFS，不再消费旧 FS table。

Block 需要先具备 Server Task 端点，保证 `VFS → Fat → Block` 都能 park；可以在过渡期从 Fat Server Task 调用旧同步 Block，须标注轮询占 CPU、不代表链路新 IPC 已完成。新的 transport buffer 复制不是 DMA buffer 转移，virtio 仍需 Core DMA allocation/mapping/backing 静默机制。

## 7. 当前实现、缺口与验收

[生产对象测试](../../os/components/filesystems/vfs/src/tests.rs) 直接调用 LocalFs、Namespace、Path、OpenFile：嵌套查找、重复身份、两个 Local 挂载、alias 同节点不同目录项、挂载位置、父路径/根约束、独立游标、read_at、close/短读/EOF、missing/not-directory、挂载保活与最后 Arc 释放。它们纳入 `make check` 的 host crate 清单；独立 fake Remote 模型已删除，避免用它代替生产证据。

当前只支持 Bytes、只读文件/目录；没有 symlink、命名流、目录列举、权限/share/delete 或 namespace 运行期并发修改。Weak cache 保持活跃路径位置一致，不证明 rename/传播/bind mount 语义。

CoreTest 真 C FatFs + 两个独立 Block 实例验证混合挂载、verified Task 权限、取消 open 回滚、退出回收、Provider 失效/重启与 IPC-only stop。init runner 用 virtio FAT 镜像验证 Local/Remote 路径与 ksh/exec。证据与阶段限制见 [STATUS §3.29](../../STATUS.md#329-endpoint-requestreply-与混合-vfs--experimental)。Block 仍为同步轮询旧绑定，littlefs 和新 IPC 私有域尚待迁移，不能宣称全通信重构完成。
