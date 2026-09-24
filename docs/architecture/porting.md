# 第三方库移植与调包能力（porting.md）

本文件定义 KaleidOS 的一条**核心能力**（不是事后补丁）：把成熟第三方库经**薄 adapter** 变成系统 component 的路径、约束与候选地图。
它回答三件事：哪些库可以调、调包时谁负责把世界接起来、以及为什么这条路径不会把 Core 拖进"重写整个世界"的泥潭。

与 `AGENTS.md` 冲突时以 `AGENTS.md` 为准；接口语义契约见 `docs/interfaces/`。

> **本文件同时是设计契约与候选地图。** 它描述目标形态与硬约束，也登记候选库。
> 候选不等于已实现；每一条的状态都显式标注（已落地 / 设计 / 提案 / 远期），不得拔高。

## 1. 核心命题

> 真正厉害的形态不是 KaleidOS 自己实现 FAT / ext4 / TCP / TLS / WASI / Win32，而是提供一套足够小、足够稳定的 **native semantic interfaces**，让成熟代码通过薄薄的 adapter 变成 component。

这不是"少写点代码"的偷懒，而是一次**战略换位**：

| | 自己实现 | 调包 + adapter |
|---|---|---|
| 维护对象 | 整个世界（每种格式、每种协议、每个 runtime） | 接口 + 胶水 |
| 替换成本 | 重写实现 | 换一个上游库，重写几百行 adapter |
| 出错面 | 实现本身 + 接口 | 接口 + 胶水；实现由上游社区长期打磨 |
| Core 纪律 | 容易被功能拖进来 | 只认 native semantic interface，不认具体库 |

**战略收益**：维护负担从"维护整个世界"移动到"维护接口和胶水"。KaleidOS 的价值不来自"我有一份自己写的 ext4"，而来自"给我一个还算 portable 的 C / Rust library，我能很快把它变成系统 component"（见 §11）。

这条命题与 `core-philosophy.md` 的"默认外置"同源：既然文件系统格式、网络协议栈、Wasm runtime、POSIX 语义都不进 Core（见 `AGENTS.md` 的"Core 只收真相，不收功能"），那么这些能力**要么自己写、要么调包**。本文件主张：**默认调包。** 自己写是例外，不是默认。

## 2. 第三方代码的三档

不是所有库都能同样便宜地接进来。按"它对宿主环境要求多少"分三档：

### 第一档（最理想，sweet spot）

库本身按 **portable / embedded** 设计，只向宿主索要**少量回调**（读一个块、发一个包、要一次随机数、看一次时钟）。接法：

```text
existing library  →  tiny KaleidOS adapter  →  .kcomp
```

绝大多数 FS / net / TLS / USB / runtime 库属于这一档。它们自带"宿主接口"这一抽象层（diskio、netif、HAL、WASI host call），adapter 只需要把这层接口翻译到 KaleidOS 的 native semantic interface。**本文件的主战场就是这一档。**

### 第二档

库需要一个**有界但成规模的运行环境**：线程、同步原语、定时器、socket、堆。adapter 之外还要先有若干 host service。成本明显更高，但边界仍然可控（前提是统一 host 接口 §8 已经存在）。

### 第三档（更晚、更难）

库需要**完整的 POSIX / Unix 环境**：进程、信号、`mmap`、Unix domain socket、还有一整个后台服务进程。这一档非常后期，典型代表见 §7（Wine / ReactOS）。**不要**把第三档的需求混进第一档的接口设计里。

> **档位决定投入顺序**：先吃第一档，用它们把 native semantic interface 打磨稳定；第二档会反过来暴露接口缺口；第三档只在 POSIX personality 真正立起来之后才谈。

## 3. 移植的落点：组件内 adapter + SDK 可选 host-glue

> **不设独立的 `kport` 层。** 移植产生的代码只落在两处，两处都不构成新层：

```text
third-party library
      ↓  （library native API，例如 disk_read / disk_write / disk_ioctl）
① 组件内 adapter（per-library，不可共享）
      ↓
native semantic interface（BlockDevice / Filesystem / Clock / RNG / …）
      ↓
② SDK 可选 host-glue（可跨库共享，随组件私有携带）
```

