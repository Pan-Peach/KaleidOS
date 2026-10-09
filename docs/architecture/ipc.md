# Endpoint Request/Reply

> 当前窄 IPC 契约。ABI 以 [core.toml](../../abi/core.toml) 为准；设计取舍见
> [IPC ADR](../development/ipc-request-reply-adr.md)。本页只规定传输，不规定 FS、Block
> 或业务对象协议。旧 Direct/Gate 仍按 [部署](deployment.md) 工作。

## 身份与授权

Endpoint 是不可复用的端口身份，不是 capability。发布沿用 staged publication、
contract 与 exact fingerprint；typed consumer 应先通过 `Endpoint<C>::from_id/lookup`
验证契约。IPC 不自动调用 legacy bind，也不自动降级到 Direct/Gate。

`listen` 将当前真实 KernelNative Task 注册为 owner 端口的唯一 Server Task。
`grant` 只允许端口 owner 或 Core 在实例声明时记录的创建者授权一个存活 consumer
Component。创建者来自实际 Core 请求上下文，不来自配置 payload；不会随父实例重启
重绑。grant 允许启动锚点编排，拒绝 Gate/IRQ/policy。普通 ID 查询不授予任何权限。
consumer 停止/失败移除其 grant，端口关闭永久失效。当前没有通用 rights transfer/revoke。

其余 IPC 操作只允许真实 KernelNative Task，Core 同时核对 ambient principal、
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
raw submit 的调用者必须 collect 或退出。当前没有 timer deadline 或强制终止服务。

caller Task 退出丢弃其结果，queued 请求立即退役，accepted receipt 保留至 server 归还。
Server Task 退出会关闭其端口；Provider 停止/失败关闭所有端口并以 ENOTCONN 完成尚无
终态的请求。已成功回复保留首个结果，允许存活 caller 收取；旧 endpoint 永不重定向。
关闭 transport 不替 Provider 回收业务 Session；创建对象后 caller 丢失回复的清理必须由
对应业务协议解决。VFS/FatFs 的创建类方法检查 reply 结果并回滚；两者还在后续请求
按公开 Task/Component 活性清理已交付引用。清理是请求驱动的，不保证空闲时立即回收。

提交检查 caller→Server Task 的未完成等待图，自调用或形成环返回 EDEADLK。
它不能发现业务锁的任意循环；服务必须禁止跨 IPC 持有对方需要的锁。一个 server 永不
返回 Core 时，协作式调度不能保证有限时间完成。全局 IPC 锁按 registry→endpoints→exchange
→Task table 顺序取得；Task exit hooks 在调度真相提交、其他锁释放后执行，wake 在解锁后执行。

## 执行域与内存边界

当前只支持 KernelNative Tasks：RV64 MMU 与 RV32 NoMMU 运行期测试分别验证。
私有域 import 白名单没有 IPC；Isolated/Sandboxed Server Task、范围检查与跨 AS copy
尚未实现。现有 Gate 的私有 AS 测试不能充当新 IPC 隔离证据。

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
