# ADR：组件 Endpoint 的最小 Request/Reply

> 状态：**已采纳，KernelNative 第一阶段已实现**。当前规则见 [IPC 契约](../architecture/ipc.md)；私有域接线仍未实现。依据与验证见 [源码审计](component-communication-audit.md)、[参考系统](reference-systems.md)、[迁移计划](component-communication-migration.md)。现行权威仍是 [部署](../architecture/deployment.md)、[服务执行](../architecture/service-execution.md)、[生命周期](../architecture/component-lifecycle.md) 与 [调度](../architecture/scheduling.md)。

## 1. 决策与适用范围

普通独立 `.kcomp` 服务推荐统一到 **Endpoint + Core 拥有的有界消息副本 + Provider Server Task + 一次性回复凭据**。Rust/C SDK 在其上封装同步 call；Provider 只实现一个请求处理入口，不同时发布业务 Direct table 和 Gate dispatcher。单镜像内部保持普通函数、trait、Arc。Echo、默认Fat/VFS与virtio Block的K链已接；Echo/Block/FS/VFS/Posix/Probe方法生成已接入；普通服务旧表/Gate与Block/FS SDK Backend已删，私有域替代仍未实现，见[现行方法生成](kabi-methods.md)。

Core 只负责 Endpoint 活性、发送权限、消息长度/归属、Request 生命周期、Task 等待和一次完成；不认识 Session、inode、FILE_OBJECT、block lba。初版不做通用 handle table、CSpace、共享内存、零拷贝、notification、多 server 调度框架。内部的 request/reply 记录不是 provider 对象注册表。

**必要契约变化**：现行 service-execution 把 Queued 业务状态留在组件 runtime，且普通 `unpark` 只允许 owner。当前实现只把 transport 的等待、匹配与有界字节搬运纳入 Core；业务排队、对象表与协议仍归组件。不能在旧 Gate 内藏 Worker 后声称完成；Phase 1 已原地更新服务执行、部署、生命周期及 Core ABI。跨 owner 的唤醒由 Core 自身执行，不赋予 consumer 任意 unpark 权限。

调度策略 `choose_next` 是机制提交链上的同步建议，不能等一个依赖该调度器运行的 Server Task。保留窄、同步、不可 park 的 policy 回调，单独命名其角色；它不是普通业务 transport 的第二套前端。组件 create/destroy/entry 与 panic escape 也不是 RPC。

## 2. 候选比较

以下复杂度是本项目适配成本判断，不是对成熟系统的行数测量。

| 维度 | seL4 风格 rendezvous | Zircon 风格 Channel | 最小 Endpoint exchange（推荐） |
|---|---|---|---|
| Core 状态 / 规模 | Endpoint sender/receiver 队列、阻塞线程、reply authority；无必须的大数据消息队列 | 双端对象、消息队列、handles/rights/转移、signals、peer close；通用对象族 | 复用 registry；小型固定 request pool、endpoint 队列、等待边；不引入双端 Channel |
| 同步 / 异步 | 发送者可先等 receive，再等 reply；server 独立执行 | write/read 异步，call 需关联 reply 与关闭 | submit/receive/reply/collect；SDK 同步等待，server 可延后完成 |
| 调度成本 | runnable 切换与 fastpath，MCS 有调度上下文约束 | dispatcher/read wait，异步可减少等待切换 | 首版 caller→server→caller 至少两次调度；不承诺 fastpath/优先级继承 |
| 数据 | 小消息/IPC buffer；大块通常另配共享内存 | bytes + handles，VMO 另立机制 | 全量有界复制，不共享 caller 指针；之后按实测研究 backing/mapping |
| 取消 / 超时 / 退出 | 必须撤销发送或回复等待；MCS 与非 MCS 不同 | queued 消息、handle 关闭、call reply 匹配分别处理 | 一个终态赢家；已接收 receipt 退休后才可回收槽位；无透明重试 |
| 引用 / 权限 | cap 与 reply 权限是真正 authority | handle 拥有对象引用及 rights | ID 是身份；Core grant 是发送权限；receipt 与服务 Task 绑定，不宣称 cap 系统 |
| SMP | 内核同步与调度设计可证明，但不能直接继承其证明 | 内核对象锁、waiter/close 竞态 | host-testable 状态机 + 统一锁序 + 真 RV64 SMP 竞态测试必需 |
| RV64 / RV32 NoMMU | 需要本项目硬件适配，不能由概念推导 MMU 防护 | 同样不能把通用对象代替页表 | 同协议可运行 K；RV64 Sv39 与 RV32 Sv32 的私有 AS Task 仍待接线；NoMMU 仅可信同域 |
| Rust/C SDK | cap/reply/context 对调用者要求高 | handle ownership/FIDL/runtime 复杂 | 薄 codec 与 typed client/server，C 固定结构与 switch；消息权威同源 |