- **① 组件内 adapter**：把"库期望的宿主接口"接到 native semantic interface。它**天然是 per-library、per-component 的**——FatFs 的 `disk_read/write/ioctl`、littlefs 的 `lfs_config` 回调、lwext4 的 blockdev 签名各不相同，**没有可共享的代码**。例：`os/components/filesystems/fatfs/diskio_kaleidos.c`。
- **② SDK 可选 host-glue**：只有当某个库索要一小段"宿主原语"时才存在，且**可跨库共享**。首个实例是 picolibc 的 `_write` / `sbrk` / `_exit`（见 §6）；将来可能还有 `Clock` / `RNG` 的 shim。它属于 SDK，**不是**独立层。

**为什么不做独立 `kport` 层：**

- "语言中立"（原本是 kport 的最强论据）已由 SDK 承担：schema `abi/*.toml` → C 头 + Rust 模块（§10）；
- native semantic interface（Block / Filesystem / …）已在 SDK 提供，双语言、带生成式布局断言；
- adapter 是 per-library 的，独立层里**没有东西可放**；
- 独立层会凭空多出**第四个 ABI 面**（Core ABI / component ABI / interface ABI / port ABI），违背"少即是多"。

> **Rule of three：** 只有当 **≥3 个 adapter 共享了真实、非平凡的代码**时，才把那段代码抽进 SDK（**不是**抽成一个 `kport` 层）。在此之前，独立层是 premature abstraction。

### 3.1 硬规则（load-bearing）

> **第三方代码永远不得直接调 Core。**

反面样本，禁止出现：

```text
fatfs → kcore_get_mmio()        ✗  库越过了抽象，直接摸核心
fatfs → BlockDevice API → block provider   ✓
```

正面形态：

```text
fatfs      → BlockDevice API → block provider
lwext4     → BlockDevice API → block provider
littlefs   → BlockDevice API → block provider
smoltcp    → NetDevice API   → net provider
lwIP       → NetDevice API   → net provider
```

**为什么这是硬规则：** 它正是让 FatFs / lwext4 / littlefs **挂在同一个 `BlockDevice` 上**、让 smoltcp / lwIP **挂在同一个 `NetDevice` 上**的原因。库对 KaleidOS、virtio、component、core、MMIO、DMA **必须一无所知**。库只认识它自己文档里那套宿主回调；回调由 adapter 提供；adapter 再经 native semantic interface 落到真正的 provider。

一旦允许库直调 `kcore_*`：
- 换一个块驱动就要改库的 adapter；
- 同一份库无法在不同执行域 / 不同 provider 下复用；
- Core 的导出白名单会被"某个库恰好需要的东西"污染。

> **判据**：一个第三方库的源码里出现 `kcore_` / KaleidOS 任何符号，就是设计错误。库应当只出现它自己的原生 API 与标准 C（或 no_std Rust）依赖。

## 4. 候选库地图

| 功能 | 可调包 | 档位（§2） | 许可 | KaleidOS 需要自己写什么 | 状态 |
|---|---|---|---|---|---|
| FAT / exFAT | FatFs | 一档 | 宽松（ChaN） | `disk_read` / `disk_write` / `disk_ioctl` → `block.device` | **已落地**（只读 kcomp 服务，见下） |
| MCU flash FS | littlefs | 一档 | BSD-3-Clause | block read / program / erase adapter | **已落地**（C `.kcomp`，只读服务；见下） |
| ext2/3/4 | lwext4 | **二档** | **GPLv2**（去 `ext4_extents.c`/`ext4_xattr.c` 才 BSD-3，但那正是 ext4 的意义） | block device adapter + malloc + C stdlib | 候选（**先定许可策略**） |
| TCP/IP（Rust） | smoltcp | 一档 | 0BSD | `NetDevice` + clock adapter | 候选（前置：`NetDevice` 契约，§8） |
| TCP/IP（C） | lwIP | 二档 | BSD-3-Clause | `netif` + timer / OS（线程 / 同步）adapter | 候选（前置：`NetDevice` + Thread/Sync） |
| TLS | Mbed TLS | 二档 | Apache-2.0 | RNG + clock + socket adapter | 候选（前置：RNG / Clock / Socket） |
| USB | TinyUSB | 一档（可 bare-metal，需同步原语） | MIT | controller / HAL + 同步原语 | 候选 |
| WASI / Wasm runtime | WAMR | 二档偏三档 | Apache-2.0 | WASI host calls → KaleidOS services | 候选（前置：File / Namespace，§9） |
| C libc | picolibc | 一档 | BSD-3-Clause | `_write` / `sbrk` / `_exit` glue + 交叉编译 recipe | 候选（见 §6） |

