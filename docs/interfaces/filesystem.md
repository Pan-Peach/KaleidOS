# 文件系统抽象边界契约（filesystem.md）

本文件定义文件系统相关抽象的**分层、职责边界与操作契约**，是设计契约，不是实现进度记录。
它回答三件事：每一层认识什么、谁负责保证什么、哪些硬问题必须在接口里写清而不是留给上层。
与实现冲突时以本文件为准；与 `AGENTS.md` 冲突时以 `AGENTS.md` 为准。

目标读者是实现者：读完应当能写出接口签名、判断一个操作该落在哪一层，并知道哪些点尚未定案。

## 1. 核心命题

> **Compatibility layer 定义「这个 OS 怎么看文件」；Filesystem component 定义「文件本身能做什么」。**

这一句话是整份契约的锚点。它反对两个方向的错误：

- 把上层语义（fd、HANDLE、进程上下文、`struct stat` 布局、`O_APPEND` 的编码）下沉进 FS/namespace 接口；
- 把底层真实语义（目录关系、数据、原生元数据、原子性）上浮成"任意 personality 都能拼装"的通用原语。

配套的项目级规则，适用于文件系统之外的任何 personality 边界：

> **Personality defines policy and representation; components provide mechanism and native semantics.**

## 2. 分层与依赖方向

```text
Applications
    │
Personalities（POSIX / Win32 / WASI ...）        ← 解释请求、表示、进程语义
    │
Generic Object / File API
    │
Namespace Service                                ← 对象在哪里
    │
Generic FS Interface                             ← 持久对象能做什么
    │
FS providers（FAT / ext4 / tmpfs ...）
    │
BlockDevice API
    │
drivers
```

规则只有一条：**每一层只认识自己下面那一层的接口**。上层不能假设下面的 provider 是某种格式，下层不能反过来依赖 personality 的表示。

未来的一个已知插入点：分区组件可以坐在块设备与 FS 之间。

```text
BlockDevice ──► Partition ──► BlockDevice instances ──► FS provider
```

也就是说 partition 组件消费一个块设备，再**发布若干新的块设备实例**。当前不实现，但接口形状不应把它排除在外（见 §10 的实例模型依赖）。

## 3. 五句话压缩

```text
Compatibility Layer = How an OS sees a file.
Namespace           = Where an object is.
Filesystem          = What a persistent object can do.
Block Layer         = Where persistent bytes live.
Driver              = How those bytes reach hardware.
```

五句话分别对应 §2 的五个抽象层。任何一段代码只能宣称它实现了其中一句，不能跨越。

## 4. 谁来解释、谁负责协调、谁保证正确

最初的草图把 Win32 `share mode` 这类东西放进 personality。这对**解释**是对的，对**强制**是错的。

> **Personality 解释请求；共享文件服务协调不同请求之间的关系；FS provider 保证文件系统操作本身的正确性。**

如果只有 Windows personality 记录共享限制，那么一个 POSIX 进程打开同一个文件时看不到这些限制。文件锁、delete-pending、以及"截断 vs 在途 I/O"是同一类跨 personality 问题：它们约束的是**同一底层对象上的多个打开引用之间的关系**，这种状态不属于任何一个 personality。

参考实现说明了正确位置：Windows 自身的 share 检查依赖挂在**文件对象上的打开状态**，并由同步机制保护。这份打开状态的归属应当是共享文件服务，而不是某个 personality 的私有表。

| 层次 | 主要负责 |
|---|---|
| Personality | fd / HANDLE、调用参数、进程语义、错误表示 |
| Namespace | 路径遍历、mount、相对目录、遍历限制 |
| File service | 打开对象、游标、共享访问、打开引用的生命周期 |
| FS provider | 节点、目录关系、数据、原生元数据、操作原子性 |
| Block provider | 块读写、容量、持久化相关操作 |

边界判断：一件事如果约束的是**单个请求如何被翻译**，归 personality；如果约束的是**多个请求之间的关系**，归共享文件服务；如果约束的是**一次底层操作本身是否正确**，归 FS provider。

