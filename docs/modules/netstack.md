# netstack（os/components/network/netstack/）

> 现状描述：基于 smoltcp 的网络服务骨架，尚不能联网。
> 用例、服务语义、等待和 wire 契约见 `docs/interfaces/network.md`，
> 数值 / 布局以 `abi/network.toml` 为准。第三方接入见 `docs/architecture/porting.md`。

## 当前状态

独立 `no_std` Rust `.kcomp`，已接入构建 / fmt / clippy。
`third_party/smoltcp` 为固定到 v0.14.0 的 git submodule（0BSD），作为本镜像私有
path dependency；禁用默认 features、std、alloc 和宿主设备后端。骨架编译面为
Ethernet、IPv4 / IPv6、TCP / UDP；未来协议选配仍以 `.config` / Kconfig 为真相。

已声明服务身份/值结构与方法号、SDK consumer 代理 / provider trait、
NetworkInstance 分发以及内部对象 / smoltcp adapter。操作体均为 `todo!()`，
**调用会 panic**。SDK bind 和组件 create / destroy 返回 ENOTSUP；没有发布 endpoint
或启动 worker。Engine 没有同步实现 / Sync 承诺，IPC method schema/client/Server 尚未接线。
未使用的旧 NetworkApi table 与 NetworkService 发布/Gate 占位已删除。
构建、类型检查与 packer 只证明接口和镜像形状，不是网络功能验证。

后续拟实现的调用链（当前 bind/create 明确拒绝）：

```text
其他组件：Endpoint<Network>.bind() → NetworkBinding → TcpSocket / UdpSocket
  → generated IPC client → Endpoint Request/Reply → Server Handler
  → NetworkProvider → Engine → Stack → 内部 connection / listener / UDP
  → 每接口 DeviceStack（Interface + SocketSet）→ SmoltcpDevice 本地 staging

worker：锁外 DevicePort / NetDevice driver 收发 ↔ 锁内 staging / 协议推进
```

调用方只依赖 SDK。Stack / SocketContext / 存储池 / backend handle 不进入服务参数；
SDK Rust 值在各镜像私有编译，跨镜像仍为窄 C ABI。网卡 endpoint 由组合配置指定，
NetDevice 自身的 C ABI 尚未定稿；VirtIO HAL 属于网卡 driver。

## 实例内的骨架

- Stack 保存稳定 SocketId 对象表、端口 / 路由、订阅表和本地 UDP staging。
  SocketPool 由 runtime 准备，open / listen / accept 在组件内取得存储，失败归还。
- 每接口 DeviceStack 持有 Interface / SocketSet 和 BackendId → owner / protocol /
  smoltcp handle 映射。可复用 backend slot 不成为服务 identity。
- TCP 内部分 connection / listener；SDK 对外采用同一个 TcpSocket 代理。
  Stack::listen_tcp 在原 identity 上把 Idle 转为 listener，保留端口；
  accept 创建独立对象并协调补槽、端口保留与 owner 转交。
- UDP 保留多接口 backend、独立 inbox / receive_peer 匹配；本地数据报与设备 RX
  统一分发。包边界、原始长度和短 buffer 消费规则在服务结果中保留。
- Engine 是服务与 worker 的共同访问入口，竞争返回 Busy；worker 只独占协议
  推进和 DevicePort。外部调用都在同步点之外；不再声明服务只入请求队列。
- SmoltcpDevice 的 token 只操作本地帧槽；DevicePort 保存锁外帧副本，driver
  Pending 时保留待发帧，consume 无需直接面对外部 driver 错误。

以上均为待手写结构。引用与端口保留、失败回滚、订阅通知、资源清理尚未实现。
Core 仍只提供任务 / 生命周期 / 硬件机制，不加入网络语义或 park_until。

## 代码入口