> **档位不是"难度感觉"，是硬约束**：它决定 adapter 之外还要先有哪几个 host service（§8）。把二档当一档接，会以为省了写实现的力，结果先要补一堆 host service。
> **许可必须核对上游 LICENSE。** KaleidOS 本体是 MIT；GPL-only 库（如 lwext4）在纳入前必须先定许可策略（独立 profile / 只用 BSD 子集 / 替换），不得先接后议。

> **已落地的先例是 FatFs 与 littlefs。**
> - **FatFs**：只读文件系统服务——`os/components/filesystems/fatfs/` 是 C `.kcomp`，包上游 `ff.c` 加一个 `block.device` diskio，向上发布最小 `filesystem` 服务契约。
> - **littlefs**（v2.9.3）：`os/components/filesystems/littlefs/` 是 C `.kcomp`，包上游 `lfs.c` + `lfs_util.c` 加一个 `block.device` 适配（read/prog/erase/sync；block.device 无 erase，故 erase = 整块写 0xFF，prog 用 read-modify-write）。对外仍是**只读** `filesystem` 服务；`mount` 内 format+mount+自检（写读校验，真实走 prog/erase）。
> - **多实例已端到端证明**：CoreTest 的 `littlefs-multi-instance` / `littlefs-isolation` 场景（`os/components/tests/core_test/src/runtime/filesystem.rs`）创建 2× `ram_blk_rw` → 2× `littlefs`，各带独立块设备与 `lfs_t`；`make test-qemu` 下两个实例的 provider / endpoint / instance id 互不相同，且擦掉其一的原始存储不影响另一实例（rv64 + rv32）。
> 现状与未决问题以 `docs/interfaces/filesystem.md` §11 为准。
> **其余全部是候选，不是集成状态。** 本文件不声明其它候选库已被接入，也不写具体上游版本号（版本无关紧要，架构契合度才决定能否调）。

## 5. 实例模型对齐：littlefs 与 Endpoint / Instance

littlefs 把文件系统状态放在**调用者分配的 `lfs_t`** 里，因此同一份代码天然支持**同时挂载多个文件系统**。这与 KaleidOS 的 **Component Definition → Component Instance** 几乎一一对应：

```text
lfs_t #1 / #2 / #3     →     FsInstance #1 / #2 / #3
（调用者分配，互不干扰）        （实例身份 + 私有状态 + 独立 storage）
```

这解释了为什么"实例模型"值得先做：`lfs_t` 这种"状态由调用者拥有"的设计，正是多实例 FS 想要的形状。

> **诚实的前提**：endpoint 模型已落地——endpoint 身份 = `(provider, port_name, contract)`，端口名只在 provider 实例内唯一，所以"多个同类型 FS 实例各自发布并各自被 bind"**已经可以做到**（组合方显式 `kcore_endpoint_lookup(provider, port_name)` 发现 + `bind`）。见 `docs/interfaces/filesystem.md` §10 及其未决问题（第 3、4 条）；namespace / 多 personality 路由仍属设计目标。

## 6. C runtime：不要手写 libc

不要靠不断长大 `kcomp-libc` 来满足库的需求。正确方向是调 **picolibc**：它按 32 / 64 位 embedded 设计，有明确的 OS-support 层，stdio 底部接口很窄，正好落在第一档（§2）。

现状（务必按此描述）：

- SDK 目前只提供 **freestanding** 的 `memcpy` / `memset` / `memmove` / `memcmp` / `strlen` / `strchr`（`os/components/kcomp-sdk/c/kcomp_rt.c`，**weak** 定义），以及 `<string.h>` / `<errno.h>` shim；
- 它刻意只实现组件真正引用到的原语，**不朝 libc 扩张**（见 `docs/architecture/component-model.md` §2.3 与 `docs/modules/components.md`）；
- 真正 libc 是**未来移植目标**，不是现状。

> 判据：当第三个库开始抱怨"少了个 `malloc` / `snprintf`"时，答案是**移植 picolibc**，不是往 `kcomp_rt.c` 里再塞一个函数。

### 6.1 动态内存：机制已在，只是 C 侧刻意不给

