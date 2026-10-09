# 组件通信与 VFS 源码审计

> 审计基线：`develop`，HEAD `63384b5`，2026-10-09；以该提交加本次测试工作树为依据。本文记录事实与验证，**不是新的运行契约**。后续实现须逐阶段更新证据；不能把推荐写成已完成。
>
> 交付入口：[参考系统](reference-systems.md) · [IPC ADR](ipc-request-reply-adr.md) · [混合 VFS ADR](hybrid-vfs-adr.md) · [逐文件迁移](component-communication-migration.md)。本次起初限定文档/测试，用户随后明确授权直接实现；该授权覆盖本任务生产逻辑，不扩大其他任务权限。

## 1. 真实链路与职责

```text
init 创建 virtio_blk（device claim / DMA）
  → 得到明确 block Endpoint → 创建 FatFs / littlefs
  → mount FS → 配置 ksh 的 filesystem Endpoint
ksh cat / ELF load → SDK FileSystem
  → bind 由 Core 选 Direct 或 Gate
Direct: typed frontend → api(ctx, ...) → Provider 后端
Gate: typed frontend → codec → kcore_endpoint_call
  → resolve/prepare → provider principal + 独立服务栈
  → image dispatcher(port, method) → codec → 同一 Provider 后端
FatFs diskio → C Block SDK → virtio_blk → virtio-drivers 同步请求
VFS: create ENOTSUP，尚未参与上述路径
```

| 问题 / 已实现事实 | 文件与函数定位 | 归属 |
|---|---|---|
| Endpoint publish staged，create 成功后 batch commit；失败回滚，ID 不复用、不重定向；同 contract exact fingerprint 协调更新 | [endpoint.rs](../../os/core/src/component/endpoint.rs)：`commit_pending`、`lookup`、`bind`、`invalidate_provider` | Core 身份/活性，不是 FS 语义 |
| K/K bind 返回 Direct table+ctx；K/I、I/K、I/I Gate；Sandbox 拒绝；显式 endpoint_call 也可 K/K Gate | 同文件 `select_mechanism`、`bind` | Core 部署选择 |
| Direct 不切 principal、不建立 provider panic boundary；运行在 caller 栈，不能因函数位于 B image 而得到 B 的资源权限 | [SDK endpoint](../../os/components/kcomp-sdk/src/endpoint.rs)、[containment](../../os/core/src/component/containment.rs) 与 [服务执行](../architecture/service-execution.md) | 传输/执行身份 |
| Gate 查 live Endpoint 与 provider Ready、保留 in-flight；当前链 reentry/IRQ/policy 等受限；I 单活动入口 busy；返回后 finish_call | [call.rs](../../os/core/src/component/call.rs)：`prepare`、`endpoint_call`、`dispatch`、`complete_call` | Core 调用/生命周期 |
| Gate 不把业务执行变成独立 Task；独立 service stack 支持 cooperative panic escape；不允许 yield/park/exit | [containment.rs](../../os/core/src/component/containment.rs)：`scheduling_forbidden`、`task_switch_forbidden`、`enter_task` | 调度边界 |
| Isolated 出站验证 caller 范围并搬运 Core buffer；跨 AS trampoline/satp 切换；native 同域 Gate 仍同步借用 | [isolated_call.rs](../../os/core/src/component/isolated_call.rs)、[call.rs](../../os/core/src/component/call.rs)、[containment.rs](../../os/core/src/component/containment.rs) | 地址空间与 copy |
| SDK Endpoint 是 typed Copy 身份；没有 owning Endpoint 引用、consumer send-rights 表或通用 capability space | [endpoint.rs](../../os/components/kcomp-sdk/src/endpoint.rs)、Core `bind` | 权限缺口，不能因 u64/opaque 称 capability |
| scheduler policy endpoint 有专用同步准备/调用，普通 endpoint_call 不允许调用 reserved policy | [call.rs](../../os/core/src/component/call.rs)：`prepare_policy`、`call_policy`；[sched.rs](../../os/core/src/sched.rs) | policy proposes/Core commits，不能搬到等待自身调度的 server |

Direct 提供低开销同域调用，且影响 shared backing 的长期保活；它没有不可替代的跨组件业务语义。删除 Direct 可以删 table/export/SDK backend 分支，但不会消除 codec、C adapter、权限、Provider 对象引用、Task、panic、DMA、stop/drain 或跨 AS copy。Gate 可复用 Endpoint 活性、执行身份、失败标记与 private-AS range/copy 思路；不能复用其不可 park 的服务栈充当 Request/Reply server。

