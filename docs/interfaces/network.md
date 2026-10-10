# 网络服务契约草案

> 从调用方用例推导的阶段契约，尚未实现或冻结。数值、布局、方法号与 exact
> fingerprint 以 `abi/network.toml` 为唯一来源。SDK bind 返回 ENOTSUP，其他
> 操作仍是 `todo!()`；这份契约不表示已有可运行的网络服务。

## 1. 从三个用例得到接口

组合者给调用方一个 `Endpoint<Network>`；SDK bind 后得到 `NetworkBinding`。
调用方只依赖 `kcomp-sdk`，不链接 netstack 或 smoltcp。
完整的可类型检查例子在 `os/components/kcomp-sdk/src/network/examples.rs`：
它们随 SDK host test 编译，但不执行网络占位操作。

### TCP 客户端：发请求、读响应

调用顺序为：

```rust
let net = endpoint.bind()?;
let mut tcp = net.tcp_socket(AddressFamily::Ipv4)?;
tcp.start_connect(peer)?; // 未 bind 时服务选择临时端口；返回后仍可能在握手。
// 订阅 + 查询 status，等 Established 或 failure。
// try_send 可部分发送，循环推进偏移；Pending 后等待容量恢复。
tcp.finish_send()?;      // 所有请求字节入队后结束发送方向。
let received = tcp.try_receive(&mut response)?;
// Bytes(n)、End、Pending 分开处理。
tcp.close()?;
```

这里需要：创建、开始连接、观察连接结果、有部分进度的发送、明确的流结束、
半关闭和释放。不能把 `start_connect` 返回成功解释为握手完成，也不能把
一次 send 返回成功解释为全部请求已经发完。错误路径同样必须 close。

### TCP 服务端：先占端口，再监听，接管后独立使用连接

```rust
let mut listener = net.tcp_socket(AddressFamily::Ipv4)?;
listener.bind_local(BindAddress { ip: None, port: 8080 })?;
listener.listen(8)?;
let accepted = listener.try_accept()?;
// Pending：等待；Ready(connection)：取得独立的 TcpSocket 代理。
```

bind 已经建立端口占用，listen 必须在同一个 SocketId 上转换角色，失败保留
原绑定。不能让 personality 缓存一个地址，等 listen 再抢端口。SDK 因而采用
一个 `TcpSocket` 代理；内部仍可分开存储 TcpConnection / TcpListener。
已接管连接借用 NetworkBinding，独立于监听代理；关闭监听器不关闭它。
accept 的新 ID、缓冲分配和监听槽补位均由服务负责。

### UDP 查询：发一包、收一包

```rust
let mut udp = net.udp_socket(AddressFamily::Ipv4)?;
udp.set_receive_peer(Some(peer))?;
let sent = udp.try_send_to(peer, request)?;
// Ready(())：整包入队；Pending：没有提交，等待后重试同一包。
let reply = udp.try_receive(&mut response, DatagramReadMode::Whole)?;
// Ready 包含源 / 目的地址、copied、original_len、consumed；零长度包也合法。
udp.close()?;
```

UDP 服务端可先 bind，再从接收结果的 source 取得回复目的地。短 buffer
如何处理必须明确：Whole 不够大时返回长度且保留整包，Peek 始终保留，
Truncate 复制前缀后消费整包。没有包才返回 Pending。Whole 返回未消费包后，
调用方应扩大 buffer 或选择其他模式，不能等待“更多数据”解决已有包过大的问题。

上述片段只展示操作顺序；完整例子包含 Busy 重试、订阅撤销和错误路径清理。

## 2. 服务、代理与内部对象

```text
personality / 普通组件
  → SDK NetworkBinding → TcpSocket / UdpSocket（绑定 + 不透明 ID）
  → generated IPC client → Endpoint Request/Reply
  → Server Handler → NetworkProvider
  → Engine 同步点 → Stack 对象表 / 端口 / 路由 / 私有存储池
  → 每接口 DeviceStack（smoltcp Interface + SocketSet）
```

计划一个实例发布一个 network IPC endpoint（当前未发布）；每个连接不再单独注册 Core endpoint。
端口名和设备 / 地址 / 路由 / 存储容量由组合配置选择，不自动发现默认网络栈。
create 配置 wire 尚待定稿。