拒绝全量 seL4：当前没有 CSpace、MCS 调度模型，照搬会引入无消费者的权限/调度对象。吸收 reply 权限与 rendezvous 的等待不变量。拒绝全量 Channel：第一位消费者只要同步 Request/Reply，无须双向自由 handle transfer/VMO/signals。也拒绝把 Gate 改名：Gate 仍在不可 park 的服务栈上执行，没有独立接收者或拥有式请求。

## 3. 身份与权限

| 表示 | 含义与检查 |
|---|---|
| ComponentId / TaskId / AddressSpace | 分别为资源 owner、执行者与映射域，不互相替代 |
| EndpointId | 不复用的发布 incarnation，含 contract/fingerprint/provider/port 关联；失效永不重定向 |
| typed SDK Endpoint | 已验证类型的 ID 包装，**不是** owning provider 引用或不可伪造 capability |
| grant `(consumer ComponentId, EndpointId, send)` | 由可信组合者安装，Core 从执行上下文取得 consumer 身份，不能信任 payload 中自报 owner；每次 submit 复验 |
| RequestId | 全局单调不复用；caller Task 只能等待、取消、收取自己的请求；溢出拒绝，不 wrap |
| ReplyReceipt | Core 记录绑定 Endpoint incarnation、request 和指定 server Task；只能该 Task 回复/丢弃一次，数字猜中也不授权 |
| provider 私有 handle | 业务对象身份，权限/引用规则在业务协议中，Core 不解析 |

初版每 Endpoint 指定一个 owner 的 Server Task；可接受多个不同 caller 的请求并延后回复，不允许运行时迁移 receipt 给任意 Task。每 caller Task 最多一个未收取的出站请求。组合参数传显式 Endpoint，不按全局名字“查到谁就连谁”。自调用不转成本地旁路，循环等待在提交前拒绝。

同特权 KernelNative/IsolatedNative 仍是可信代码；grant 约束 API 路径，不抵抗任意 S-mode 内存/CSR 操作。Sandbox 未实现时不得宣称不可伪造权限。真实 private-AS 消息复制依靠范围验证/copy，不能把 Rust 借用检查当跨域防护。

## 4. 有界状态与已实现入口

当前实现采用 [IPC 契约](../architecture/ipc.md) 的固定预算：16 个请求槽、每端口 4 个占用槽、1024 字节消息、32 个 listener、每端口 16 个 grant。每槽复用一份 request/reply storage，共 16 KiB 数据容量；不存在第二份永久 reply 缓冲或每端口大队列。

ABI 入口是 listen/grant/submit/receive/reply/collect/wait/cancel/close。晚 reply 在取消或 caller 退出后丢弃数据、退役 receipt 并返回 ECANCELED；服务端据此回滚未交付的业务引用。Core 仍不识别业务对象；已经成功交付而后退出的引用由 VFS/FatFs 自己按 verified Task 活性清理。

预留的 storage、listener capacity、grant 数组与固定 wake 列表让状态提交和失败清理无需临时堆分配。缓冲只借用本次入口，Core 复制；短输出不消费。KernelNative 指针遵守可信 ABI，null/长度/对齐/地址溢出检查不等于可恢复的任意 VA fault。私有 AS copy 尚未实现，不用提议的保证描述现状。

## 5. 完成、取消与退出

```text
Queued --receive--> Accepted --reply--> Done
   |                    |
   +--cancel/fail------->+--cancel/fail--> terminal error
Done/error --collect--> caller reference consumed
accepted receipt --reply/discard/fail--> receipt retired
两方引用均退休 -> 槽位可复用（RequestId 不复用）
```

- reply/cancel/provider-fail 在同一状态锁下争夺首个终态；后来的动作不能覆盖成功或错误，也不能重复唤醒/交付。业务错误是一次正常 reply。
- queued cancel 从接收队列移除；accepted cancel 使 caller 可返回，但保留 receipt 记录，server 的晚 reply 被丢弃并退休 receipt。**取消返回不承诺业务副作用未发生**，SDK 不自动重发。
- caller Task exit：queued 释放；accepted 标记取消、失去 caller 引用，receipt 退休后回收；已完成且无 receipt 立即释放。不能向退出 Task 的旧栈写回。
- server Task exit、endpoint revoke、provider fail：一次性终结 pending，撤销全部 receipt，唤醒 live callers；已 committed 的成功仍可 collect。revoke 后 submit 拒绝，receive 不再交付。
- restart 发布新 Endpoint；旧 pending/handle 不重连，不把一次旧 read 送到新实例。provider business sessions 按 FS 协议清理，Core 只能提供真实 caller 身份与失效事件，不能自行调用 FS close。
- 停止必须先停接新请求、清理 transport 等待并 drain/取消 server，最后 destroy。不能直接套现有“有 Task 则 EBUSY”的 stop；drain 的业务协议与 transport lifecycle 分工见迁移计划。
- 失效≠物理回收。KernelNative backing、私有执行域页表与 DMA 静默仍按现行生命周期/driver 契约，IPC 不扩大回收保证。

