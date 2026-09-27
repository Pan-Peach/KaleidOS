# components（os/components/ + tools/）

> 组件层：**组件 crates** + **`kcomp-sdk`（SDK / CRT）** + **`.kcomp` 构建 / 打包 / 加载流水线**。
> 组件是热插拔边界；`.kcomp` 是**语言无关**的组件二进制（ET_REL），不是 rustc `.o`。

## 组件 crates

实际目录（`os/components/`）：生产组件 —— `driver_prober`、`drivers/`、`filesystems/`、`kbench`、`kcomp-sdk`、`scheduler_rr`；test-only fixture 统一在 `tests/` —— `core_test`、`kcomp_c_smoke`、`kcomp_isolated`、`kcomp_isolated_bad`、`kcomp_isolated_life`、`kcomp_isolated_svc`、`kcomp_min`、`kcomp_panic`、`kcomp_smoke`、`drivers/ram_blk`、`drivers/ram_blk_rw`；`Kconfig` 是组件选择扩展点（当前无符号）。

**test-only 与生产的分界**：test-only fixture / 组件一律放 `os/components/tests/`；`.kcomp` 名取目录 basename（`load <basename>`），所以搬路径不改组件名，`load core_test` / `load kcomp_c_smoke` 等运行时契约不变。

| 组件 | 路径 | 形态 | 一句话 |
|---|---|---|---|
| `core_test` | `os/components/tests/core_test/` | Rust `.kcomp` | CoreTest 板内自检 + **唯一的组件/系统集成编排者**；只走 `kcore_*` 白名单（分组 boot / sched / resource / trace + 场景 filesystem / driver / c_frontend） |
| `scheduler_rr` | `os/components/scheduler_rr/` | Rust `.kcomp` | 轮转 `SchedulerPolicy` 参考实现；cursor 是实例状态，只提议下一个 `TaskId` |
| `driver_prober` | `os/components/driver_prober/` | Rust `.kcomp` | 协议无关设备 prober（总线角色）：opaque compatible 粗匹配；逐台以扁平 create config 下发 `(device_id, 结果端口名)`，create 返回后 pull 驱动的 `probe.result`，本地更新 cursor（**无环**，driver 不回调） |
| `kcomp_virtio_blk` | `os/components/drivers/virtio_blk/` | Rust `.kcomp` | VirtIO-MMIO 块驱动；从 create config 读 assignment、claim 设备、细匹配；发布 `block.device` 与 `probe.result`（单设备限制见其模块文档） |
| `fatfs` | `os/components/filesystems/fatfs/` | C `.kcomp` | 只读 FatFs 文件系统服务（`kcomp_filesystem_api`），包 third_party `ff.c` + `block.device` diskio |
| `littlefs` | `os/components/filesystems/littlefs/` | C `.kcomp` | littlefs 文件系统服务（对外只读 `kcomp_filesystem_api`）；包 third_party `lfs.c` + `lfs_util.c`，`block.device` 适配（read/prog/erase/sync，erase = 整块写 0xFF）；mount 内 format+mount+自检（写读校验，走 prog/erase） |
| `ram_blk_rw` | `os/components/tests/drivers/ram_blk_rw/` | Rust `.kcomp` | **可写、per-instance** RAM 块设备（`ram_blk` 的可写对偶）：每实例经 `kcore_memory_acquire` 取独立零初始化缓冲；Direct `ctx` 指向携带本实例 state 的 per-instance provider |
| `kcomp_smoke` | `os/components/tests/kcomp_smoke/` | Rust `.kcomp` | SDK 参考 smoke：经白名单打印 `[smoke] hex=<n>` |
| `kcomp_c_smoke` | `os/components/tests/kcomp_c_smoke/` | C `.kcomp` | 最小 freestanding C 组件：`#include "kcomp.h"` + SDK C 运行时 |
| `kcomp_panic` | `os/components/tests/kcomp_panic/` | Rust `.kcomp` | 在 create 里故意 panic，端到端验证 panic containment |
| `kcomp_isolated` | `os/components/tests/kcomp_isolated/` | Rust `.kcomp` | **零依赖 / 零 import** 的 ArchTest fixture：text/rodata/data/bss + 控制页协议，供按域装载与页级权限强制用例在私有 AS 里执行 |
| `kcomp_isolated_bad` | `os/components/tests/kcomp_isolated_bad/` | Rust `.kcomp` | **放段失败** fixture：合法 `.kcomp`（过 packer 契约 + import 包络）但带一个 17 MiB 零初始化段，超出按域装载的实例镜像窗口 → `isolated_load::place` 显式拒绝（`SegmentOutsideWindow`），供 `isolated-load-reject` 证明「放段失败在声明组件 / 创建 AS / 登记之前」 |
| `kcomp_isolated_life` | `os/components/tests/kcomp_isolated_life/` | Rust `.kcomp` | **零依赖 / 零 import** 的 ArchTest fixture：实现实例窗口协议（读 args / 写 `out_state` 上报 tp / satp / config；destroy 写标记），供 `isolated-lifecycle` / `isolated-lifecycle-fail` / `isolated-lifecycle-fault` / `isolated-config-reject` / `isolated-prepare-reject` / `isolated-destroy-fault` / `isolated-restart` 经生产生命周期创建 / 销毁。故障注入：`FAIL_ABI`（create 返回 `-EINVAL`）、`FAULT_ABI`（create trap）、`DESTROY_FAULT_ABI`（create 成功、destroy trap）与 destroy 进入计数（`isolated-destroy-fault` 的「绝不重试析构」证据） |
| `kcomp_isolated_svc` | `os/components/tests/kcomp_isolated_svc/` | Rust `.kcomp` | **零依赖 / 零 import** 的 ArchTest fixture：Isolated 服务 provider。create 把 `out_state` 指向上报区；`kcomp_service_dispatch` 记录 Core 交付的邮箱帧（port / method / frame / args / input / output / tp / satp）、按 method 回显（echo）或对 caller 域地址注入缺页（fault），供 `isolated-service` / `isolated-service-limits` / `isolated-service-fault` / `isolated-stale-access` / `isolated-ready-fault` 证明跨域 Gate、stale 阻断与重新 instantiate（重启） |
| `kcomp_min` | `os/components/tests/kcomp_min/` | Rust staticlib（host fixture） | 手写最小生命周期入口，供 `os/core/build.rs` host 测试钉重定位布局；**不在 `KCOMP_SRCS`** |
| `kbench` | `os/components/kbench/` | Rust `.kcomp` | 板端 benchmark：clock/query + 真实 `sched.yield_roundtrip` 交接 |
| `kcomp-sdk` | `os/components/kcomp-sdk/` | Rust lib + C 头 / CRT | 组件 SDK/CRT，**不是可加载组件**（见下） |