- **Rust 组件现在就能动态分配**：`kcomp-sdk` 的 `alloc` feature 把 `#[global_allocator]` 接到 per-instance heap（`src/alloc.rs` → `crate::heap`；分配器是 freestanding C，backing 经 `kcore_memory_acquire` 取）；
- **C 组件现在不能**，不是缺机制，是两个刻意的选择：`kcomp_rt.c` 不朝 libc 扩张；C 没有 `#[global_allocator]`，要 malloc 得手写一层包在 `kcomp_heap_alloc` 上；
- FatFs 今天不需要 malloc，纯因配置（`FF_FS_READONLY=1` / `FF_USE_LFN=0` / `FF_FS_EXFAT=0`）。一旦开写支持 / LFN / exFAT，它就会要 `ff_memalloc`。

### 6.2 picolibc 的接入形状：可选库，谁用谁引

- **链接私有**：每个 `.kcomp` 静态链自己那份 picolibc（与"第三方是组件私有实现"一致，不是 shared runtime）；
- **构建共享**：picolibc 用 KaleidOS 的 freestanding recipe 为每个 target 交叉编译**一次**（`third_party/` submodule + build 步骤），产物是被各组件链入的静态库；
- **opt-in**：组件级开关，形状同 `alloc` feature。

### 6.3 picolibc 需要 KaleidOS 提供什么（host-glue）

| picolibc 要求 | KaleidOS 侧落点 | 备注 |
|---|---|---|
| `_exit` | 标记该 instance Failed / Core 退出路径 | `abort` / `raise` 也落到它 |
| `_write` | → `kcore_log_line` / console | tinystdio 输出只需这一个 |
| `sbrk`（malloc 靠它） | 静态 arena，或包 `kcomp_heap_alloc` | picolibc 明确支持 sbrk 返回**不连续**内存 |
| `close` / `lseek` / `open` / `read` | File service（**尚未实现**） | 只有要 `fopen` 才需要；等 File / Namespace |
| `__libc_lock_*` | 单线程编译即可免 | 不引入多线程 libc |

这段 glue 就是 §3 的"SDK 可选 host-glue"，放 SDK，不是新层。SDK 里已备好一个 **opt-in** 实现：`os/components/kcomp-sdk/libc/kcomp_libc_glue.c`（`_write` → console、`_sbrk` → 组件私有静态 arena、`_exit`；刻意放在 `libc/` 而非 `c/`，故**不随默认构建链接**，只有显式把它加进自己 `kcomp-c-src.txt` 的组件才会链）。`__ashlti3` / `__lshrti3` 由链接 `libclang_rt.builtins` 提供，**不手写**。

### 6.4 真正的前置：loader 重定位 recipe（不是"链一下"）

**picolibc 不是 drop-in 替换 `kcomp_rt.c`。** `.kcomp` 是 `clang -c`（`-ffreestanding -fno-builtin -fno-pic -mno-relax -mcmodel=medany`、关 `.eh_frame`、function-sections）→ partial link → packer，而 **loader 只认一份重定位 / 段白名单**（见 `tools/build-kcomp-c.sh` 与 `tools/kcomp-link.sh` 的契约校验）。因此必须：

- 用**同一套 freestanding recipe** 交叉编译 picolibc（否则它的重定位 / TLS / `.init_array` / PIC 会被 packer 拒）；
- 非 TLS `errno`、单线程、无 unwind；
- 这是"编一次"的活，不是"每次引"的活。

> **顺序：** 现在不急。FatFs 既不要 malloc 也不要 printf。等**第一个真正要 malloc / printf 的库**出现（开 FatFs 写支持 / LFN / exFAT，或接 lwIP / WAMR）时再上。

**已验证的 cross-file recipe（2026-09，picolibc 1.5.1 + clang 10；rv64 / rv32 均通过）——提案，尚未接入构建：**

picolibc 自带 cross file 直接建出的 `libc.a` 会被 packer 拒（含 `ALIGN` / `RELAX` / `BRANCH` / `JAL` / `RVC_*` / `ADD*` / `SUB*` / `TPREL_*` 及 `.init_array` / `.tdata` / `.tbss`）。根因：它没用组件的 freestanding 旗标。把 cross file 的 `c` / `cpp` 换成组件同款：

