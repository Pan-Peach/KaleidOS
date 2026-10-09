# components（os/components/ + tools/）

> 组件层：**组件 crates** + **`kcomp-sdk`（SDK / CRT）** + **`.kcomp` 构建 / 打包 / 加载流水线**。
> 组件是独立镜像与生命周期边界；`.kcomp` 是**语言无关**的组件二进制（ET_REL），不是 rustc `.o`。

## 组件 crates

实际目录（`os/components/`）：生产组件 —— `driver_prober`、`drivers/`、`filesystems/`、`kbench`、`kcomp-sdk`、`scheduler_rr`、`init`、`ksh`；test-only fixture 统一在 `tests/` —— `core_test`、`kcomp_c_smoke`、`kcomp_isolated`、`kcomp_isolated_bad`、`kcomp_isolated_life`、`kcomp_isolated_svc`、`kcomp_min`、`kcomp_panic`、`kcomp_smp`、`kcomp_checksum`、`kcomp_smoke`、`drivers/ram_blk`、`drivers/ram_blk_rw`；`Kconfig` 定义初始编排者 `BOOT_COMPONENT`。

**test-only 与生产的分界**：test-only fixture / 组件一律放 `os/components/tests/`；`.kcomp` 名取目录 basename（`load <basename>`），所以搬路径不改组件名，`load core_test` / `load kcomp_c_smoke` 等运行时契约不变。