## `kcomp-sdk`

路径 `os/components/kcomp-sdk/`（连字符；package `kcomp-sdk`；独立 workspace，被根 workspace `exclude`）。它**随每个 `.kcomp` 私有携带**，不是 shared runtime。

- **C 作者面**：`include/kcomp.h`（umbrella，只 include 生成物 + 契约说明）、`include/generated/kcomp_abi.h`（`kcore_*` / 生命周期入口 / `block.device` / `filesystem` 的 C 声明，schema 单一来源）、`include/errno.h`、`include/string.h`、`include/inttypes.h`（freestanding shim 声明；`inttypes.h` 因 littlefs 的 `lfs_util.h` 无条件 include 它而补）。
- **C 运行时**：`c/kcomp_rt.c`——freestanding **weak** `memcpy` / `memset` / `memmove` / `memcmp` / `strlen` / `strchr` / `strcpy` / `strspn` / `strcspn`（C 组件私有携带；只实现组件真正引用到的原语，不朝 libc 扩张；后三个为 littlefs 引入）。
- **Rust 面**（`src/`）：`lib.rs`（`kcomp_instance_create!` / `kcomp_instance_destroy!` / `kcomp_services!` / `klog!` 宏 + 重导出）、`abi.rs`（`kcore_*` facade）、`binding.rs`（typed service binding）、`endpoint.rs`（typed `Endpoint<C>`）、`block.rs`（`block.device` 契约类型 + provider 包装，声明本体 re-export 生成物）、`filesystem.rs`、`probe.rs`（`DriverCreateConfig` 扁平编解码 / `ProbeReply` / `ProbeResult` 契约 + pull / publish helper）、`generated/{abi,block,filesystem,errno,probe}.rs`（schema 生成物）、`dma.rs`（`DmaDirection`）、`errno.rs`（`Errno` / `Result`）、`logging.rs`、`panic.rs`（组件私有 `#[panic_handler]`）、`alloc.rs`（feature `alloc` 的 `GlobalAlloc` → 本实例 `HeapState`，即 per-instance 堆，非 Core 共享堆；契约见 `docs/architecture/memory-and-heap.md`）。
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
  make init.kpkg → build/kpkg/<n>.kcomp + manifest → cpio -H newc > tools/qemu/init.kpkg
内嵌
  boot 以 include_bytes! 收进 .initpkg 段（__initpkg_start / __initpkg_end）
加载
  store（cpio newc 解析）→ loader（段放置 + 重定位 + 入口校验）→ registry（生命周期，`ComponentRecord` 直接持有 loaded）
```

- **导出白名单**：`abi/core.toml` 声明 **40** 项 `kcore_*`；实现与解析在 `os/core/src/component/export.rs` + 生成的 `component/generated/exports.rs`。打包时按前缀校验（`UNDEF` 必须以 `kcore_` 开头），加载时精确名解析；未导出符号 → `UnresolvedSymbol`，整次加载失败。
- **构建列表真相**：`Makefile` 的 `KCOMP_SRCS`（Rust：`tests/core_test tests/kcomp_smoke scheduler_rr tests/kcomp_panic tests/kcomp_isolated tests/kcomp_isolated_life tests/kcomp_isolated_svc tests/kcomp_isolated_bad drivers/virtio_blk driver_prober kbench tests/drivers/ram_blk tests/drivers/ram_blk_rw`）与 `KCOMP_C_SRCS`（C：`tests/kcomp_c_smoke filesystems/fatfs filesystems/littlefs`）；`.kcomp` 名取目录 basename（`load <basename>`）。

## 测试 / smoke vs 真实组件

- **真实策略 / 服务 / 驱动**：`scheduler_rr`（policy）、`driver_prober`（service）、`drivers/virtio_blk`（driver）、`filesystems/fatfs`（service）。
- **测试 / smoke / 基准**：`core_test`（CoreTest **权威**：唯一的组件 / 系统集成编排者，见 [testing.md §3](../development/testing.md)，但仍只是 test-only 镜像，故与 fixture 同放 `tests/`）、`kcomp_smoke`、`kcomp_c_smoke`、`kcomp_panic`、`kcomp_isolated`（按域装载 fixture）、`kcomp_isolated_life`（Isolated 生命周期 / destroy 故障注入 fixture）、`kcomp_isolated_svc`（跨域服务 provider fixture）、`kcomp_isolated_bad`（放段失败 fixture）、`kcomp_min`（host fixture）——全部在 `os/components/tests/`；`kbench`（度量）留在生产位置。

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
| `os/core/build.rs` | host fixture 组件构建 + `.initpkg` 内嵌 |
