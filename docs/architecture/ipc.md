# Endpoint Request/Reply

> 当前窄 IPC 契约。ABI 以 [core.toml](../../abi/core.toml) 为准；设计取舍见
> [IPC ADR](../development/ipc-request-reply-adr.md)。本页只规定传输，不规定 FS、Block
> 或业务对象协议。同步策略/隔离与生命周期诊断的 Direct/Gate 仍按 [部署](deployment.md) 工作；普通业务仅使用 IPC。

## 身份与授权

Endpoint 是不可复用的端口身份，不是 capability。发布沿用 staged publication、
contract 与 exact fingerprint；typed consumer 应先通过 `Endpoint<C>::from_id/lookup`
验证契约。IPC 不自动调用 legacy bind，也不自动降级到 Direct/Gate。

`listen` 将当前真实 Component Task 注册为 owner 端口的唯一 Server Task。
`grant` 只允许端口 owner 或 Core 在实例声明时记录的真实创建祖先授权一个存活 consumer
Component。创建链来自实际 Core 请求上下文，不来自配置 payload；Registry::created_by 沿不可改写链
校验祖先，资源 owner 不因此改变，send 权限仍须显式 grant；不会随父实例重启
重绑。grant 允许启动锚点编排，拒绝 Gate/IRQ/policy。普通 ID 查询不授予任何权限。
consumer 停止/失败移除其 grant，端口关闭永久失效。当前没有通用 rights transfer/revoke。

其余 IPC 操作只允许支持域中的真实 Component Task，Core 同时核对 ambient principal、
current Task、Task owner 和 Running CPU。提交复验双方存活与 consumer grant；
receive/reply 仅属于指定 Server Task。receipt 不能靠猜中数字由另一 Task 使用。
业务收到 Core 验证过的 consumer ComponentId 与 TaskId，不能信任消息中的自报身份。

## 容量与拥有关系

全系统预分配 16 个请求槽，每端口最多 4 个占用槽；最多 32 个 listener、每 listener
16 个 consumer grant。单条 request/reply 最大 1024 字节，包括 SDK 业务编码；空消息合法。
请求 ID 单调递增，耗尽明确拒绝。每 caller Task 最多一个尚未收取的请求。

每槽只有一份 1024 字节 Core storage：submit 复制 request，receive 复制给 server，
reply 在 receive 后复用该 storage，collect 复制给 caller。队列不保存 caller/server
缓冲地址。Server 拿到自己的副本，可以继续接收并逆序回复。所有状态与 grant storage
在初始化时预留，收发、关闭和失败清理不临时分配堆对象。

容量包含 queued、accepted、completed-uncollected 和 canceled-but-receipt-live。
满时返回 ENOBUFS/ENOSPC，不半提交；短 receive/collect 返回 EMSGSIZE，不消费，允许
用足够缓冲重试。成功 collect 消费一次；方法错误应编码在正常 reply 内，和 transport
完成错误分开。Core 不解析业务方法号、文件句柄或返回数据。

## 等待、取消与退出

`submit → collect` 提供非阻塞原语；EAGAIN 后 `wait(request)` 原子检查终态并登记等待。
Server 的空 receive 使用 `wait(endpoint, 0)`。登记与完成共用状态锁；解锁后 Core 按已
验证的目标 Task owner 唤醒，现有 pending permit 覆盖 wake-before-park。SDK 同步 call
始终循环检查谓词，不把一次 unpark 当作完成；锁不跨 park 或业务后端调用。

reply、cancel、端口 close/Provider failure 在同一锁下竞争首个终态。之后的动作不覆盖
结果，不重复交付或唤醒。cancel 不承诺撤销已执行的业务；已 receive 的 receipt 仍占槽，
直到 server reply 或端口失效。晚 reply 丢弃数据、退役 receipt 并返回 ECANCELED，
让业务服务回滚未交付的拥有引用；该错误不允许重试旧 receipt。SDK call 的短输出路径会 drain Core 结果；
raw submit 的调用者必须 collect 或退出。IPC 没有请求 timer deadline；Component Force 单独撤销服务并等待真实执行离场。

caller Task 退出丢弃其结果，queued 请求立即退役，accepted receipt 保留至 server 归还。
Server Task 退出会关闭其端口；Provider 停止/失败关闭所有端口并以 ENOTCONN 完成尚无
终态的请求。已成功回复保留首个结果，允许存活 caller 收取；旧 endpoint 永不重定向。
关闭 transport 不替 Provider 回收业务 Session；创建对象后 caller 丢失回复的清理必须由
对应业务协议解决。VFS/FatFs 的创建类方法检查 reply 结果并回滚；两者还在后续请求
按公开 Task/Component 活性清理已交付引用。清理是请求驱动的，不保证空闲时立即回收。