## 2. Task、等待与失败

- [task/mod.rs](../../os/core/src/task/mod.rs) 的 create/start 已支持 KernelNative owned Task；[sched.rs](../../os/core/src/sched.rs) `park_current` / `unpark_task` 与 [task/table.rs](../../os/core/src/task/table.rs) switch commit 有 permit 防 wake-before-park；public unpark 验证 owner。Core 可以为真实 IPC 做内部 wake，但 consumer 不能直接唤醒其他 owner 的 Worker。
- RV64 有协作式多 CPU Task 与 remote-ready 通知；RV32 默认单 CPU。`on_timer_tick` 尚未建立普通任务抢占/IPC deadline 服务。能 park 不等于已有 receive wait/reply wait、原子谓词登记或 cross-owner transport。
- [isolated_load.rs](../../os/core/src/component/isolated_load.rs) `SUPPORTED_IMPORTS` 没有普通 task/park 服务面；当前 I 服务入口是同步 trampoline，尚无持久私有 Server Task。Sandbox loader ENOTSUP；普通 U-mode user Task 不等于 SandboxedNative Component。
- [exit.rs](../../os/core/src/component/exit.rs) `stop_component` 在 Ready、有 live Task / active call / 已发布 Direct 时返回 busy，然后才开始 stop/destroy；它没有 server quiesce/join/cancel。Direct publication 的 pin 甚至在 endpoint invalid 后仍保留，裸 table 不能撤回。
- [failure.rs](../../os/core/src/component/failure.rs) `fail_component` / `revoke_authority_and_unbind` 先逻辑 Failed、endpoint invalid、设备/DMA authority 撤销/quarantine；不是业务 Session close，也不是 KernelNative backing 物理回收。
- checksum fixture 的 single-slot mailbox + yield、Gate stop race 是当前回归证据；它不是通用 IPC，没有队列、reply ownership、任意异步 cancel。必须新增真 Server Task Echo。

未完成的生命周期：queued/accepted/completed 请求退出清理、首个终态赢家、receipt retirement、消费者死后 Provider lease 回收、旧 handle 与重启 incarnation、正常 stop 的 drain；不能从现有 containment 推导强隔离或自动恢复。

## 3. SDK、ABI 与重复胶水

[abi/component.toml](../../abi/component.toml) 定义 lifecycle/frame，[core.toml](../../abi/core.toml) 定义 Core exports/bind/call，业务 schema [block](../../abi/block.toml)、[filesystem](../../abi/filesystem.toml)、[vfs](../../abi/vfs.toml) 定义 table/方法常量与 wire 文档。Core 不解释业务 table。Rust SDK 私有携带 trait/client/service；C SDK 用固定 binding 表示遮蔽机制。

Rust [filesystem.rs](../../os/components/kcomp-sdk/src/filesystem.rs) / [dispatch](../../os/components/kcomp-sdk/src/filesystem/dispatch.rs) 与 C [kcomp_filesystem.c](../../os/components/kcomp-sdk/c/kcomp_filesystem.c) 都维护 Direct 与 Gate 分支；Fat/littlefs 各有 C dispatcher。Rust trait 本身未被禁止，只是独立 image 不能互传 Rust trait object。read 前端 **已经接受普通 data buffer**，Gate 的 8 字节长度头与 512 字节 scratch 在 SDK；service-execution 中相反旧措辞此次修正，不能以旧文档要求重复实现。

[tools/kabi/kabi_gen.py](../../tools/kabi/kabi_gen.py) 已生成 C/Rust 定义、布局断言、Core export 名称、常量，`make abi-check` 对 16 个生成文件 diff。方法语义仍主要在注释/手写 codec，不是已有 IDL；没有自动生成 client/server 编码。

selftest 的真实固定值：core functions 62、core structs 7、component structs 3、filesystem table size_ptrs 8，精确字段列表、fingerprint `0x46534E4F4445524F`、旧 method 0..4。新增 root/lookup/node_info 5..7 没有同等编号断言。它们是当前契约 golden checks，schema 演进时应协调更新，不能只删固定断言来让测试绿。