> **阶段一落点**：Namespace 与 File service 可以只是一个 `vfs.kcomp` 内部的普通模块，不需要先拆成独立组件或独立 binding。
> 把**职责**分清立刻有价值；把**组件/binding 边界**拆开不是当前目标（见 §13）。

## 5. 并非每个操作都走完整链路

```text
open("/data/a")  →  Namespace lookup  →  File service 创建 open object  →  FS provider
read(已打开的 obj) →  File service  →  FS provider
```

`open` 一次性做路径解析；之后的 `read` 只面对已经建立的对象，不再触碰 namespace。由此推广：

> **打开之后，路径的变化不得改变这次读写的目标对象。**

这就是经典 `file`（打开实例）与 `dentry/inode`（路径与节点）分离的意义：`rename`、`unlink`、再次 `open` 同一路径，都不应让已在途的读写改变指向。接口必须允许表达"我对某个对象/实例操作"，而不是"我每次都重新解释这条路径"。

## 6. 去 POSIX 化：已定范围与未决问题

**已定**：从 FS / namespace 接口中移除下面的东西：

- `fd` / `HANDLE` 本身；
- 进程 / session 状态；
- syscall 参数编码；
- 依赖某一种 `struct` 内存布局的约定。

**保留**：底层**原生语义**，以及现有的 `0 / -errno` 状态编码。后者只是一种**状态编码选择**，它不依赖 POSIX 进程模型，NT personality 完全可以把它翻译成自己的错误表示。因此去 POSIX 化不等于抛弃 `-errno`。

**不要做**：为了看起来"通用"而给原生 FS 概念改名或强行抽象，代价是真实语义丢失。

### 原子性必须留在 personality 边界以下

- 解码 `O_APPEND` 是 personality 的活；
- "在文件末尾原子地追加"是 File service / FS provider 的契约。

`O_CREAT|O_EXCL` 同理：检查与创建必须不可被并发操作打断。`rename`、截断 vs 在途 I/O 也一样。

> **不得把一个原子操作拆成若干上层无法正确重组的小操作。**

`getattr(size)` 加 `write_at` 不是原子 append 的替代（见 §12）：上层的读大小与写偏移之间存在窗口，别的操作可以插进来。

`struct stat` 是 personality 的表示；真正存储在介质上的 owner / permissions / timestamps 是 FS 的原生元数据。两者之间的映射由 personality 完成，不是让 FS 直接产出某个平台的 `struct`。

### 未决问题（记录，不在本文件决定）

是否定义 KaleidOS 原生权限模型和/或新的错误分类法，还是保留原生权限语义加现有数值错误编码、只在信息会丢失处增加一个 domain-status 通道？

具体失败模式：把 permission-denied、share-conflict、delete-pending 三种情况塌缩成一个 `EACCES`，会让 NT 行为**无法恢复**，因为上层再也分不出是哪一种。这条一旦塌缩就不可逆，所以必须先决定再实现相关接口。

VFS 第一阶段声明的局部选择见 `docs/interfaces/vfs.md` / `abi/vfs.toml`：保留负 errno，
在回复头补 domain-status 区分共享冲突、delete-pending 等。Rust VFS SDK/服务已传递此头，当前业务只用domain=0；完整share/delete分类尚未实现；既有 FS 契约
不变，通用原生权限模型与完整 POSIX / NT 映射仍未决。

## 7. Namespace：两个必须写下的细节

### 7.1 名字不是 Rust `str`

POSIX 名字是字节串，Windows 名字是 UTF-16；不同后端还各有自己支持的字符集与大小写规则。这三者之间需要一个**显式**的转换契约。

- 阶段一可以限制到 ASCII；
- 但不得对不可表示字符做静默替换；
- 也不得在查找前统一小写（这会把大小写敏感的后端语义改掉）。

### 7.2 遍历需要一个请求上下文

Namespace 必须支持一种"从哪个目录开始、最多能走多远"的请求上下文。这样普通 cwd、NT 相对目录、WASI preopened dir 都复用同一套遍历机制。