提交检查 caller→Server Task 的未完成等待图，自调用或形成环返回 EDEADLK。
它不能发现业务锁的任意循环；服务必须禁止跨 IPC 持有对方需要的锁。一个 server 永不
返回 Core 时，协作式调度不能保证有限时间完成。IPC 先按 registry→endpoints→Task table 复验身份并释放 Task 锁，再持 AS pin→exchange 完成 copy/提交；不在 AS 锁内反取 Task 锁；Task exit hooks 在调度真相提交、其他锁释放后执行，wake 在解锁后执行。

## 执行域与内存边界

当前 RV64 S/MMU 支持 K/I/U 九格，RV32 S/MMU 支持 K/I 四格真实 Task IPC。
I 白名单与 U thunk 均接线；private buffer 通过 AS ledger 逐页检查并经 PA 复制。
无 MMU 时仅 K 路径；RV32 U 明确拒绝。Gate 硬件诊断仍单独保留。

Core 检查 null、长度、标量对齐及地址加法溢出。KernelNative 同特权同地址空间，指针
必须遵守可信 C ABI 借用约定；这不是任意坏 VA 的 fault containment，也不是安全隔离。
RPC SDK envelope 为 16 字节请求头（method/output capacity/args length/input length，均 LE
u32）和 4 字节方法状态回复头（LE i32）；其余字节由契约决定。Core 只搬运它们。
当前 Task 栈固定 16 KiB，容纳消息副本与真实 Core wait/switch 帧；不提供 stack guard 或配额。

## 验证入口

纯真相逻辑：[exchange/tests.rs](../../os/core/src/component/exchange/tests.rs)。
真实跨镜像 Server Task：[kcomp_echo](../../os/components/tests/kcomp_echo/src/lib.rs)；
CoreTest 编排：[runtime/ipc.rs](../../os/components/tests/core_test/src/runtime/ipc.rs)。
QEMU 覆盖 RV64 跨 CPU 往返及 RV32 同 CPU，比较旧 Direct/Gate 的 trace-on 基线。
结果、阶段缺口与回归统一记录在 [STATUS](../../STATUS.md)。

## 私有执行域接通

Task/import/runner/copy 已实现，一般 Graceful cleanup/drain 与完整竞态矩阵仍缺。
`access::Pinned` 持 AS 表锁覆盖完整检查、copy 与 Exchange 提交；所有输出验证先于消费。

业务 Contract/Handler 与现有 Wire 不变：K 经窄 C ABI，I 经 Core 栈/root 桥接，
U 经 syscall stub/ecall；provider 始终由自己的 Server Task/AS 执行。生成器继续只管
codec/client/dispatcher，不把 domain 分支、资源或 Session 语义放进 schema。
Task 当前 owner、Running CPU、实例域与 AS 来自 Core，不接受消息内自报身份。

I/U copy 必须逐页验证完整范围和读/写权限，U 要求 USER；不准 provider 解引用
caller 私有 VA，不依赖 SUM。除 payload 外，request id、consumer、length、completion
等所有标量输出与输入结构同样 marshal。先验证所有输出、保证 backing 在 copy 期间
稳定，再提交 receive/collect 等消费动作；copy-out 失败不能丢掉唯一结果或留下
无法收取的 submit。锁/借用顺序需与 Task/AS teardown 一起设计，不能只有一次
validate 后锁外使用可能被 unmap/free 的 PA。消息仍只存 Core-owned 副本。

停止初期沿用 close 的首终态/cancel-and-drain：成功 reply 保留供活 caller collect；
其余请求终结 ENOTCONN；关闭 receipt 不表示 provider 已停止处理自己的副本。
Task/callback 离场仍由[生命周期](component-lifecycle.md#11-runtime-完整化当前与目标)确认。
Graceful 已有 Task 的 cleanup 允许 cancel/collect/释放等拆除操作，拒绝新 submit/listen/
grant；目前 transaction 的 may_run 门禁不能支持此目标。

raw IPC 每次复验 endpoint 活性和 grant；exact ABI 当前在 typed validate/bind，
submit ABI 本身不携带 contract/fingerprint，Core 不解析业务 envelope。私有域使用
同一 typed 路径；不得宣称 raw submit 自身做 exact 校验。若将来要求不可信 raw caller
必须证明 expected ABI，应先明确最小 ABI 缺口，再协调 schema；不新增业务 Wire。

当前真实 IPC 往返已覆盖上述九格（RV32 四格）。bad buffer、I/U fault、stale 与
重复回收另由压力场景验证；不宣称各格已经穷尽 caller/server 退出和取消竞争。
逻辑寻址与连接编排的取舍见 [路由 ADR](../development/ipc-routing-service-discovery-adr.md)，
仅设计，不是私有域 Task/IPC 的前置框架。
