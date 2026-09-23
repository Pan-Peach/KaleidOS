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

## 3. `kport` 层

提议一个正式层，暂名 `kport`（落地位置二选一：`kcomp-sdk/kport`，或独立仓库 `kaleidos-port`）。形态：

```text
third-party library
      ↓  （library native API，例如 disk_read / disk_write / disk_ioctl）
KaleidOS adapter
      ↓
native semantic interface（BlockDevice / NetDevice / Clock / File / Random / Socket）
```

`kport` 不是 runtime，不是 sandbox，也不是一个新的抽象王座。它是一组**适应层 + 约定**：把"库期望的宿主接口"接到"KaleidOS 的 native semantic interface"。

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

| 功能 | 可调包 | KaleidOS 需要自己写什么 | 状态 |
|---|---|---|---|
| FAT / exFAT | FatFs | `disk_read` / `disk_write` / `disk_ioctl` → `BlockDevice` | **已落地**（只读 kcomp 服务，见下） |
| ext2/3/4 | lwext4 | block device adapter | 候选 |
| MCU flash FS | littlefs | block read / program / erase adapter | 候选 |
| TCP/IP（Rust） | smoltcp | `NetDevice` + clock adapter | 候选 |
| TCP/IP（C） | lwIP | `netif` + timer / OS adapter | 候选 |
| TLS | Mbed TLS | RNG + clock + socket adapter | 候选 |
| USB | TinyUSB | controller / HAL + 同步原语 | 候选 |
| WASI / Wasm runtime | WAMR | WASI host calls → KaleidOS services | 候选 |
| C libc | picolibc | 极小的 OS glue layer | 候选（见 §6） |

> **已落地的唯一先例是 FatFs。** 它已作为**只读**文件系统服务集成：`os/components/filesystems/fatfs/` 是 C `.kcomp`，包上游 `ff.c` 加一个 `block.device` diskio，向上发布最小 `filesystem` 服务契约（不透明 handle、singleton 端点名、自带 selftest）。现状与未决问题以 `docs/interfaces/filesystem.md` §11 为准。
> **其余全部是候选，不是集成状态。** 本文件不声明任何候选库已被接入，也不写具体上游版本号（版本无关紧要，架构契合度才决定能否调）。

## 5. 实例模型对齐：littlefs 与 Endpoint / Instance

littlefs 把文件系统状态放在**调用者分配的 `lfs_t`** 里，因此同一份代码天然支持**同时挂载多个文件系统**。这与 KaleidOS 的 **Component Definition → Component Instance** 几乎一一对应：

```text
lfs_t #1 / #2 / #3     →     FsInstance #1 / #2 / #3
（调用者分配，互不干扰）        （实例身份 + 私有状态 + 独立 storage）
```

这解释了为什么"实例模型"值得先做：`lfs_t` 这种"状态由调用者拥有"的设计，正是多实例 FS 想要的形状。

> **诚实的前提**：当前 Interface Registry 对每个接口**名字**只保留一个 provider 槽位，所以"多个同类型 FS 实例同时发布并各自被 bind"**现在做不到**，需要先做 endpoint / instance 模型。见 `docs/interfaces/filesystem.md` §10 及其未决问题（第 3、4 条）。在实例模型落地之前，多实例只是设计目标，**不得宣称已支持**。

## 6. C runtime：不要手写 libc

不要靠不断长大 `kcomp-libc` 来满足库的需求。正确方向是调 **picolibc**：它按 32 / 64 位 embedded 设计，有明确的 OS-support 层，stdio 底部接口很窄，正好落在第一档（§2）。

现状（务必按此描述）：

- SDK 目前只提供 **freestanding** 的 `memcpy` / `memset` / `memmove` / `memcmp` / `strlen` / `strchr`（`os/components/kcomp-sdk/c/kcomp_rt.c`，**weak** 定义），以及 `<string.h>` / `<errno.h>` shim；
- 它刻意只实现组件真正引用到的原语，**不朝 libc 扩张**（见 `docs/architecture/component-model.md` §2.3 与 `docs/modules/components.md`）；
- 真正 libc 是**未来移植目标**，不是现状。

> 判据：当第三个库开始抱怨"少了个 `malloc` / `snprintf`"时，答案是**移植 picolibc**，不是往 `kcomp_rt.c` 里再塞一个函数。

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
     Port Layer
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
  几百行 adapter（kport 层，实现该库的宿主回调）
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
- `docs/development/roadmap.md`：移植平台（kport）作为**方向**的登记处。