| SDK 接口 | 服务方法 | 含义 |
|---|---|---|
| NetworkBinding::tcp_socket / udp_socket | tcp_open / udp_open | 创建未绑定对象，服务分配存储 |
| TcpSocket / UdpSocket::bind_local | socket_bind | 立即占用端口；本地 port=0 请求临时端口 |
| TcpSocket::start_connect / listen | tcp_connect / tcp_listen | Idle 进入握手或监听，保留对象身份 |
| TcpSocket::try_accept | tcp_accept | 接管已建立连接，返回新的独立对象 |
| TcpSocket::status | tcp_status | 连接阶段、方向结束和持久失败 |
| TcpSocket::try_send / try_receive | tcp_send / tcp_receive | 有界流收发，保留部分进度 / EOF |
| TcpSocket::finish_send / abort | tcp_finish_send / tcp_abort | FIN / 中止，均不释放身份 |
| UdpSocket::try_send_to / try_receive | udp_send_to / udp_receive | 有界整包收发，显式消费规则 |
| UdpSocket::set_receive_peer | udp_set_receive_peer | 只过滤后续投递；保留已有 inbox |
| InetSocket::info / events / subscribe / close | socket_info / events / subscribe / release | 公共能力，trait 仅在调用方镜像内使用 |
| Subscription::cancel | socket_unsubscribe | 撤销精确登记 |

Stack / SocketContext / smoltcp handle / 存储借用都是 provider 内部细节。
服务工厂可以同时创建 TCP 和 UDP；具体协议操作在调用方仍挂在相应代理上。
计划 SDK 只接受 IPC binding，不自行降级，也不把旧 ID 重绑到新实例。
部署与 binding 作用域见 `docs/architecture/deployment.md`。

## 3. 身份、状态与生命周期

- 对象与订阅 ID 非零、实例内不复用；检查耗尽。ID 是身份，不是权限、数组
  下标或 smoltcp SocketHandle。访问策略属于组合 / 部署与服务实现。
- open / accept 成功各交付一个对象。SDK 代理没有 Clone / Copy 或隐式 Drop I/O，
  使用者显式 close；personality 可以共享一个代理，在最后用户引用处关闭。
  不在本阶段提供跨执行域代理转移或服务端 retain 协议。
- bind 只接受未绑定 Idle TCP / 未绑定 UDP；family 必须匹配。TCP 连接失败后
  查询保留错误，重新连接需新建对象。监听对象不接受流收发 / FIN / abort。
- 未绑定 UDP 接收、未建立 TCP 的流操作返回状态错误，不能返回没有可用数据来源的
  Pending。TCP 方向结束后的接收按下文 EOF 规则处理。SDK 缓存创建时的不可变地址族，
  用于编码 wildcard bind；accepted 代理继承监听对象的地址族。
- 未绑定 TCP 的 connect / listen，以及 UDP 首次发送，可以由服务自动绑定
  wildcard + 临时端口。失败回滚自动绑定；UDP Pending 同样不提交首次绑定。
  自动绑定属于网络服务的端口管理能力，不必由 personality 重复实现。
- listen 的 pending_limit 必须大于零。转为监听时保留原 ID / reservation，
  多接口容量不足明确失败；不静默缩小容量。accept 无连接是 Pending，缺少对象
  或替补缓冲则是资源错误，不消费已有连接。
- accepted 连接独立保有端口所需的 reservation；监听器释放不解除该占用。
  对象 pool、backend owner、端口保留和新 ID 的提交需要统一回滚。
- close 成功是逻辑退役。Idle / UDP 清理，Connecting 中止，listener 停止并清理
  未接管连接；Established 排空已有 TX 后 FIN，未读 RX 可丢弃。需要立即中止时
  先 abort。协议清理期间服务继续保有 buffers / 端口，close 不等待远端。
  清理失败不能让 caller 重复消费 ID；无效 ID 不影响其他对象。
- SDK close 成功后代理失效；Busy 不消费对象。传输失败不能靠重新发现服务并
  自动重放来恢复，因为旧对象属于旧实例。释放和取消的错误由调用者处理。

首阶段地址限 IPv4 / 无需 scope 的 IPv6 单播；需要 zone 的 IPv6 地址和未声明的
组播 / 广播语义明确返回 ENOTSUP，不猜测接口。local wildcard 只在 bind 中合法；
peer 地址必须具体且端口非零。接口 0 表示未限定，其余为实例内接口身份。

## 4. 非阻塞调用与等待

服务方法只做有界复制、缓冲操作和状态修改，返回前不保留调用方指针。
尝试操作返回 `Result<Attempt<T>>`，Pending 没有创建异步请求；有数据可用的通知
也不是某个 read 请求的完成。后续完成式 I/O 必须另定请求 / 取消 / buffer 契约。

TCP 非空发送 Ready(n) 保证 0<n<=len；零长操作仍验证对象 / 状态，成功可以为 0。
零长 TCP 接收是 Bytes(0)，不消费 EOF；FIN 且缓冲排空后非零容量读取返回 End。
reset / 协议超时是错误。发送成功只表示复制入本组件 TX，不保证远端收到或 ACK。

一次等待的顺序必须为：