```text
clang --target=riscv64-unknown-elf -nostdlib \
  -march=rv64gc -mabi=lp64d -mcmodel=medany \
  -mno-relax -fno-pic -fno-unwind-tables -fno-asynchronous-unwind-tables \
  -ffunction-sections -fdata-sections
```

（rv32 用 `--target=riscv32-unknown-elf -march=rv32imac -mabi=ilp32`。）再加 meson 选项 `-Dnewlib-global-errno=true`（去 TLS errno）。结果：**BRANCH / ALIGN / RELAX / eh_frame 全部消失**；archive 选择性链接（组件只用 `strcpy` / `malloc` / `strlen` / `snprintf`）后，最终 `.kcomp` 的重定位**全部落在白名单内**，TLS / init_array 成员不会被拉入。

组件侧仍需提供的 glue：`_write`（→ console）、`sbrk`（或 `__heap_start` / `__heap_end`）、`_exit`；以及 `__ashlti3` / `__lshrti3`（128 位移位，C 组件没有 compiler_builtins）。

**已知环境约束（2026-09）：** clang 10 建不了 picolibc 1.8（libm complex 用 `__builtin_complex`，需 clang ≥ 11），但能建 **1.5.1**；rv32 的 meson 链接器探测在 clang 10 下需 `-B<dir>`（该 dir 放一个指向 `riscv64-unknown-elf-ld` 的 `ld` 包装）绕过 `-fuse-ld=<绝对路径>` 不被识别的问题。

## 7. Windows 兼容：嵌套是长线选项，不是当前任务

Windows personality 未必意味着全部由 KaleidOS 自己实现。它可以是一个 **runtime component + host adapter**。两条研究路线：

### Wine（长线，非常后期）

与其手写 kernel32 / ntdll / user32 / advapi32，不如移植 **Wine**。Wine 的设计本就是把 Win32 API 调用翻译成 POSIX 调用，因此它坐在一个 **KaleidOS POSIX personality** 之上是自然形态。代价：Wine 需要一个很丰富的 POSIX / Unix 环境（线程、`mmap`、信号、Unix socket、`wineserver`），属于 §2 第三档。**非常后期。**

### ReactOS（借语义，不整体移植）

ReactOS 更适合**学习语义 / 借用孤立模块**，而不是整体移植：

- 它假设 NT Object Manager / syscall / 进程 / I/O 模型，整体采纳有把 KaleidOS 变成 NT 的风险；
- 它是 GPLv2，代码复用需要仔细处理许可。

> **结论**：Windows personality 的路线是"runtime component + host adapter"，且**前置条件明确**（先有 POSIX personality 与第二档 host 接口）。当前不做，只登记方向。

## 8. 统一 host 接口：真正要设计的东西

库不该看见一堆零散机制。它只应看见一个**统一、收窄**的宿主面：

```text
Third-party portable code
        ↓
组件内 adapter + SDK 可选 host-glue（§3；不是一层）
        ↓
Memory | Clock | RNG | Block | Net | Files | Thread | Sync | Log
        ↓
KaleidOS Interfaces
```

这张表就是"一个 portable library 允许看见的全部"。状态必须分清（**不得把提案说成现状**）：

| host 能力 | 状态 | 权威位置 |
|---|---|---|
| Block | **已落地**：SDK 的 `block.device` 设备接口 | `os/components/kcomp-sdk/src/block.rs`、`docs/interfaces/filesystem.md` §11 |
| Files / Namespace | **设计**：分层与职责已定，尚未实现 | `docs/interfaces/filesystem.md` |
| Net | **提案**：尚无 `NetDevice` 契约文档 | 本文件（待成文） |
| RNG | **提案** | 本文件（待成文） |
| Clock | **提案**（Core 有 timer 机制，但未见统一 `Clock` 接口） | 本文件（待成文） |
| Log | **部分**：组件经 `kcore_log_line` 输出，无独立 `LoggerService` 契约 | `docs/modules/components.md` |
| Thread | **提案**：有 task 机制，无"库可移植线程"契约 | 本文件（待成文） |
| Sync | **提案**：无独立同步原语契约 | 本文件（待成文） |

> **诚实边界**：统一 host 接口是**待设计的提案**，不是已存在的层。Block 已有；Files / Namespace 是设计；Net / RNG / Clock / Log / Thread / Sync 尚未成文。不要拿这张表当"已实现能力清单"。

## 9. WASI via WAMR：FS 抽象的理想试金石

