# 服务执行、组合与纵向负载研究

> **审计与研究记录，非接口契约。** 2026-10-09 核对用户提供的《KaleidOS 架构研究报告》
> 与《KaleidOS 架构深度审查：从真实负载反推设计》。两份报告基线为 `69a3b43`，
> 本次工作区基线为 `d60afb9`；本次只整理文档，未修改实现或运行 OS 测试。
> 概念边界见 [服务执行契约](../architecture/service-execution.md)，进度/优先级见
> [STATUS](../../STATUS.md)，原始资料统一见 [参考资料](../philosophy/references.md)。

## 1. 证据规则与基线变化

| 标记 | 含义 |
|---|---|
| CONFIRMED | 本次读取源码可直接确认的分支、限制或配置 |
| REPRODUCED | 实际执行指定测试复现；必须附命令、配置、日志与验证层次 |
| INFERRED | 根据源码推导的交错/后果，尚无实际复现 |
| DESIGN PROPOSAL | 候选设计，未实现或尚未决定 |

本次没有新增 REPRODUCED 结论。报告中的事件模型推演不是 QEMU 结果；已有测试的
源码和历史 PASS 记录只是已有证据，不能计为本次重跑。未实现能力记为 BLOCKED 时，
须说明缺哪个前置，不提交一个只断言 Unsupported 的用例来冒充目标功能验证。