1. 登记当前 task，得到订阅 ID 和同一同步点的 events 快照。
2. 重试实际操作或查询连接状态；就绪立即处理。
3. 仍为 Pending 才调用方普通 park；醒后重新判断。
4. 完成、失败或调用方取消时撤销订阅。

服务在发布可观察变化后通知 task；提前通知由 Core permit 保留。revision
包含收发进度、连接完成 / 失败、方向结束和关闭，查询不清除状态。共享 TX staging
等容量恢复也必须通知受影响端点，不能只盯某个 backend 的 can_send。
最终释放先安排通知再撤销观察者；在途通知可导致额外唤醒，不能当成操作完成。

**Busy 与 Pending 不同。** Engine 锁竞争返回 Busy，尚未执行操作；传输失败保留为 Transport。
Busy 应在调用方让出执行机会后重试，
不能等待 socket 事件解决，也不能映射成没有唤醒来源的 Pending。
取消、超时、信号以及同时等待多个对象的策略由调用方实现。普通 park 没有
默认 deadline；跨组件 unpark / 显式 timer 仍是待实现依赖。

## 5. 并发与 worker

服务入口和 worker 经实例唯一 Engine 同步点访问状态，只有 worker 调用协议
poll。锁必须为非阻塞尝试：失败返回 Busy / worker yield；持有期间不能 park、
yield 或调用其他组件。Server Task 与 worker 的等待不能持有 Engine 锁。
临界区有界，批处理达到预算后解锁并重新调度。当前 Engine 仍是占位，没有 Sync 承诺。

driver binding 由 worker 独占。锁外收帧到 worker buffer，锁内送入 SmoltcpDevice
staging 并有界推进协议，再将 TX 复制到 worker buffer，锁外调用 driver。
smoltcp TxToken 只提交本地预留槽，不在 infallible consume 中直接调用 driver。
driver Pending 时保留待发帧并订阅容量恢复，不能覆盖或静默丢弃；设备失败需记录并
发布端点失败。通知与 timer 登记同样在解锁后执行。

服务修改状态后必须唤醒 worker；worker 提交状态后通知消费者。无工作时才 park，
budget 耗尽或临界区 Busy 时不能睡等 socket 事件。协议 deadline 取各接口最早值，
显式登记 / 更新 / 取消 timer；不新增 Core park_until，不以 timer 掩盖漏通知。
提交后的通知失败不得把已经入队的数据报告成 Pending，让 caller 重复发送；
必须记录服务 / 端点故障并处理剩余通知。通知存储容量应在提交之前检查。

## 6. 计划 IPC Wire 与错误

旧 NetworkApi function table 与 Gate 分发占位已删除。当前没有可运行的 network Endpoint，
以下为后续协议要求：在 `abi/network.toml` 补全 method AST，经现有生成器生成 C/Rust
client/codec/dispatch。所有整数逐字段 LE 编码，不复制 C padding / Rust enum；
值结构和 METHOD 常量已声明，完整方法形状与 IPC 接线仍未实现。
NetworkAddress 用高低两个 u64 表示 IP 数值：IPv4 192.0.2.1 为 high=0、
low=0xc0000201；IPv6 高 64 位在 high。地址字节转换须显式做，不能对整个对象 transmute。

- 方法返回 0 时输出有效；负值时不读输出。TCP receive 的计划 Wire output 由
  NetworkStreamRead 头和 payload 容量组成；UDP receive 用 NetworkDatagram 头。
  payload 有效长度以头内 copied / count 为准，不超过容量。
- 只有 tcp_accept / tcp_send / tcp_receive / udp_send_to / udp_receive 的
  EAGAIN 映射 Pending；其余负 errno 保留为 Method。EBUSY 映射独立 Busy。
- Core 传输错误保留为 Transport，与 provider 的结果分开。正的方法返回值、
  未知 phase / flags、非法长度等为 InvalidReply；不把未知负 errno 静默归一化。
  provider 内部依赖错误不能冒充本次外层 Core 的 Transport 错误。
- C 指针只借用本次调用，零长度 payload 允许空指针；结构与 out 参数必须有效，
  输出不得与输入 / 参数别名。Server 先校验完整 frame 形状和所有字段再操作对象，
  无效请求不能产生部分状态修改。未使用字段 / reserved 为零，未知编码拒绝。
- 单次 payload 上限见 IO_MAX，定稿 method AST 时还须满足现有 IPC envelope 上限。UDP 还受实际协议大小限制，
  超限返回 EMSGSIZE。未实现模式明确失败，不按另一种模式默默成功。

personality 负责 fd / HANDLE、进程复制与继承、sockaddr 布局、错误表示、阻塞规则、
epoll / IOCP；网络服务负责地址 / 端口、流与数据报、协议进展、缓冲和通知。
DNS、DHCP、TLS、零拷贝与应用异步请求队列不在本次契约中。