可生成：固定宽度 LE 字段、长度/reserved/flag 检查、method switch 骨架、C/Rust typed codec/调用薄层。仍需手写：FatFs/littlefs 业务转换、node/open 表、名字语义、错误映射、锁、对象引用、DMA/backing、read 分块与游标策略。建议先给 TOML 增最小 method 描述并用 synthetic schema 测试，拒绝大型 IDL/宏 runtime；具体迁移见计划。

## 4. 文件系统与 VFS 事实

| 模块 | 实际能力 | 不能推导的目标 |
|---|---|---|
| Fat [backend](../../os/components/filesystems/fatfs/fatfs_backend.c) / [state](../../os/components/filesystems/fatfs/fatfs_internal.h) | mount/unmount、path open、read/close；8 FIL slots + 8 node slots；root/lookup/node_info；try-enter busy 拒绝并发；last handle/node 单调、unmount invalidates | 尚无 node retain/release/node-open/read_at；FIL 是 open，不是稳定 Node；node slot 不回收导致查找耗尽 |
| Fat lookup | parent token→Provider 内 canonical path→`f_stat`；从 fname 归一重复查找；仅 bytes、ASCII 8.3，拒绝空/NUL/分隔符/`.`/`..`/不支持编码 | 不是 namespace pathwalker，不具备 Unicode/LFN、symlink、任意 inode 身份 |
| Fat 配置 | [ffconf.h](../../os/components/filesystems/fatfs/ffconf.h)：READONLY=1、MINIMIZE=0、LFN=0、CODE_PAGE=932、VOLUMES=1、REENTRANT=0；上游 [ff.c](../../third_party/fatfs/source/ff.c) 编译包含 f_stat/f_lseek/f_opendir/f_readdir | wrapper 只实际使用 stat/path/read；opendir/readdir/lseek 不等于已有目录服务或位置读取 |
| 多实例 | private writable image，FATFS/FIL/node/active_block 不同 `.kcomp` 实例各自独立；显式 block binding；两个独立 provider 已 QEMU 回归 | 一个 image 内多个 volume 未支持，不能把 FF_VOLUMES=1 的库当可任意多 context 的 host library |
| littlefs | 私有状态、8 open slots、同步 block adapter；mount 失败后 format，自检写入；旧只读 file service 可用 | root/lookup/node_info 仍 ENOTSUP，不是已完成 Remote Node backend；不可称 mount 每次无条件 format |
| virtio_blk | [lib.rs](../../os/components/drivers/virtio_blk/src/lib.rs) Mutex 内调用 virtio-drivers 0.13.0 read_blocks/write_blocks；private statics per image；device claim/DMA allocation+mapping，Block + probe endpoints | 同步轮询没有 server Task/IRQ I/O completion；复制消息不会证明 DMA 静默，I 真实设备 imports/window 尚缺 |
| VFS | [provider](../../os/components/filesystems/vfs/src/provider.rs) 的 NodeRef/ProviderHandle、[namespace](../../os/components/filesystems/vfs/src/namespace.rs) 的 PathRef/Mount、[file](../../os/components/filesystems/vfs/src/file.rs) 的 OpenFile/share/delete 都是结构草图，各操作 Unsupported；[runtime](../../os/components/filesystems/vfs/src/runtime.rs) create/destroy ENOTSUP | 无可用 namespace/path/open backend，无 Local/Remote、retain/release、失败传播；丰富类型不代表机制存在 |
| ksh / init | [shell.rs](../../os/components/ksh/src/shell.rs) `filesystem_endpoint`、`cat`、用户 ELF 文件加载直接走 FS；[init](../../os/components/init/src/lib.rs) 配置 FS Endpoint | 尚未通过 VFS；迁移只跑新 FS 测试而保留 shell 永久 bypass 不算完成 |

## 5. 保留功能与权威关系

现行 [filesystem](../interfaces/filesystem.md)、[vfs](../interfaces/vfs.md) 仍为契约，待实现的新方案不覆盖它们。此前 [服务研究](service-runtime-study.md)、[执行归属审查](execution-ownership-review.md) 的部分目标与本推荐不同，以当前源码判事实、现行 architecture 判契约、新 ADR 判提议，不由日期自动取代。