限制必须在**实际 symlink / mount 遍历过程中**执行；只对路径字符串做前置清洗不足够，因为符号链接和 mount 会在遍历中改变可达范围。

> **KernelNative 下这些仍是可信组件之间的协作式契约。** 要针对不可信代码强制这些限制，需要执行域 / runtime 边界（私有地址空间 + 页表，见 `docs/architecture/driver-model.md` 与 `docs/architecture/component-model.md` §4）。当前不承诺。

## 8. Capabilities：描述边界，不替代操作契约

不同 FS 的真实差异很大：ext4 有 symlink / hardlink / 大小写敏感；exFAT 没有 symlink / hardlink，大小写语义也不同。因此需要暴露能力（symlink、hardlink、case sensitivity、xattr、acl、sparse、mmap ...），且能在需要时比 bool 更丰富，例如：

```text
CaseSensitivity = Sensitive | Insensitive | PerDirectory
```

但 capability 字段**不都属于同一层**：

| 能力 | 真实归属 |
|---|---|
| `hardlink` | 文件系统操作能力 |
| `case rules` | 卷 / 目录的**名字匹配规则** |
| `acl` | 需要说明是**哪一种**权限模型 |
| `mmap` | File service + cache + VM + backend 的**联合**能力 |

单一个 `acl: true` 无法表达 POSIX ACL 与 NT Security Descriptor 之间如何对应。所以：

- provider 保留它**确实能表达、确实能强制**的原生权限语义；
- personality 把请求映射到这些原生语义；
- 对同一对象使用**显式**的公共授权规则；
- 没有等价映射的操作，必须显式返回 unsupported，或使用一个**已声明**的受限映射。

"Windows personality 能访问 ext4"是合理目标；"因此每个 Windows 文件行为都能在 ext4 上完整实现"需要逐条证明，不能由前者推出。

> **Capability 查询只是规划提示（planning hint）。** 目录规则和 mount 状态随时可能变化，最终操作**仍然必须**实际检查并返回真实结果。上层可以拿 capability 做优化或提前拒绝，但不能拿它当正确性依据。

## 9. 缓存：保持方向，钉住索引单位

| 缓存 | 基本索引 |
|---|---|
| 文件数据缓存 | 文件系统实例代次 + 文件身份 + 文件内偏移 |
| 块缓存 | 块设备身份 + 块位置 |

- 文件数据缓存可以由 File service 调用，也可以由 FS provider 经**共享实现**调用；无论在哪一侧，它只有一份。
- 块缓存位于块访问一侧。

> **Personality 不拥有互不协调的文件内容副本；缓存一致性由共享文件服务与后端共同维护。**

每个 personality 各自缓存一份文件内容，等价于在系统里制造多个真相，这是明令禁止的。索引单位必须显式写上"实例代次"和"身份"，否则组件替换 / 重挂载后旧缓存会命中错误对象。

> **不要同时实现两级缓存。** 先直接读 FatFs，把接口形状定对，更容易验证；数据缓存与块缓存留到接口稳定之后再加。

## 10. 多实例（worked example）与它的当前依赖

同一份存储、同一个 FS 实例，可以被 POSIX、Win32、WASI 同时访问：

```text
/data/hello.txt        （POSIX 路径）
D:\hello.txt           （Win32 路径）
preopen /data          （WASI 句柄）
        ↓ 都解析到同一个
FsInstance + FsNode + 同一份 storage
```

这是目标形态，不是现状。要让一个底层实例同时被多个 personality 通过不同"入口"访问，需要 namespace 把多条路径映射到同一个实例/节点身份，而不是复制实例。

**诚实的依赖**：endpoint 模型已落地——endpoint 身份 = `(provider, port_name, contract)`，端口名只在 provider 实例内唯一，因此"多个同类型 FS 实例各自发布、各自被 bind"**已经可以做到**（组合方显式 `kcore_endpoint_lookup(provider, port_name)` 发现，`bind` 由 Core 选定机制）。只读Local+Remote Fat namespace已接；多personality路由仍为目标。

## 11. 现状 vs 目标

**现状**（运行期门禁以 STATUS 为准）：