初版不提供真实时间 deadline：现有普通调度 timer tick 尚未形成该服务机制。提供显式 cancel，测试可用 CoreTest 截止时间防止挂死；后续 deadline 必须连 timer→原子终态→唤醒，不用 busy-yield 包装成 timeout。若 server 永不回到 Core，accepted receipt 可耗尽有界资源；cooperative K 下不能保证强制终止或有限时间回复。下一步必须测试 hung server 行为，不能用模型证明 liveness。

## 6. 等待、锁序、SMP 与死锁

同步 client 运行在真实 caller Task，submit 后循环：检查 terminal → 注册该 request 的 waiter → Core 提交 park → 被唤醒后重新检查 → collect。Server 空 receive 同样依赖 endpoint predicate。完成可以在注册前、注册后/park 前或 park 后到来；**predicate 注册与唤醒登记必须原子协调**，现有 Task permit 可防晚 park，但 permit 不是请求完成数据。

建议锁序 `component registry → endpoint/exchange → Task table`，与现有 lifecycle/task 路径统一；锁内仅记录 permit/ready 通知，释放锁后交给目标 CPU。`unpark` 的 public owner 检查不绕开；Core 为已验证 request 的 caller 执行内部 wake。Task 死亡/组件失败必须按相同顺序删除等待登记，禁止保留 Task 栈地址。核对 commit_switch 最终 permit 检查，验证 wake-before-park 与跨 CPU 完成。生产 host 测试验证真相状态机，实际锁与跨 CPU wake 由 QEMU 验证。

初版唯一 serverTask 允许 Core 构造有限等待图 `caller Task → endpoint server Task`：submit 前沿当前等待边检查环，self-call 或环返回 deadlock error，无 enqueue。该图只解释 transport 等待；不能检测组件持锁、设备、任意语义依赖，因此还需：

- 组合静态依赖保持 `personality/ksh → VFS → FatFs → BlockDevice` 无环；Block 不回调同步等待 VFS，完成通知另行设计。
- 不跨 IPC 持 namespace/provider/global mutex。VFS 先取 Arc/快照、解锁，再远程调用；必须复验的改动使用明确版本或顺序提交，首版只读避免引入 rename 事务。
- 每 server 串行业务状态可避免 C 不可重入库锁等待；接受中的请求由 server 自有记录保存。SMP 并发提交/取消仍由 Core 状态锁处理。
- 单 CPU 协作调度只要 park/return 交还 CPU 就能嵌套请求；轮询 virtio 仍会占 CPU，IPC 不自动把驱动改成 IRQ 等待。RR callback 不进入此依赖图。

## 7. 执行域接线与验收门槛

KernelNative Task create/start/park 与真实 Echo 已有；本轮回归结果见[专项审计](component-communication-audit.md#6-本轮真实门禁与已定位回归)。Gate 服务栈、IRQ、policy/create/destroy 同步边界不能接收同步 IPC call。需要从 Task 入口进行，禁止在 Gate 中 park。

IsolatedNative 现有同步 Gate 有私有 AS/trampoline/copy，但 import whitelist 没有 task/park/receive，单 invocation 活动限制与临时栈不等于持久 server Task。必须先实现：域关联 Task、持久私有栈、切换 satp/寄存器、受检 IPC imports、请求双向 copy、panic/exit 与 wait 回收；再测 K→I、I→K、I→I。RV64 Sv39 和 RV32 Sv32 分开验证。RV32 S-mode NoMMU 只验可信同域 transport，不承诺 private-AS；M-mode 启动与 Sandbox 另列缺口。

生产 [exchange tests](../../os/core/src/component/exchange/tests.rs) 验证 owned copy、FIFO/逆序回复、错误 owner/Task、短输出、队列满、取消 receipt 占槽、六种 reply/cancel/close 顺序、caller/server exit、等待谓词与循环等待。原先独立模型已被这些生产测试替代，避免重复维护协议。

真实 [Echo Server Task](../../os/components/tests/kcomp_echo/src/lib.rs) 和 [CoreTest](../../os/components/tests/core_test/src/runtime/ipc.rs) 使用公开 API，在 RV64 跨 CPU 与 RV32 同 CPU 验证同步调用、两个 caller、权限、错误参数、自调用、取消、Provider panic、server exit、满队列恢复、caller 退出、close/stop 与 SDK envelope。基线见 [审计](component-communication-audit.md) 后续实现记录。

缺口仍包括私有域 IPC、真实跨 AS 复制、timer deadline 和通知。逆序 reply 的完整排列目前是生产 host 证据，QEMU Echo 按接收顺序回复；不能声称覆盖所有 SMP race 或真实 IPC 隔离。旧通道保留。