整个迁移期间保留：显式 block/FS 连接、多驱动多镜像实例、driver probe/NoMatch、Direct/Gate 等价与错误参数、staged publish/no-redirect、failed/stale/restart、heap 私有实例、Task park/owner/SMP、C/Rust load、FAT cat/ELF exec 与双盘不同内容、littlefs format/selftest/独立介质、无盘/坏 FAT/OOM、policy/panic/domain/真实页表回归。重构缺口不能借缩减 gate tests 隐藏。

## 6. 验证与性能基线

本轮 QEMU：10.0.11；RISC-V qemu profiles、release 组件；timebase 10 MHz；默认 RV64 Sv39/RV32 Sv32；trace 开启 `mask=2047`，并有 Gate containment。不是裸机、trace-off 性能结论。起始 clean HEAD 加本次 test-only echo 修改，无生产 IPC 修改。

测试 [convergence.rs](../../os/components/tests/core_test/src/runtime/convergence.rs) `transport_baseline` 真实调用 test-only [checksum echo](../../os/components/tests/kcomp_checksum/src/lib.rs)：Direct function 与 public endpoint_call Gate，共 0/8/64/512 字节。4 个 warmup batch，31 个测量 batch，每批 32 次；每批后验证结果；报告 min/median/p95/max，p95 为第 30 个排序值。无自适应 K 或 null subtraction；以下为 default 场景的**每批原始 ticks**，单次估值除以 32，不能把它当精确单次延迟。

| arch | bytes | Direct median / p95 | Gate median / p95 |
|---|---:|---:|---:|
| RV64 | 0 | 10 / 16 | 989 / 1023 |
| RV64 | 8 | 27 / 27 | 1007 / 1036 |
| RV64 | 64 | 26 / 26 | 1008 / 1034 |
| RV64 | 512 | 52 / 53 | 1041 / 1065 |
| RV32 | 0 | 9 / 14 | 539 / 557 |
| RV32 | 8 | 24 / 24 | 563 / 569 |
| RV32 | 64 | 27 / 29 | 572 / 586 |
| RV32 | 512 | 76 / 77 | 622 / 641 |

完整输出在 `build/tests/qemu-{rv64,rv32}/logs/coretest-*-default-*.log` 的 `[ipc-baseline]` 行（build 为非提交工件），命令 `make test-qemu` 可重跑。trace 与调度/主机抖动影响结果；此最小 harness 是迁移前对照，正式性能门禁按 [benchmark](benchmark.md) 另做 trace-off、计时分辨率校准及环境固定。**新 Request/Reply 未实现、未测量，没有 IPC vs Direct/Gate 数字。**

| 执行检查 | 结果与验证层次 |
|---|---|
| make abi-gen / abi-check | PASS；16 generated files match，没有生产 schema 变化 |
| make check（新增 Echo 后） | PASS；fmt/clippy、host tests、RV64 build/RV32 check；原有两个新 ISA host test ignored 并非失败 |
| make test-qemu（新增 Echo 后） | PASS；RV64/RV32 CoreTest default/no-block，新增 Direct/Gate Echo；init FAT/dual-FAT/no-block/bad-FAT，RV64 OOM/exec |
| make test-arch（基线） | PASS；RV64/RV32 实际硬件/私有 AS 同步 Gate、RV64 SMP；不证明新 IPC/持久 I Task |
| Request/Reply model | 8 host tests PASS；终态/owned copy/权限/退出/等待环；不是 Core 实现、无硬件访问 |
| Hybrid VFS model | 6 Rust host tests PASS；只读 Local+Remote fake、身份/位置/Arc/错误；不含真实 IPC 或 FAT |

日志：`/tmp/kaleidos-architecture-{abi-gen,abi-check,check,test-qemu,test-arch}.log` 为基线；`/tmp/kaleidos-architecture-final-{check,test-qemu}.log` 为 Echo 后；模型单独执行并纳入 test-tools discovery。后续实现的验证另记，不沿用此表冒充新机制通过。

## 7. 风险与下一步

明确建议按 [迁移计划](component-communication-migration.md) 实现小型 Endpoint exchange，先 Echo 再 VFS，保留旧机制作功能对照。最高风险是 waiter/park 原子性、失败/退出竞态、Provider lease orphan、deferred release 耗尽、I 持久 Task、正常停止 drain、同步 Block 轮询、政策回调的调度循环。队列有界控制内存，但不保证协作式 hung server 的 liveness；同特权/private-AS 不能称为对抗性隔离。
