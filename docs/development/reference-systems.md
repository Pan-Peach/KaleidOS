# 成熟系统参考索引

> 长期研究入口，非 KaleidOS 契约。调查日期：2026-10-09。源码快照与在线文档分别标注；“借鉴”是本项目的判断，不表示已移植或机制等价。当前决策见 [IPC ADR](ipc-request-reply-adr.md)、[混合 VFS ADR](hybrid-vfs-adr.md)。

## 1. 使用与维护

遇到架构、并发或生命周期难题，先写出需要维护的不变量，再比较至少两个真正不同的成熟方案。优先官方源码、设计文档；记录 tag/commit、配置条件、函数和未核实事项。在线 latest 文档不能代替固定版本源码。评估须覆盖代价、失败、权限与部署假设，不能只比较成功路径。

以下是此次阅读范围，不是对各系统全部实现的审计。研究副本仅在临时目录，无外部代码复制进仓库；未来复用代码另行核对许可并使用 submodule。更新参考时保留决策所依据的快照，重核实发生变化的机制；不维护第二份 KaleidOS 契约。

## 2. 系统矩阵

| 系统与核对版本 | 官方入口 / 实际阅读定位 | 可借鉴 | 本阶段不照搬 |
|---|---|---|---|
| Linux **v6.12** | [VFS 源文档](https://github.com/torvalds/linux/blob/v6.12/Documentation/filesystems/vfs.rst)、[路径查找](https://github.com/torvalds/linux/blob/v6.12/Documentation/filesystems/path-lookup.rst)；`inode` / `dentry` / `file` / `(vfsmount,dentry)`；[在线 VFS](https://www.kernel.org/doc/html/latest/filesystems/vfs.html)只作导航 | 对象身份与名字边分离；open 引用保活，路径携带 mount；查找跨挂载与链接必须由 namespace 控制 | RCU-walk、全量 dcache、page cache、复杂并发重命名、Linux 模块/调度/内存子系统的完整实现。它们是后续研究方向，本次没有审计其全部源码 |
| Windows NT **WDK 在线文档，无私有内核源码证据** | [Object Manager](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/windows-kernel-mode-object-manager)、[I/O 与 file object](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/end-user-i-o-requests-and-file-objects)、[FILE_OBJECT](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/ns-wdm-_file_object)、[IRP cleanup](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/irp-mj-cleanup)、[Filter Manager](https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/filter-manager-concepts) | handle 与对象引用、stream 状态与每次 open 状态分离；最后 handle 关闭和 outstanding I/O 保活不同；业务可延后完成请求 | NT Object Manager / IRP stack / filter altitude 体系。官方 DDI 行为不能用于推断闭源内部实现或性能 |
| seL4 **13.0.0，区分 MCS 配置** | [IPC 教程](https://docs.sel4.systems/Tutorials/ipc)、[endpoint.c](https://github.com/seL4/seL4/blob/13.0.0/src/object/endpoint.c)：`sendIPC`、`receiveIPC`、`cancelIPC`；源码中的 `CONFIG_KERNEL_MCS` 分支 | rendezvous、发送者身份、一次性 reply authority、取消等待状态；权限由内核验证 | 完整 CSpace、cap derivation、fastpath、调度上下文 donation 与形式化保证。传统 caller reply cap 与 MCS Reply 对象不同，不能混称同一协议 |
| MINIX 3 **官方源码 4db99f4012570a577414fe2a43697b2f239b699e** | [VFS request.c](https://github.com/Stichting-MINIX-Research-Foundation/minix/blob/4db99f4012570a577414fe2a43697b2f239b699e/minix/servers/vfs/request.c)：`req_lookup`；[path.c](https://github.com/Stichting-MINIX-Research-Foundation/minix/blob/4db99f4012570a577414fe2a43697b2f239b699e/minix/servers/vfs/path.c)：`lookup`；[协议常量](https://github.com/Stichting-MINIX-Research-Foundation/minix/blob/4db99f4012570a577414fe2a43697b2f239b699e/minix/include/minix/vfsif.h)；[旧 wiki](https://wiki.minix3.org/doku.php?id=developersguide:vfsfsprotocol) | 卷内分段查找；`EENTERMOUNT` / `ELEAVEMOUNT` / `ESYMLINK` 返回边界及已消费偏移；FS 引用计数与 VFS vnode 分开 | grant、安全凭据、多 worker 锁、完整恢复框架。wiki 是较早版本；本次源码确认上述边界仍存在，**没有**证明所有 FS 可透明恢复。`minix3/minix` 当前只是官方仓库重定向，不作为实现证据 |
| Fuchsia / Zircon **在线 kernel/channel/FIDL 文档** | [kernel concepts](https://fuchsia.dev/fuchsia-src/concepts/kernel/concepts)、[Channel](https://fuchsia.dev/fuchsia-src/reference/kernel_objects/channel)、[directory capability](https://fuchsia.dev/fuchsia-src/concepts/components/v2/capabilities/directory)、[fuchsia.io](https://fuchsia.dev/reference/fidl/fuchsia.io/)；[源码入口](https://fuchsia.googlesource.com/fuchsia/) | handle 拥有引用及 rights；channel 消息与 peer 关闭；远程目录连接限制权限；VMO 是独立共享 backing 机制 | 每个 inode 一个 kernel object、全量 FIDL/async runtime、handle transfer/VMO/完整组件路由。未固定并通读 Zircon commit，故不声称审计其实现或具体队列常数 |
| QNX Neutrino **8.0 官方文档** | [MsgSend](https://qnx.com/developers/docs/8.0/com.qnx.doc.neutrino.lib_ref/topic/m/msgsend.html)、[消息传递](https://qnx.com/developers/docs/8.0/com.qnx.doc.neutrino.sys_arch/topic/ipc.html) | send-blocked 与 reply-blocked 分离；server 接收身份再回复；服务死亡是 transport failure；resource manager 统一语义 | 实时优先级继承、连接/channel/rcvid 的完整体系。不把文档的实时性保证移植到 KaleidOS 协作式调度 |
| Redox OS **官方 book d3850973e52d9b7f0139dc6bc36f7a4c45a6ec96** | [schemes](https://github.com/redox-os/book/blob/d3850973e52d9b7f0139dc6bc36f7a4c45a6ec96/src/schemes.md)、[scheme operation](https://github.com/redox-os/book/blob/d3850973e52d9b7f0139dc6bc36f7a4c45a6ec96/src/scheme-operation.md) | Rust 服务在用户态，provider 处理请求与延后回复；客户端 fd 与 provider handle 不同 | 将所有资源都变成文件 scheme；把书中的旧 `:scheme`/packet 示例当当前 wire ABI。书内存在历史与未来措辞，当前 scheme 库协议尚需另核源码，不能从该书推断自动重启 |
| DragonOS **87d6011c7b4e1bac577fdc016f35026d720338e8** | [IndexNode / FileSystem](https://github.com/DragonOS-Community/DragonOS/blob/87d6011c7b4e1bac577fdc016f35026d720338e8/kernel/src/filesystem/vfs/mod.rs)：`IndexNode::find` 返回 `Arc<dyn IndexNode>`；[MountFS](https://github.com/DragonOS-Community/DragonOS/blob/87d6011c7b4e1bac577fdc016f35026d720338e8/kernel/src/filesystem/vfs/mount/mod.rs)：dentry-keyed mountpoints、Weak wrapper cache；[FAT 目录项实现](https://github.com/DragonOS-Community/DragonOS/blob/87d6011c7b4e1bac577fdc016f35026d720338e8/kernel/src/filesystem/fat/entry.rs) | 同镜像 Rust 对象接口、FS 实例引用、弱反向边避免环；缓存与挂载不能只按 inode 键 | 巨大的 `IndexNode` 方法集、全量 POSIX 与 mount namespace/propagation。当前源码为 `vfs/mount/`，不能继续按旧 `mountfs.rs` 教程推断；FAT 缓存并非 KaleidOS C FatFs 实现 |
| FreeBSD **releng/14.3** | [vnode.h](https://github.com/freebsd/freebsd-src/blob/releng/14.3/sys/sys/vnode.h)：`v_usecount`、`v_holdcnt`、`v_mount`、`v_data` / `v_op`；[架构手册](https://docs.freebsd.org/en/books/arch-handbook/vm/) | 使用引用、回收保留与 FS 私有数据分离；vnode 不等于一个 open | 完整 vnode 回收协议、buffer cache 与网络栈；手册含历史实现，本次仅核对 vnode 源定义，未审计现代存储/网络路径 |
| Zephyr **v4.2.0** | [msg_q.c](https://github.com/zephyrproject-rtos/zephyr/blob/v4.2.0/kernel/msg_q.c)、[消息队列文档](https://docs.zephyrproject.org/latest/kernel/services/data_passing/message_queues.html)、[FS](https://docs.zephyrproject.org/latest/services/storage/file_system/index.html) | 固定有界消息、复制语义、锁下 waiter 协调；嵌入式预算必须可计算，多平台 API 与 FS backend 分离 | RTOS queue 不等于跨 AS 安全 IPC；不引入无消费者的 async/设备框架。online latest 与源码 tag 可能不同 |
| RT-Thread **v5.2.1** | [DFS 源码](https://github.com/RT-Thread/rt-thread/blob/v5.2.1/components/dfs/dfs_v2/src/dfs.c)、[官方 DFS 手册](https://www.rt-thread.io/document/site/programming-manual/filesystem/filesystem/) | C FS 注册/挂载与 backend ops 分工，适合考察受限内存中的薄 adapter | DFS/POSIX 放进 Core、全量动态注册；手册覆盖较早 DFS，源码为 DFS v2，不据手册断言二者布局一致 |

## 3. 本次重要选择的两组依据

- IPC：seL4 的 rendezvous/reply authority 和 Zircon 的拥有式排队是不同起点；QNX 给出阻塞直到回复的服务视角。当前选择小型 Endpoint exchange，拒绝完整 capability/channel 对象族，具体权衡见 [IPC ADR](ipc-request-reply-adr.md)。
- FS：Linux 区分 inode/dentry/path/file，DragonOS 证明 Rust 内部 Arc 对象可用；MINIX 提供远程批量路径解析的边界机制，Windows 提醒 open/cleanup/outstanding I/O 不能混成 Node 生命周期。选择单个 VFS + Local/Remote 统一对象接口，批量 pathwalk 延后，见 [VFS ADR](hybrid-vfs-adr.md)。

这些选择适用于当前工作负载与部署能力。性能、NoMMU、防护与硬件生效仍需本项目自身证据，不能由参考系统代证。

## 4. 业务协议生成补充（Cleanup）

2026-10-09另核对[Fuchsia generated bindings](https://fuchsia.dev/fuchsia-src/reference/fidl/bindings/cpp-bindings)
与[Wayland Code Generation](https://wayland.freedesktop.org/docs/book/Protocol.html)：
比较结构化wire/client/dispatch生成与薄stub路径，采用范围和明确不照搬的复杂度见
[KABI小模块设计](component-communication-cleanup-design.md#2-两个参考方案与采用范围)。
这是在线文档核对，不是固定commit源码审计，无外部代码复制。