| 位置 | 内容 |
|---|---|
| `abi/network.toml` | 服务身份、exact fingerprint、值结构与方法号；method AST 尚未定稿 |
| `os/components/kcomp-sdk/src/network/client.rs` | NetworkBinding、TcpSocket / UdpSocket、InetSocket、Subscription |
| `os/components/kcomp-sdk/src/network/provider.rs` | 镜像内 NetworkProvider 业务 trait |
| `os/components/kcomp-sdk/src/network/types.rs` | 地址、状态、事件、流 / 数据报结果 |
| `os/components/kcomp-sdk/src/network/examples.rs` | TCP 客户端、TCP 服务端、UDP 查询类型检查用例 |
| `src/service.rs` | NetworkInstance / Engine、全部服务方法的分发占位 |
| `src/stack.rs` | 私有存储池、工厂 / 监听转换 / accept、借用、订阅、投递 / poll |
| `src/tcp.rs`、`src/udp.rs` | 内部协议对象、缓冲 prepare、具体 TCP / UDP 操作 |
| `src/socket.rs` | 内部静态分发、SocketContext、观察者；公共值复用 SDK |
| `src/backend.rs` | 每接口 smoltcp 对象、attach / detach / transfer |
| `src/device.rs` | NetDevice 草案、worker DevicePort、本地帧 adapter |
| `src/address.rs`、`src/port.rs`、`src/routing.rs` | 接口配置、端口预留 / 提交、路由选择 |
| `src/clock.rs`、`src/worker.rs` | 时间转换、锁外设备收发 / 通知、显式 timer、普通 park |
| `src/runtime.rs` | 配置、backing、发布 / 启动 / 回滚与退役接线占位 |

## MangoCore 参考

参考 [MangoCore](https://github.com/Mango-Iced-Americano/MangoCore)，阅读快照
`2c9691974f9cb6a7de4da46e1a37d1e64d9a98ac`。只借鉴结构，没有引入源码或运行时依赖：

| 参考 | 本骨架对应结构 |
|---|---|
| [每设备栈 / 间接 handle](https://github.com/Mango-Iced-Americano/MangoCore/blob/2c9691974f9cb6a7de4da46e1a37d1e64d9a98ac/os/src/net/config.rs) | DeviceStack；稳定 SocketId 与 BackendRef 分开 |
| [TCP 上层生命周期](https://github.com/Mango-Iced-Americano/MangoCore/blob/2c9691974f9cb6a7de4da46e1a37d1e64d9a98ac/os/src/net/socket/inet/stream/inner.rs) | 延迟 attach、多监听 backend、accept 转交与补槽 |
| [UDP 上层投递](https://github.com/Mango-Iced-Americano/MangoCore/blob/2c9691974f9cb6a7de4da46e1a37d1e64d9a98ac/os/src/net/socket/inet/datagram/udp.rs) | 原生 inbox / 匹配 / 本地投递，不按单 backend owner 交付 |
| [独立端口预留](https://github.com/Mango-Iced-Americano/MangoCore/blob/2c9691974f9cb6a7de4da46e1a37d1e64d9a98ac/os/src/net/socket/inet/common/port/registry.rs) | 端口生命周期独立于 backend；监听与接管共享保留 |

不引入全局网络单例、netns、多层业务锁或 CPU0 约定。每实例保留一个 worker，
同一 Interface / SocketSet 串行推进。无 alloc 的存储约束只影响 provider 内部，
不要求调用方构造 smoltcp buffers。

## 尚未接线与检查

NetDevice / 网卡 provider、生成 IPC 方法与 Server handlers、实例 create 配置、Engine 同步、
协议操作、跨组件 unpark / 组件 timer 均待手写。当前 Core unpark 仍限同 owner，
骨架没有伪造新 Core 导出。DNS / DHCP / TLS 和 personality socket 接线不在此阶段。

worker 的显式唤醒源为 driver RX / TX、服务提交和协议 timer。普通 park 没有兜底
deadline；验证通知时不注册兜底 timer，定时路径单独验证，避免漏通知被定时唤醒掩盖。

```sh
make abi-check
cargo test --manifest-path os/components/kcomp-sdk/Cargo.toml --offline --locked
cargo clippy --manifest-path os/components/network/netstack/Cargo.toml --target riscv64gc-unknown-none-elf --offline --locked -- -D warnings
tools/build-kcomp.sh "$PWD/os/components/network/netstack" riscv64gc-unknown-none-elf "$PWD/build/netstack/netstack.kcomp" "$PWD/build/netstack/cargo"
```