**不要**自己实现 Wasm VM。用 **WAMR**，把 WASI host calls 接到 KaleidOS services：

```text
WASI host call（path_open / fd_read / fd_write / clock_time_get / random_get）
        ↓
KaleidOS Namespace / File / Clock / Random service
```

这正是统一 host 接口（§8）的一次实战。更重要的是，**WASI 是检验文件系统 / namespace 抽象是否被 POSIX 污染的理想试金石**：

> 如果 WASI 基于 capability 的 preopened-dir 模型能落在**同一套** Namespace / File service 上（而不是逼出一套平行的"WASI 专用"接口），那么这套抽象就是站得住的。

WASI 的 preopened dir 恰好对应 `docs/interfaces/filesystem.md` §7.2 所说的"请求上下文"（从哪个目录开始、最多能走多远）：普通 cwd、NT 相对目录、WASI preopened dir 复用同一套遍历机制。这条若走通，说明 FS 抽象**没有把 POSIX 的 `fd` / 进程语义漏进下层**。

## 10. 顺带解决 C / Rust SDK 的重复

接口定义**不属于 Rust，也不属于 C**。这正好接上刚落地的 KABI 工作：

```text
canonical schema  abi/*.toml
        ↓  tools/kabi/kabi_gen.py（make abi-gen / make abi-check）
C binding（os/components/kcomp-sdk/include/generated/…）  +  Rust binding
        ↓
第三方 C 库用 C binding；Rust 库用 Rust binding；两者都编译成 .kcomp
        ↓
Core 不关心它原本用什么语言写
```

- ABI 形状的**单一来源**是 `abi/*.toml`，生成器是 `tools/kabi/kabi_gen.py`，产物是 C 头与 Rust 模块（见 `docs/modules/components.md`、`docs/modules/core/generated.md`）。
- 因此"调一个 C 库"和"调一个 Rust 库"走的是同一套契约路径，adapter 只是选择用哪个语言的 binding。
- 接口契约的归属与导航见 `docs/interfaces/README.md`；新增接口应登记在那里，而不是复制一份会漂移的副本。

> 这条同时消掉了"C 组件要重写一套 / Rust 组件要重写一套"的重复：**接口是语言中立的，库的语言只是 adapter 的实现选择。**

## 11. 生态布局（方向，非现状）

理想形态大致如此（每个目录 ≈ 上游代码 + 几百行 KaleidOS adapter + component manifest）：

```text
components/
  fs-fatfs/  fs-lwext4/  fs-littlefs/
  net-smoltcp/  net-lwip/
  runtime-wamr/  runtime-lua/
  usb-tinyusb/
  crypto-mbedtls/
  libc-picolibc/
```

> **这是方向，不是现状。** 仓库当前是 `os/components/`（组件都在 `os/` 下，且没有这种扁平 `components/` 布局），目录命名也只是示意。现有布局见 `docs/modules/components.md`。

每个"调包组件"的内容大致是：

```text
<name>/
  上游源码（git submodule 或打包进来的源码）
  几百行 adapter（组件内，实现该库的宿主回调；见 §3）
  Kconfig / Cargo.toml / manifest（编译与打包声明）
```

## 12. 差异化（收束原则）

> KaleidOS 的优势不是"我有自己写的 ext4"，而是"给我一个设计得还算 portable 的 C / Rust library，我能很快把它变成系统 component"。

这句话反过来约束 Core：**Core 不因为某个库需要就多出一个功能**。库需要的都经 native semantic interface 表达；表达不了，就说明接口缺一块，去补接口，而不是把库拉进 Core。

## 13. 与其他文档的关系

- `docs/philosophy/core-philosophy.md`：默认外置、机制 vs 策略、什么进 Core 的判断标准（调包是这套原则在"功能"上的直接推论）；
- `docs/interfaces/filesystem.md`：Block / Namespace / File 的分层与多实例端点模型（§10、§11）；
- `docs/architecture/component-model.md`：`.kcomp` 是语言无关组件程序、third-party crate 是组件私有实现（§2.2、§2.3）；
- `docs/interfaces/README.md`：ABI / 接口契约的归属与导航；
- `docs/modules/components.md`：SDK、C 运行时、`.kcomp` 流水线现状；
- `docs/development/roadmap.md`：移植能力（porting）作为**方向**的登记处。