| 组件 | 路径 | 形态 | 一句话 |
|---|---|---|---|
| `core_test` | `os/components/tests/core_test/` | Rust `.kcomp` | CoreTest 板内自检 + **唯一的组件/系统集成测试编排者**；只走 `kcore_*` 白名单（分组 boot / sched / resource / trace + 场景 filesystem / driver / c_frontend / smp） |
| `init` | `os/components/init/` | Rust `.kcomp` | 普通 profile 启动编排：scheduler / prober / FAT root / ksh；见 [`init.md`](init.md) |
| `ksh` | `os/components/ksh/` | Rust `.kcomp` | KernelNative 交互会话与文件读取；见 [`ksh.md`](ksh.md) |
| `scheduler_rr` | `os/components/scheduler_rr/` | Rust `.kcomp` | 轮转 `SchedulerPolicy` 参考实现；每 CPU cursor 保存上次提议的 TaskId，按候选 id 后继轮转，避免列表排除 outgoing 时的下标饥饿；游标属于实例，只提议 TaskId |
| `driver_prober` | `os/components/driver_prober/` | Rust `.kcomp` | opaque compatible 粗匹配；create config 下发 device/结果端口，成功后 pull probe.result；完成全部候选；同驱动/设备去重，已 Match 设备不交后续候选 |
| `virtio_blk` | `os/components/drivers/virtio_blk/` | Rust `.kcomp` | 每实例 claim 一个 VirtIO-MMIO 设备；sector 0 传输健康检查后发布 block.device/probe.result；不解释格式签名，RV32 LBA 溢出明确拒绝 |
| `fatfs` | `os/components/filesystems/fatfs/` | C `.kcomp` | 只读 FatFs + block.device diskio；root / 单段 lookup / node_info，ASCII 8.3；节点表含根共 8 槽，挂载期间驻留、卸载失效；实例级 try-lock 串行库状态/文件表/节点表，竞争 EBUSY、handle 与 node token 分别单调不复用；语义与 wire 见 [filesystem schema](../../abi/filesystem.toml) |
| `littlefs` | `os/components/filesystems/littlefs/` | C `.kcomp` | littlefs 文件系统服务（对外只读 `kcomp_filesystem_api`，实例级 try-lock，handle 不复用）；包 third_party `lfs.c` + `lfs_util.c`，`block.device` 适配（read/prog/erase/sync，erase = 整块写 0xFF）；mount 内 format+mount+自检（写读校验，走 prog/erase） |
| `vfs` | `os/components/filesystems/vfs/` | Rust `.kcomp` 骨架 | Namespace / File service 内部类型与操作占位；create 返回 `-ENOTSUP`，尚无服务 endpoint。现状与手写入口见 [`vfs.md`](vfs.md) |
| `posix` | `os/components/personalities/posix/` | Rust `.kcomp` | RV64/MMU 普通用户进程族，fork/exec/wait、console 与只读 posix.process endpoint 已接；通用 VFS/fd 仍骨架，见 [`posix.md`](posix.md) |
| `netstack` | `os/components/network/netstack/` | Rust `.kcomp` 骨架 | TCP / UDP 服务契约与 SDK 代理、私有 smoltcp / 帧 adapter / worker 占位；操作为 `todo!()`，bind / create 拒绝。见 [`netstack.md`](netstack.md) |
| `ram_blk_rw` | `os/components/tests/drivers/ram_blk_rw/` | Rust `.kcomp` | **可写、per-instance** RAM 块设备（`ram_blk` 的可写对偶）：每实例经 `kcore_memory_acquire` 取独立零初始化缓冲；Direct `ctx` 指向携带本实例 state 的 per-instance provider |
| `kcomp_smoke` | `os/components/tests/kcomp_smoke/` | Rust `.kcomp` | SDK 参考 smoke：经白名单打印 `[smoke] hex=<n>` |
| `kcomp_c_smoke` | `os/components/tests/kcomp_c_smoke/` | C `.kcomp` | 最小 freestanding C 组件：`#include "kcomp.h"` + SDK C 运行时 |
| `kcomp_panic` | `os/components/tests/kcomp_panic/` | Rust `.kcomp` | 在 create 里故意 panic，端到端验证 panic containment |
| `kcomp_checksum` | `os/components/tests/kcomp_checksum/` | Rust `.kcomp` | 同一 create 入口的 Passive / Active / Hybrid 与 Gate-only 准入探针；私有 C-layout mailbox / 原子同步；CoreTest 唯一编排，见 [测试指南](../development/testing.md#component-形态与-stop-准入实验) |
| `kcomp_smp` | `os/components/tests/kcomp_smp/` | Rust `.kcomp` | CoreTest 的独立任务 panic 被测对象；普通 SMP 任务与所有断言在 core_test 内 |
| `kcomp_isolated` | `os/components/tests/kcomp_isolated/` | Rust `.kcomp` | **零依赖 / 零 import** 的 ArchTest fixture：text/rodata/data/bss + 控制页协议，供按域装载与页级权限强制用例在私有 AS 里执行 |
| `kcomp_isolated_bad` | `os/components/tests/kcomp_isolated_bad/` | Rust `.kcomp` | **放段失败** fixture：合法 `.kcomp`（过 packer 契约 + import 白名单）但带一个 17 MiB 零初始化段，超出按域装载的实例镜像窗口 → `isolated_load::place` 显式拒绝（`SegmentOutsideWindow`），供 `isolated-load-reject` 证明「放段失败在声明组件 / 创建 AS 之前」 |
| `kcomp_isolated_life` | `os/components/tests/kcomp_isolated_life/` | Rust `.kcomp` | **零依赖 / 零 import** 的 ArchTest fixture：实现实例窗口协议（读 args / 写 `out_state` 上报 tp / satp / config；destroy 写标记），供 `isolated-lifecycle` / `isolated-lifecycle-fail` / `isolated-lifecycle-fault` / `isolated-config-reject` / `isolated-prepare-reject` / `isolated-destroy-fault` / `isolated-restart` 经生产生命周期创建 / 销毁。故障注入：`FAIL_ABI`（create 返回 `-EINVAL`）、`FAULT_ABI`（create trap）、`DESTROY_FAULT_ABI`（create 成功、destroy trap）与 destroy 进入计数（`isolated-destroy-fault` 的「绝不重试析构」证据） |
| `kcomp_isolated_svc` | `os/components/tests/kcomp_isolated_svc/` | Rust `.kcomp` | **零依赖 / 零 import** 的 ArchTest fixture：Isolated 服务 provider。create 把 `out_state` 指向上报区；`kcomp_service_dispatch` 记录 Core 直接交付的 caller 帧（port / method / frame / args / input / output / tp / satp）、按 method 回显（echo）或对未映射 VA 注入缺页（fault），供 `isolated-service` / `isolated-service-fault` / `isolated-stale-access` / `isolated-ready-fault` 证明跨域 Gate、stale 阻断与重新 instantiate（重启） |
| `kcomp_min` | `os/components/tests/kcomp_min/` | Rust staticlib（host fixture） | 手写最小生命周期入口，供显式 host fixture 测试钉重定位布局；**不在 `KCOMP_SRCS`** |
| `kbench` | `os/components/kbench/` | Rust `.kcomp` | 板端 benchmark：clock/query + 真实 `sched.yield_roundtrip` 交接 |
| `kcomp-sdk` | `os/components/kcomp-sdk/` | Rust lib + C 头 / CRT | 组件 SDK/CRT，**不是可加载组件**（见下） |

## `kcomp-sdk`

路径 `os/components/kcomp-sdk/`（连字符；package `kcomp-sdk`；独立 workspace，被根 workspace `exclude`）。它**随每个 `.kcomp` 私有携带**，不是 shared runtime。

- **C 作者面**：`include/kcomp.h`（umbrella，只 include 生成物 + 契约说明）、`include/generated/kcomp_abi.h`（`kcore_*` / 生命周期入口 / `block.device` / `filesystem` 的 C 声明，schema 单一来源）、`include/errno.h`、`include/string.h`、`include/inttypes.h`（freestanding shim 声明；`inttypes.h` 因 littlefs 的 `lfs_util.h` 无条件 include 它而补）。
- **C 运行时**：`c/kcomp_rt.c`——freestanding **weak** `memcpy` / `memset` / `memmove` / `memcmp` / `strlen` / `strchr` / `strcpy` / `strspn` / `strcspn`（C 组件私有携带；只实现组件真正引用到的原语，不朝 libc 扩张；后三个为 littlefs 引入）。
- **Rust 面**（`src/`）：`lib.rs`（`kcomp_instance_create!` / `kcomp_instance_destroy!` / `kcomp_services!` / `klog!` 宏 + 重导出）、`abi.rs`（`kcore_*` facade）、`binding.rs`（typed service binding）、`endpoint.rs`（typed `Endpoint<C>`）、`block.rs`（`block.device` 契约类型 + provider 包装，声明本体 re-export 生成物）、`filesystem.rs`、`probe.rs`（`DriverCreateConfig` 扁平编解码 / `ProbeReply` / `ProbeResult` 契约 + pull / publish helper）、`generated/{abi,block,filesystem,errno,probe}.rs`（schema 生成物）、`dma.rs`（`DmaDirection`）、`errno.rs`（`Errno` / `Result`）、`logging.rs`、`panic.rs`（组件私有 `#[panic_handler]`）、`alloc.rs`（feature `alloc` 的 `GlobalAlloc` → per-image 部署 adapter，create 前选择 K 共享堆 / I 私有堆）、`heap.rs`（私有执行域的 freestanding C 分配器 facade；`c/kcomp_heap_runtime.c` 生产消费，host 测试驱动真实 C 实现；契约见 `docs/architecture/memory-and-heap.md` §6）。
- **ABI 目标**：稳定窄 C ABI（`kcore_*` 白名单）；target `riscv64gc-unknown-none-elf` / `riscv32imac-unknown-none-elf`。Rust ABI 永不成为组件 ABI。

## `.kcomp` 流水线（端到端）

```text
构建（语言前端）
  Rust: tools/build-kcomp.sh <dir> <target> <out>   → cargo staticlib → kcomp-link.sh
  C:    tools/build-kcomp-c.sh <dir> <target> <out> → clang freestanding .o → kcomp-link.sh
打包器（语言无关）
  tools/kcomp-link.sh <out> <input.o|.a>...
    rust-lld -r --gc-sections --no-relax（-u 三个生命周期符号）
    llvm-objcopy strip-debug / 去 .llvmbc / .llvmcmd
    契约校验：ET_REL；create/destroy/kcomp_abi DEFINED；UNDEF 仅 kcore_*；重定位白名单
打包
  make init.kpkg → $(O)/components/<n>.kcomp → scripts/build/package.py → $(O)/init.kpkg
内嵌
  boot 以 include_bytes! 收进 .initpkg 段（__initpkg_start / __initpkg_end）
加载
  store（cpio newc 解析）→ loader（段放置 + 重定位 + 入口校验）→ registry（生命周期，`ComponentRecord` 直接持有 loaded）
```

- **导出白名单**：`abi/core.toml` 声明 **61** 项 `kcore_*`；实现与解析在 `os/core/src/component/export.rs`、`export/{query,user}.rs` + 生成的 `component/generated/exports.rs`。打包时按前缀校验（`UNDEF` 必须以 `kcore_` 开头），加载时精确名解析；未导出符号 → `UnresolvedSymbol`，整次加载失败。Isolated 另有更窄的 import 支持面（19 项，包含诊断、只读查询、memory 和 endpoint 操作；精确名单见 [部署契约](../architecture/deployment.md)），装载前拒绝其余符号。
- **构建列表真相**：[mk/components.mk](../../mk/components.mk) 维护生产、guest test 与 host fixture 库存；`CONFIG_TEST_COMPONENTS` 决定是否加入测试组件。完整列表见该文件，`.kcomp` 名取目录 basename（`load <basename>`）。SandboxedNative 是 Core 执行域，不是构建列表中的组件。

## 测试 / smoke vs 真实组件

- **真实策略 / 服务 / 驱动**：`init`（启动编排）、`ksh`（会话）、`scheduler_rr`（policy）、`driver_prober`（service）、`drivers/virtio_blk`（driver）、`filesystems/fatfs`（service）。
- **测试 / smoke / 基准**：`core_test`（CoreTest **权威**：唯一的组件 / 系统集成测试编排者，见 [testing.md §3](../development/testing.md)，但仍只是 test-only 镜像，故与 fixture 同放 `tests/`）、`kcomp_smoke`、`kcomp_c_smoke`、`kcomp_panic`、`kcomp_isolated`（按域装载 fixture）、`kcomp_isolated_life`（Isolated 生命周期 / destroy 故障注入 fixture）、`kcomp_isolated_svc`（跨域服务 provider fixture）、`kcomp_isolated_bad`（放段失败 fixture）、`kcomp_min`（host fixture）——全部在 `os/components/tests/`；`kbench`（度量）留在生产位置。

## 明确不做

- **不建 shared Rust runtime**：每个 `.kcomp` 私有携带自己的 Rust 支撑；组件可链接的外部符号只有 `kcore_*` 白名单。
- **组件间不互链 flat ELF 符号**：只经 Interface binding（见 [`core/component.md`](core/component.md)）。
- loader 不是 Rust dynamic linker：只做段放置 + 对白名单 `kcore_*` 的重定位。
- 不做组件热插拔 / 依赖解析器 / 自动 ABI 兼容协商（推迟）。

## 代码在哪

| 路径 | 内容 |
|---|---|
| `os/components/<name>/` | 各生产组件 crate（见上表）；test-only fixture 在 `os/components/tests/` |
| `os/components/kcomp-sdk/` | SDK / CRT：`include/`（C 头）、`c/kcomp_rt.c`、`src/`（Rust） |
| `tools/build-kcomp.sh` / `tools/build-kcomp-c.sh` | Rust / C 语言前端 |
| `tools/kcomp-link.sh` | 语言无关打包器 + 契约校验 |
| `tools/kabi/kabi_gen.py` | ABI schema 生成器（源 `abi/*.toml`） |
| `os/core/src/component/{store,loader,registry,export}.rs` | store / loader / registry / 导出白名单 |
| `scripts/build/package.py` | 组件构建与 newc 打包；boot 经 KALEIDOS_INITPKG 内嵌 |
| `os/core/build.rs` | 转发 trace 容量与 bench commit，不构建 fixture |

执行域可移植性由 `tests/kcomp_heap` 与 `tests/kcomp_domain_service` 两个真实工件验证：前者覆盖 K/I 的 C/Rust 分配，后者通过相同 SDK `block.device` provider/consumer 覆盖 K/K、K/I、I/K、I/I，含跨页缓冲、嵌套、panic/stale 与循环重入。它们只验证堆和扁平服务，不代表 Isolated 硬件驱动或任务已可用。
