# 服务执行与系统组合

> **设计契约：职责与语义边界。** 本文规定服务执行模型、组合策略和业务会话的归属；
> Endpoint Request/Reply 的当前传输规则见 [IPC](ipc.md)。标为「候选」的机制尚未定案。
> binding/transport 以 [部署契约](deployment.md) 为准，身份与停止/失败以
> [组件生命周期](component-lifecycle.md) 为准，文件对象以 [文件系统契约](../interfaces/filesystem.md)
> 和 [VFS 草案](../interfaces/vfs.md) 为准。实现进度统一见 [STATUS](../../STATUS.md)。

## 1. 独立维度

| 概念 | 回答的问题 | 所属层 |
|---|---|---|
| Artifact / Component | 哪份程序、哪次实例化 | loader / Core 实例真相；不另建 image 身份 |
| Service / Contract | 提供什么能力、请求何时完成、失败如何表达 | 接口契约与 provider |
| Endpoint | 哪个实例发布的哪个端口 | Core 发布真相 |
| Binding | consumer 如何使用确切 endpoint、有效期到哪里 | Core 交付调用窗口，SDK 保存；无独立 binding registry |
| Transport | 请求如何到达 provider | Direct / Synchronous Gate / KernelNative Request/Reply；Wasm host call 是未来路径 |
| Execution Model | 谁处理请求、是否排队、能否等待、怎样并发 | provider 的 adapter / Runtime |
| ExecutionDomain | 特权级、地址空间、可用 import 与保护条件 | 组合方提议，Core 验证并提交 |
| Session | 一次 open / connection / stream 的业务状态 | provider / 服务组件，不默认成为 Core endpoint |
| Composition | 谁连接谁、如何启动、失效后怎样调整路由 | init / profile / 组件 Runtime |

**Server / Worker 是执行模型，不是第三种 Transport。** 一个组件可以没有 Task，
也可以有多个 Worker 和 Inline 入口；不为这些角色建立 Server 基类或第二套实例表。
Native/Wasm 是代码执行后端，本文的 Inline/Queued 则描述请求处理方式，两者分别讨论。

`ComponentId`、`EndpointId`、`DeviceId` 是身份。发现身份、持有 binding、保活代码、
获准访问资源是不同事实；当前可枚举的 EndpointId 不等于不可伪造 capability。
未来让不可信组件使用服务时，必须先定义并验证调用授权，不能只验证 id 存在。

## 2. 控制路径与调用路径

```text
组合控制路径：init / profile
  → 选择 artifact、部署、配置、provider 与初始化顺序
  → Core 验证实例 / owner / 生命周期 / exact ABI / 部署能力
  → consumer 建立确切 endpoint 的 binding

请求数据路径：consumer → SDK binding
  ├─ Direct → provider 本地入口
  ├─ IPC    → Core 拥有副本 → Provider Server Task receive / reply
  └─ Gate   → Core 验证并同步进入 provider 本地入口
                ↓
       Inline 业务处理，或 Runtime 入队 → owned Worker
```

组合策略不参与每次高频读写。Core 保存 publication、实例状态、资源归属和部署真相，
不理解 root 磁盘、mount 路径、默认 FS、重试策略或客户端应当连接哪个 provider。
组合层的连接记录引用 Core 身份，不复制一份实例或 endpoint 存活真相。

当前 bind 按两端执行域确定机制；**显式 transport preference 是候选**，不是现有配置项。
若真实消费者需要，应由组合层提出需求，Core 验证可行性并交付；SDK 不自行降级。
选择 provider 与选择 transport 分开，不通过「取全局第一个」消除多 provider 歧义。

## 3. 执行与完成语义

| 组合 | 执行者与完成方式 | 当前边界 |
|---|---|---|
| Direct + Inline | caller 的栈直接运行 provider 方法，同步返回 | 已有；不切 principal，不提供独立 panic 边界 |
| Gate + Inline | Core 管理的同步服务栈执行 dispatcher，同步返回 | 已有；不是独立 Server Task 的 receive/reply |
| Direct + Queued | 本地入口提交，owned Worker 处理 | 组件侧候选；唤醒 Worker 的 owner 条件必须满足 |
| Gate + Queued | 同步入口由 Runtime 入队，owned Worker 处理 | 业务完成协议仍为候选 |
| IPC + Server Task | Core 搬运有界副本，指定 Task 接收/回复，caller 可 park | KernelNative Echo/virtio Block/Fat/VFS 已接线；私有域 Task IPC 未实现 |

Inline 表示执行者没有被移交，**不自动表示可阻塞或线程安全**。当前 Gate 栈不可
yield / park / exit；Direct 仅在合法 Task 边界及服务契约允许时可能使用 caller 的调度
上下文，不能给所有 Inline 方法一律套上 Gate 的限制。IRQ / Policy / Init / Exit 的
上下文许可继续服从生命周期与调度契约。

每项服务在接口文档中至少说明：

- 完成：返回表示工作完成，还是仅提交成功；短读、EOF、部分成功如何表示。
- 等待：允许的调用上下文，是否轮询、是否可能阻塞，以及等待设备的上界。
- 并发：同实例是否串行、锁保护哪些状态、能否重入；不能以 Gate 代替服务锁。
- 借用：输入输出只借用本次调用，还是明确转移/保活；排队后不能保留已到期的 caller 指针。
- 失败：transport 错误与业务错误、取消/超时/重复完成，以及旧对象失效后的行为。