- virtio `block.device` 已 IPC-only，Rust/C Block SDK 仍保留 Direct/Gate/IPC；RAM测试设备仍旧通道。
- FatFs 默认启动是IPC-only，legacy测试仍有八项表/Gate；littlefs仍旧通道，mount失败才format。
- FatFs已支持root/lookup/node_info/node_details/open_node/read_at；64个mount-lifetime borrowed Node、
  8个owned FIL open。权限与取消/失活清理由Provider按verified Component/Task处理。
- 单个VFS已含Namespace/File service、Local+Remote Fat、SDK和ksh cat/ELF文件读取，
  多实例/身份/游标/EOF/失效回归有真实CoreTest证据；littlefs节点/Remote仍未接。
- 写、目录枚举、Page Cache、完整share/delete/ACL、通用POSIX文件fd未实现。

wire布局/编号以 [filesystem schema](../../abi/filesystem.toml) 为准；架构不要求Core
识别文件对象。method生成与旧业务表退出仍未实现，见[收敛审计](../development/component-communication-audit.md)。

**目标**（本契约要长成的样子）：

- Namespace 与 File service 作为职责存在（阶段一可以是同一个 `vfs.kcomp` 内的模块）；
- 去 POSIX 化的 generic FS 接口；
- capabilities 描述边界；
- 文件数据缓存与块缓存，各自只有一份。

## 12. 设计判据：新接口该放哪一层

对任何新接口问一句：

> **如果上面那一层不是 Linux，而是 Win32 或 WASI，这个接口还成立吗？**

- 只有 POSIX 需要它 → 属于 compatibility layer；
- **除非**它需要共享状态、原子性或持久存储支持，那么必须由共享服务或 FS provider 提供。

`append` 是这条规则的标准样本：`getattr(size)` 加 `write_at` **不是**原子 append 的替代，因为两步之间存在窗口，并发操作可以插入。所以"末尾原子追加"必须由下层提供，不能由上层组合出来。

同样的推理适用于文件系统之外（socket / task / timer / process / IPC / GUI），但那些不在本文件设计。

## 13. 明确不做与未决问题

### 当前不做（deferred）

- 一个包办全 OS 的 `Generic Object API`。先做目录 / 文件 / 打开对象的契约；是否让 event / process / socket 共享同一套对象框架，以后再说；
- 统一权限模型；
- ACL 互译（POSIX ACL ↔ NT Security Descriptor 的对应规则）；
- 两级缓存（文件数据缓存 + 块缓存同时上）；
- partition 组件（接口不排除它，但本阶段不实现）；
- 热插拔/完整物理回收；多实例FS已落地，不与热插拔混为一谈。

### 未决问题（记录，等人类定稿）

| # | 问题 | 为什么现在不能替它决定 |
|---|---|---|
| 1 | 是否定义 KaleidOS 原生权限模型 | 影响 provider 保留什么语义、personality 如何映射 |
| 2 | 是否引入新错误分类法 / domain-status 通道 | permission-denied / share-conflict / delete-pending 塌缩进 `EACCES` 后不可逆 |
| 3 | Namespace / File service 何时从 `vfs.kcomp` 内部模块提升为独立组件与 binding | 职责分离马上有价值，但拆 binding 需要实例模型与真实需求 |
| 4 | 运行期mount/卸载和热替换协议 | Endpoint/实例与只读多mount已有；动态变更另定 |
| 5 | 缓存的最终归属（File service 侧还是 FS provider 侧） | 先读 FatFs 验证接口，缓存留后 |

## 14. 与其他文档的关系

- `docs/architecture/overview.md`：分层总览、「Core 不包含 VFS / FS 格式」的边界；
- `docs/architecture/component-model.md`：组件、Interface、binding、ExecutionDomain；
- `docs/architecture/driver-model.md`：`block.device` 驱动侧与 device claim / DMA；
- `docs/development/benchmark.md`：当前无稳定 FS 路径的现状与未来 FS 基准；
- `docs/philosophy/core-philosophy.md`：机制与策略分离的判断标准。