`69a3b43..d60afb9` 已补资源 grant 与失败的提交复验、IRQ irq-save/硬件事务、
Init/Exit 调度门禁、DMA mapping id 耗尽和 create/load 准入。
其回归与局限已在 [Execution & Ownership review §12](execution-ownership-review.md#12-授权后的修复与验证2026-10-08)
记录。本研究不重复登记为未修 bug；Direct/Gate principal 差异仍是既定契约。

## 2. 保留的研究结论与取舍

| 参考系统的事实 | KaleidOS 吸收的判断 | 不直接采用的机制 |
|---|---|---|
| CAmkES 的 connector 区分同址 Direct、RPC、共享数据与通知 | Contract、Transport、部署分开；相同接口不保证所有生命周期/堆语义相同 | 静态 ADL 与完整 connector 框架 |
| Genode 父级决定 session 路由，client 使用建立后的服务引用 | 建连策略与数据路径分开；有状态 Session 留在服务层 | quota/capability 图与每项服务必建 Core Session |
| Fuchsia DFv2 区分 node、匹配、管理器、host、实例与 dispatcher | 设备匹配、实例关联、运行域分步；并发纪律不由 transport 代替 | 完整 Driver Manager、共享 driver-host 实现 |
| Linux 模块引用纪律与 RCU callback 排空处理不同条件 | 保活、执行静止、DMA 静默、物理回收分别证明 | 全局 Arc、每次 Direct 过 Core、立即添加可卸载框架 |
| MINIX 更新需要静止点、状态迁移与失败回滚 | Restart / Recovery / Live Update 分开 | 无条件透明恢复或直接移植 RS |
| FlexOS 比较真实负载的隔离配置与性能 | 固定业务负载后改变部署/调用方式，记录保护边界与成本 | 把编译期 compartment 等同于动态 ComponentId |
| RedLeaf 以语言安全与域间所有权支撑隔离 | 研究跨边界借用/转移；C/FFI 不自动获得 Rust 隔离 | 第四种执行域或共享 Rust runtime |
| Theseus 减少一个组件为另一个持有的状态 | 明确失效传播归属；VFS/fd 状态不进入 Core | 现有 C ABI/裸指针已具备 live evolution 的推断 |

上述是对参考系统的设计推论，不是其实现可移植到 KaleidOS 的证明。
资料核对链接在 [参考资料 §18–§22](../philosophy/references.md#18-camkes接口与连接方式)；
RedLeaf、Theseus、Linux 分别见该页 §7、§8、§17。

未接受报告中的三项表面简化：所有 Inline 都不可阻塞（Direct 的合法 Task 上下文不同）；
增加通用 Instance Resource Context 解决 owner（优先已有 Init/Worker 与对象规则）；
保留旧 create/load 兼容别名（ABI 协调替换，不维持陈旧兼容层）。
报告提到无 MBR 的 raw block 被拒绝属实，但 GPT 通常有 protective MBR，不能推成所有
GPT 盘都失败；0xAA55 也可能是 FAT boot sector 签名，单凭它不能证明磁盘存在 MBR。

报告提出的路径前缀 Mount Router 可作为独立、明确受限的组合实验，复用现有 FS
path-open/read 验证双 provider 选择；它不实现节点身份、相对路径/symlink 边界或
VFS 草案 ABI，不能据此把 VFS 记为完成。正式 VFS 仍按现有文件对象契约推进。

## 3. 源码问题矩阵

下面列触发条件、影响与最小选项；优先级 P1 是双盘/文件访问前置，P2 是动态/Queued
实验前置，P3 是后续部署研究。源码链接指向本地当前文件，函数名为稳定定位提示。

| # / 优先级 / 标记 | 源码与触发条件 | 初审影响与处理选项（历史触发条件） | 已有证据 / 缺少的验证 |
|---|---|---|---|
| 1 / P1 / CONFIRMED | [VirtIO create](../../os/components/drivers/virtio_blk/src/lib.rs)：sector 0 传输成功，510/511 非 0xAA55 仍 EIO | raw block attach 被格式签名阻断；驱动只验证传输，把内容解释留给分区/FS | 既有 driver 场景；缺无签名真实盘 attach/read |
| 2 / P1 / CONFIRMED | [prober dispatch](../../os/components/driver_prober/src/runtime.rs)：is_match 后 stopped=true，跳出设备循环 | 默认只自动 attach 首台；Match 应结束当前设备匹配并记录 DeviceId→ComponentId/EndpointId | CoreTest driver-multi-device **显式 create** 第二实例，不能证明自动全枚举 |
| 3 / P1 / CONFIRMED | [init root observe](../../os/components/init/src/root.rs)：第二个匹配 Block endpoint 返回 EBUSY | 正确拒绝歧义但缺配置选择；init 显式选择两项连接，不能随便取第一项 | root host 用例已有歧义/指纹/生命周期过滤；缺显式双盘组合 |
| 4 / P1 / CONFIRMED | [ksh cat/exec](../../os/components/ksh/src/shell.rs) 全局枚举 FS；[VFS namespace](../../os/components/filesystems/vfs/src/namespace.rs) 返回 Unsupported | shell 拒绝多个 provider；VFS 实现 mount/resolve，ksh 消费选定 VFS | test-init 只证单 FAT；VFS ABI/SDK 已有草案，行为 BLOCKED |
| 5 / P1 / CONFIRMED + INFERRED | [ffconf](../../os/components/filesystems/fatfs/ffconf.h) REENTRANT=0；[fatfs_open](../../os/components/filesystems/fatfs/fatfs_backend.c) 修改 files 无锁 | 同实例双 CPU open 可争用 slot/库状态；adapter 串行整个状态操作或单 Worker。冲突后果为 INFERRED | block-chain 不证并发；缺强制交错与 RV64 双 CPU 文件操作 |
| 6 / P2 / CONFIRMED | [ambient](../../os/core/src/resource/context.rs)、[DMA](../../os/core/src/resource/dma.rs)：Direct 中 alloc 按 caller，map 锚定 device owner | provider 长期动态池不能误用 caller 生命周期；优先 Init/Worker 分配，保留 alloc/map 各自规则 | 归属 review 已有回归；缺真实长期池需求，不先添通用 resource context |
| 7 / P2 / CONFIRMED | [unpark_task](../../os/core/src/sched.rs)：caller 不是 Task owner 时拒绝 | B.Worker 无权直接 wake A；A principal 的 Direct 也不能 wake B.Worker。先原型，再论证窄授权通知 | permit/同 owner/远端 CPU 有测试；跨 owner 业务完成 BLOCKED |
| 8 / P3 / CONFIRMED | [management](../../os/components/kcomp-sdk/src/management.rs)、[Core export](../../os/core/src/component/export.rs)：load 有 domain 无 config；create 有 config，指定 KernelNative | 配置与部署尚未正交；候选统一创建请求消费现有 store artifact 名，不把 artifact 字节强塞 SDK | create 准入与嵌套 Init 有测试；待有私有域配置消费者后协调 ABI |
| 9 / P3 / CONFIRMED | [endpoint bind](../../os/core/src/component/endpoint.rs)、[call dispatch](../../os/core/src/component/call.rs)：id 可发现，校验 contract/liveness/domain，无完整 per-consumer grant | EndpointId 不是 capability；未来 Sandbox 须单独设计服务授权 | host bind 错 ABI/stale/domain 及 ArchTest 部署矩阵；不证明不可信 consumer 授权 |
| 10 / P2 / CONFIRMED | [has_direct_exports](../../os/core/src/component/endpoint.rs)、[stop_component](../../os/core/src/component/exit.rs)：K 实例发布过非空 api，即使没有 bind 也拒绝 Stop | publication 保活是保守限制，不是已有 Binding refcount；未来可选显式长期 acquire/release+drain，保留 Direct 快路径 | host DirectExports 与 Gate inflight/任务停止门禁；无 Direct release 或物理 unload |
| 11 / P1 / CONFIRMED | [filesystem client read](../../os/components/kcomp-sdk/src/filesystem/client.rs)：buf 含 8 字节头；ksh 用 buffer[8..] | typed SDK 泄漏 wire；候选后端内部 scratch/搬运，业务只见数据。说明容量、分块、零长与拷贝成本 | SDK read 帧校验与 Direct/Gate 差分已有；新数据 API 待 SDK 测试，不自动改 wire |
| 12 / P1 / CONFIRMED | [POSIX syscall](../../os/components/personalities/posix/src/execution.rs)、[fd](../../os/components/personalities/posix/src/fd.rs)：只有 console fd，通用 FdTable Unsupported，openat 未接 | FAT→exec 是镜像快照，不是应用文件 I/O；VFS OpenFile→fd 引用、usermem copy 与 fork/close 语义由 POSIX 接入 | 真实 exec/fork/wait/console 已有；通用 openat/read/close BLOCKED |
| 13 / P3 / CONFIRMED（范围限制） | [Isolated](../../os/core/src/component/isolated.rs)、[私有映射](../../os/core/src/component/isolated_load.rs)、[AS](../../os/core/src/memory/address_space.rs) | S-mode+私有 AS+共享 Core 映射为受限实验；远端 TLB shootdown、DMA 静默、私有域 Task/设备、U 组件授权不能从可装载推导 | 既有 ArchTest 证明范围见 deployment §10；无新 SMP 隔离/恶意代码保证 |

本次授权修复已改变上表中的部分代码事实：#1 删除格式签名门禁；#2 完成全部分配并
去重；#3 增加根盘序号配置；#4 的 ksh 已支持显式 FS endpoint，VFS 仍未接线；
#5 的两个 C provider 用实例级 try-lock 串行库状态和文件表，竞争返回 EBUSY；
#8 的 create 同时接受 domain/config；#11 的 Rust/C SDK read 都只交业务数据。
同时修复两类新发现：C provider 的槽位复用导致旧 handle 复活（改为单调 token），
RV32 的 u64 LBA 收窄导致高位丢失（显式 EOVERFLOW）。read 的零长度调用仍验证 handle。

其余条目保持原有归属和安全约束：#6 是长期分配应位于 Init/Worker 的生命周期纪律；
#7 是 Queued 的授权通知前置；#9/#13 是未来执行域支持面；#10 的 DirectExports
门禁继续生效；#12 依赖正式 VFS。它们不能靠放宽 owner、假回收或路径字符串代理关闭。
已实施项的验收记录和剩余阶段只在 [STATUS §6](../../STATUS.md#6-近期依赖链) 维护。

模块事实入口：[init](../modules/init.md)、[ksh](../modules/ksh.md)、[VFS](../modules/vfs.md)、
[POSIX](../modules/posix.md)。既有集成用例位于
[CoreTest driver](../../os/components/tests/core_test/src/runtime/driver.rs)、
[filesystem](../../os/components/tests/core_test/src/runtime/filesystem.rs)、
[exec](../../os/components/tests/core_test/src/runtime/exec.rs)。

## 4. 真实纵向负载与前置

目标仅作为待实现实验；不把候选组合写成已支持能力：

```text
VirtIO disk A → driver A → FatFs A ─── /fat    ┐
                                             ├→ VFS → ksh / POSIX
VirtIO disk B → driver B → littlefs B ─ /little┘
```

littlefs 可在挂载失败时格式化；实验必须使用独立一次性盘并明确选择 B，避免把 A 的
FAT 镜像当作 littlefs 输入。现有 path-open/顺序 read 的 filesystem ABI 不能表达
VFS 草案要求的 node/root/lookup/read_at 等身份；最小 provider adapter/接口演进是前置，
不能用拼接字符串假装满足节点契约。

| 场景 | 初审预期（实施前的源码推导） | 目标验收与验证层次 |
|---|---|---|
| 自动双设备 + raw B | prober 首个 Match 停止；手工 attach 无签名 B 返回 EIO | CoreTest 真实两盘分别 attach/read；读写数据与 DeviceId/实例关联对应 |
| 显式两条 Block→FS 连接 | 默认 root 选择 EBUSY；已有 FS config 可传 EndpointId | 组合策略 host 覆盖歧义/错指纹/过期 id/构造失败；CoreTest 两个 FS 各读自己的数据 |
| 两个 mount、统一 cat | VFS create/resolve Unsupported，ksh 拒绝双 FS；BLOCKED | provider 语义补齐后，CoreTest /fat 与 /little 路径互不串盘；ksh 串口流程证调用入口 |
| 双 CPU 同 FS 打开/读/关 | 同步纪律不足，交错后果 INFERRED | 组件层固定 slot 交错；RV64 SMP 真实客户端任务验证锁/Worker纪律、独立游标 |
| 应用运行期 openat/read/close | openat ENOSYS，通用 fd 表未接；BLOCKED | 普通 U-mode ELF 经 POSIX→VFS；坏用户指针、EOF/短读、dup/fork共享与独立open分别验证 |
| F1 逻辑失败，F2 显式重新挂载 | Core endpoint 失效已有；VFS 对象/路由未实现；BLOCKED | 受控 Gate 故障或业务错误，旧对象不改指向，新 open 属 F2；另一 FS 继续可用 |
| Queued 完成与等待 | 队列/通知协议未定；跨 owner unpark 拒绝；BLOCKED | 原型先证请求/取消；阻塞版须证明完成早于park、失败唤醒、重复完成与撤销，最终无忙轮询 |
| K/I 同负载比较 | 合成服务部署矩阵已有，真实驱动 import/等待不支持全组合 | 先列实际可行部署，再测延迟/吞吐/栈与拷贝成本；ArchTest另证页表/IRQ/TLB，不能用性能证明隔离 |

故障场景不把 Native Direct panic 当成安全隔离，也不要求现有 FS 能正常热卸载。
重建 FS 可沿用仍健康的 Block；失败驱动的 quarantine 设备不能交新实例直接重用。
没有合法注入入口时先登记 BLOCKED，不以 CoreTest 改私有表制造故障。

### 4.1 可以被实验推翻的研究假设

| 假设 | 比较/记录方法 | 怎样判断不足 |
|---|---|---|
| 现有 Core 足以支撑双 FS namespace 的主要业务组合 | 记录 Core/SDK/provider/VFS/personality 分别需要的修改与新增 API；先区分上层语义缺口 | 必须新增 Core 对象时给最小调用链反例，不能只说「更灵活」 |
| 同一服务语义跨合法部署可保持，成本可以拆解 | 基线包括普通 Native 函数、Direct、K/K Gate、跨 AS Gate；分别测 bind、调用、AS切换、搬运、整体 I/O；变化 payload/并发，记录尾延迟与中断延迟 | 不支持的 import/等待组合登记不可比；数据规模、IRQ关闭区间或同步改变必须说明 |
| 逻辑失败只使相应对象失效，新路由不改变旧对象身份 | 分别观察旧/new binding、mount、打开对象、Task、Device/DMA；Logical death、quiescence、recovery 分组记录 | 创建新实例但旧请求无结果、旧对象静默重定向或另一 FS 被逻辑误伤均不满足 |
| Isolated 的实际可访问范围符合声明 | ArchTest 逐类检查本实例私有页、其他实例页、共享 Core 页、MMIO；受控双 CPU 映射/撤销/退役测试检查旧 TLB 翻译 | 本地 sfence/satp 切换成功不足；缺少合法远端执行前置则 BLOCKED，不能假装已有私有域 Worker |

性能实验方法服从 [benchmark 指南](benchmark.md)。最后一项是单独的正确性研究，
不阻塞 KernelNative 双 FS 主线；失败后 backing 不复用的现有限制仍须保留。

## 5. 实施与验证纪律

阶段和完成状态只在 [STATUS §6](../../STATUS.md#6-近期依赖链) 维护。
默认人类手写生产逻辑；本次用户明确授权 Agent 修改生产实现并分阶段验证。每阶段
先写当前预期/最小反例，确认真正的
缺口来自上层组合还是 Core 机制；测试经真实公开 API，拒绝路径不改变有效旧对象。

代码改动按 [测试指南](testing.md) 跑 `make check`，涉及集成跑适用 `make test-qemu`，
涉及 trap/页表/IRQ 跑 `make test-arch`；新负载需另外接入真实 harness。
这些是后续实施验收要求，不是本次文档变更已经执行的命令。
host 证状态逻辑，组件测试证协议与同步，QEMU/ArchTest 证真实入口和硬件效果；
记录配置、场景、通过/失败、日志和未覆盖边界，不以总通过数替代这些证据。