串行 Inline、并发 Inline、单 Worker、多个 Worker 是 provider 的实现选择，暂不做
公开枚举。KernelNative Gate 可并发进入，Isolated 的入口限制也不是跨 transport 的
业务同步保证。对不可重入库，adapter 必须把实例状态和库调用放进同一同步纪律；
Gate 中不得持有需要当前 CPU 上另一个 Task 才能释放的锁并无限等待。

SDK 的 typed 前端应让业务看到数据缓冲与业务长度；wire 头、method 编号和搬运留在
调用后端。当前 Rust/C filesystem read 前端已接受普通数据缓冲区；Gate 的 8 字节
长度头与分块 scratch 留在 SDK，不交给业务调用者。

## 4. principal 与 Worker

Direct 不建立 provider 的 Core 边界。函数位于 B 的镜像、ctx 指向 B，都不能把 A 的
ambient principal 自动改为 B。Gate 的 principal 是 provider；caller Task 仅是 provenance。
详细规则只在 [生命周期 §7](component-lifecycle.md#7-资源归属-identity-规则重要陷阱)
与 [驱动契约](driver-model.md) 维护。

provider 的长期设备、DMA 池与 owned Task 优先在其 Init / Worker 中获取。
caller 的临时 backing 与 device 锚定的 DMA mapping 各自服从已有对象规则；不能为了
统一表面 owner 而更改 map/unmap，或让组件传任意 ComponentId 取得资源。

`unpark` 仅允许 owner。A 的 Direct 入口调用 B 的代码时，不因此获得唤醒 B.Worker
的权限；B.Worker 也不能凭 A.TaskId 唤醒 A。已有 Gate 可以在合法上下文中通知自己的
Worker。新 IPC 的跨 owner 完成唤醒由 Core 对真实请求执行，不放宽公开 unpark owner 检查。

Core 的 IPC 等待、匹配、取消与退出规则以 [IPC 契约](ipc.md) 为准；业务队列、Session
和对象表仍属于 Provider。普通设备 notification 与 timer 登记仍是后续能力，不能从 IPC
完成唤醒推导任意跨组件 signal 权限。

## 5. 连接、会话与失效

建立服务 binding 不等于建立业务 Session。一个 FS endpoint 可以服务许多打开对象：

```text
FS Endpoint → provider open handle → VFS OpenFile → POSIX fd / NT HANDLE
```

provider 管 handle 与业务状态；VFS 管 mount、对象引用、游标与跨客户端协调；
personality 管自己的 fd/HANDLE 与进程表示。每次 open 不在 Core Registry 新增 endpoint。

Core 使旧 provider endpoint 永久失效；组合层/VFS 处理自己的依赖与名字路由。
依赖 F 失败不意味着所有 consumer 必须一起 Failed。Core 不去修改 fd table。
Native Direct 的旧表不能被 Core 撤回，服务作者不得把逻辑失效当成缓存指针已消失。

目标失效规则：`/fat` 原指向 F1，打开 H1；F1 失败，H1 保持关联 F1 并明确失败。
组合方创建 F2 并重新挂载后，**新 open** 才得到 F2 的对象，H1 不自动重定向。
另一独立 FS 的对象不因 F1 的逻辑失败而被误伤。错误编码按对应接口定，不在这里
统一强定为 EIO。Native 内存破坏不在这项逻辑故障承诺内。

## 6. 生命周期保证分别论证

| 层面 | 必须证明什么 | 权威 |
|---|---|---|
| Logical death | 拒绝新工作、旧身份逻辑失效 | Core 生命周期 / endpoint |
| Execution quiescence | 调用、Task、已准入 IRQ、异步 callback 不再访问旧状态 | Core 可见准入 + 组件排空协议 |
| Physical reclamation | 代码/状态/backing 无 CPU 或 DMA 引用，可实际释放 | 内存、驱动、部署契约；当前不承诺完整回收 |
| Recovery | 客户端错误、重新连接、业务状态重建或重放 | 组合与业务 Runtime |

无新 binding、无长期引用、inflight 为零、无活动 Task、设备静默分别是不同条件。
当前 Direct publication 的停止限制不靠加一个全局计数解决。
Resident / Stoppable 只可作为描述部署的用语，不新增生命周期状态或 manifest 类别：
可停止也只表示通过既有停止门禁，仍不意味着物理卸载。
Restart 创建新 ComponentId；Recovery 恢复服务；Live Update 还要证明切换与状态迁移。
失败设备 quarantine 不因新实例创建而清除。

## 7. 最小落点与扩展门槛

组合从现有 init 的显式配置与连接记录开始，不先新建 Composer crate、通用服务注册中心
或 Runtime Graph 解析器。驱动匹配、设备与实例关联、部署、FS 选择、VFS mount 分步处理；
块驱动验证传输，分区/FS 组件解释磁盘字节。VFS Namespace 与 File service 先留在同一组件。
双盘负载与验证方案见 [服务研究](../development/service-runtime-study.md)，推进顺序见
[STATUS §6](../../STATUS.md#6-近期依赖链)。

新增 Core API 前，必须给出真实 consumer、当前 API 无法正确表达的调用链、状态和授权
归属、SMP/失败不变量、SDK/Runtime 更简单方案为何不足，以及成本与验证方法。
不因研究系统有 capability/channel/session 就在 Core 加对应通用对象。
