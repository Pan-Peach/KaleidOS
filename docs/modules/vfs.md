# vfs（os/components/filesystems/vfs/）

> 现状描述。外部语义见 [文件系统](../interfaces/filesystem.md) 与
> [VFS wire](../interfaces/vfs.md)；Local/Remote 取舍见 [ADR](../development/hybrid-vfs-adr.md)。

## 当前状态

独立 `vfs.kcomp` 的 Server Task 通过 Request/Reply 提供只读路径与文件服务。
一个 Namespace/OpenFile 同时管理静态 Rust LocalFs 和独立 C FatFs 的 RemoteFs。
组件内部使用 trait、Arc、Box 和普通方法，跨镜像只传有界 LE 消息。
init 显式组合 FatFs→VFS→ksh，cat 与 ELF 文件读取使用同一 VFS；virtio Block已为IPC-only；RAM fixtures仍旧绑定，Block SDK保留三Backend。

create 的配置由 `abi/vfs.toml` 定义：control ComponentId、0..2 个 filesystem EndpointId。
Local 文件 `/local/README.TXT` 总可用，远程卷依次挂在 `/fat` 与 `/second`。
零配置仅创建 Local 服务且无控制 consumer。创建者可通过 Core grant 授权消费者。
配置先确定图，不进行全局唯一 FS 名字发现，也不重连失败实例。

## 职责与代码

| 文件 | 已实现职责 |
|---|---|
| `src/provider.rs` | object-safe FileSystem/FsNode/FsOpen；FS/Node 身份、kind/size、canonical lookup |
| `src/local.rs` | 只读内存树；独立实例身份，Arc 数据保活，独立 backend open |
| `src/name.rs` | 名字表示、单段 Bytes 校验；当前后端只支持 Bytes |
| `src/namespace.rs` | 强父/弱子 Dentry 缓存、Path=(Mount,Dentry)、挂载保活、root/beneath/no-cross |
| `src/file.rs` | OpenFile 持 Path/backend；独立游标、read_at、一次 close |
| `src/remote.rs` | generated filesystem client；固定连接与借用 NodeId；owned OpenLease；预留槽的非阻塞 drop/close drain |
| `src/service.rs` | 32 个 Path 与 32 个 Open 槽，verified Component/Task 归属；wire 解码、引用与回滚 |
| `src/runtime.rs` | 生命周期、混合挂载、owned Server Task、请求驱动 reaper、控制 shutdown |
| `src/tests.rs` | 六项生产 host 测试，对象语义及服务错误/回滚；不冒充 IPC 隔离 |

LocalFs 隐式 root；配置 parent 指向此前的目录。名字非空、最多 255 字节，拒绝
`/`、NUL、`.`、`..` 和同父重复名字；按字节区分大小写。文件数据复制到实例 Arc。
Node identity 不强求 Arc 指针相同，未增加 Core inode 注册表。

Dentry 保存 canonical 名字与父关系；每次查询后端后按名字和 NodeIdentity 合并活跃
弱缓存，不持缓存锁跨 IPC，不缓存磁盘数据或负项。Mount 按 covered 路径位置匹配，
不能按 NodeIdentity 合并。Path 保活 Mount/Dentry，OpenFile 保活 Path；活动引用时
内部 detach 为 EBUSY。`.`/`..`、root、beneath 与 mount crossing 均由 Namespace 裁决。
空路径、NUL、超过 512 字节、尾 `/` 指向文件均拒绝；外国 Namespace Path 为 ESTALE。

FatFs Node 是挂载期借用身份，在 provider 的 64 槽有界表驻留；unmount 使其失效。
RemoteNode 的 Arc 只保活本地连接，无每-node retain/release。独立 open 则拥有 provider
lease；创建前预留 8 个 close 槽之一，drop 只排队，Server Task 在请求后 drain。
provider 失败使旧连接操作失败，不重定向；Local 和其他挂载继续可用。

VFS 外部 Path/Open 引用绑定 Core 验证的 ComponentId 与 TaskId，跨 Task 数字复制
返回 EACCES，未知/过期 path 为 ESTALE，file 为 EBADF。retain 共享同一 open 游标，
重新 open 游标独立。最终 close 先消费本地引用，后端清理错误也不能重试旧 token。
新引用在 reply 取消/退出时回滚；成功交付后 Task/Component 失效，由后续请求 reaper
清理。空闲时没有独立 watchdog。控制 shutdown drain 所有 opens 后结束 server，随后
公开 Core stop 才能通过 live-Task 门禁；Native 已发布 backing 的驻留承诺仍适用。

## 限制与验证

未实现写、目录枚举、UTF-16、symlink/readlink、命名流、share/delete/ACL、page cache、
运行期 mount 协议或引用转移。只接受 read + share-read，其他访问明确拒绝。
64 个不同 Fat 节点耗尽返回 ENOSPC，8 个 provider open 耗尽返回 EMFILE；预算不是
可无限增长的 inode cache。服务调用只能在 KernelNative Task 中进行。

```sh
cargo test --manifest-path os/components/filesystems/vfs/Cargo.toml
make check
make test-qemu
```

CoreTest [hybrid_vfs](../../os/components/tests/core_test/src/runtime/hybrid_vfs.rs) 用真实
C FatFs 镜像与两个独立 FAT12 Block 实例，验证混合路径、身份、游标、wrong Task、
close/stale、短读/EOF、取消创建、退出回收、provider 失效/重启与 IPC-only stop。
init runner 另用真实 virtio FAT 盘验证 cat、Local/Remote 绝对路径、双盘选择及 RV64 ELF。
逐次结果与剩余迁移门禁统一记录在 [STATUS](../../STATUS.md)，host/构建不证明隔离。
