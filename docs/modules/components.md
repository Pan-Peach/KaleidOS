# components（os/components/ + tools/）

> 组件层：**组件 crates** + **`kcomp-sdk`（SDK / CRT）** + **`.kcomp` 构建 / 打包 / 加载流水线**。
> 组件是热插拔边界；`.kcomp` 是**语言无关**的组件二进制（ET_REL），不是 rustc `.o`。

## 组件 crates

实际目录（`os/components/`）：`block_chain`、`core_test`、`driver_prober`、`drivers/`、`filesystems/`、`kbench`、`kcomp_c_smoke`、`kcomp_min`、`kcomp_panic`、`kcomp-sdk`、`kcomp_smoke`、`Kconfig`、`littlefs_chain`、`logger`、`scheduler_rr`。

| 组件 | 路径 | 形态 | 一句话 |
|---|---|---|---|
| `core_test` | `os/components/core_test/` | Rust `.kcomp` | CoreTest 板内自检；只走 `kcore_*` 白名单（分组 boot / sched / resource / trace） |
| `scheduler_rr` | `os/components/scheduler_rr/` | Rust `.kcomp` | 轮转 `SchedulerPolicy` 参考实现；cursor 是实例状态，只提议下一个 `TaskId` |
| `driver_prober` | `os/components/driver_prober/` | Rust `.kcomp` | 协议无关设备 prober（总线角色）：opaque compatible 粗匹配；逐台以扁平 create config 下发 `(device_id, 结果端口名)`，create 返回后 pull 驱动的 `probe.result`，本地更新 cursor（**无环**，driver 不回调） |
| `kcomp_virtio_blk` | `os/components/drivers/virtio_blk/` | Rust `.kcomp` | VirtIO-MMIO 块驱动；从 create config 读 assignment、claim 设备、细匹配；发布 `block.device` 与 `probe.result`（单设备限制见其模块文档） |
| `fatfs` | `os/components/filesystems/fatfs/` | C `.kcomp` | 只读 FatFs 文件系统服务（`kcomp_filesystem_api`），包 third_party `ff.c` + `block.device` diskio |
| `littlefs` | `os/components/filesystems/littlefs/` | C `.kcomp` | littlefs 文件系统服务（对外只读 `kcomp_filesystem_api`）；包 third_party `lfs.c` + `lfs_util.c`，`block.device` 适配（read/prog/erase/sync，erase = 整块写 0xFF）；mount 内 format+mount+自检（写读校验，走 prog/erase） |
| `ram_blk_rw` | `os/components/drivers/ram_blk_rw/` | Rust `.kcomp` | **可写、per-instance** RAM 块设备（`ram_blk` 的可写对偶）：每实例经 `kcore_memory_acquire` 取独立零初始化缓冲；Direct `ctx` 指向携带本实例 state 的 per-instance provider |
| `littlefs_chain` | `os/components/littlefs_chain/` | Rust `.kcomp` | **多实例组合策略**：2× `ram_blk_rw` → 2× `littlefs`（各带独立块设备与 `lfs_t`），证明 Core endpoint/instance 模型承载两个互不干扰的 FS 实例 |
| `vfs` | `os/components/filesystems/vfs/` | 空目录 | 占位，无文件、无 `Cargo.toml` |
| `kcomp_smoke` | `os/components/kcomp_smoke/` | Rust `.kcomp` | SDK 参考 smoke：经白名单打印 `[smoke] hex=<n>` |
| `kcomp_c_smoke` | `os/components/kcomp_c_smoke/` | C `.kcomp` | 最小 freestanding C 组件：`#include "kcomp.h"` + SDK C 运行时 |
| `kcomp_panic` | `os/components/kcomp_panic/` | Rust `.kcomp` | 在 create 里故意 panic，端到端验证 panic containment |
| `kcomp_min` | `os/components/kcomp_min/` | Rust staticlib（host fixture） | 手写最小生命周期入口，供 `os/core/build.rs` host 测试钉重定位布局；**不在 `KCOMP_SRCS`** |
| `kbench` | `os/components/kbench/` | Rust `.kcomp` | 板端 benchmark：clock/query + 真实 `sched.yield_roundtrip` 交接 |
| `logger` | `os/components/logger/` | Rust lib（workspace 成员） | 结构化日志组件 stub（M2），无 `.kcomp`、无实现 |
| `kcomp-sdk` | `os/components/kcomp-sdk/` | Rust lib + C 头 / CRT | 组件 SDK/CRT，**不是可加载组件**（见下） |

## `kcomp-sdk`

路径 `os/components/kcomp-sdk/`（连字符；package `kcomp-sdk`；独立 workspace，被根 workspace `exclude`）。它**随每个 `.kcomp` 私有携带**，不是 shared runtime。

- **C 作者面**：`include/kcomp.h`（umbrella，只 include 生成物 + 契约说明）、`include/generated/kcomp_abi.h`（`kcore_*` / 生命周期入口 / `block.device` / `filesystem` 的 C 声明，schema 单一来源）、`include/errno.h`、`include/string.h`、`include/inttypes.h`（freestanding shim 声明；`inttypes.h` 因 littlefs 的 `lfs_util.h` 无条件 include 它而补）。
- **C 运行时**：`c/kcomp_rt.c`——freestanding **weak** `memcpy` / `memset` / `memmove` / `memcmp` / `strlen` / `strchr` / `strcpy` / `strspn` / `strcspn`（C 组件私有携带；只实现组件真正引用到的原语，不朝 libc 扩张；后三个为 littlefs 引入）。
- **Rust 面**（`src/`）：`lib.rs`（`kcomp_instance_create!` / `kcomp_instance_destroy!` / `kcomp_services!` / `klog!` 宏 + 重导出）、`abi.rs`（`kcore_*` facade）、`binding.rs`（typed service binding）、`endpoint.rs`（typed `Endpoint<C>`）、`block.rs`（`block.device` 契约类型 + provider 包装，声明本体 re-export 生成物）、`filesystem.rs`、`probe.rs`（`DriverCreateConfig` 扁平编解码 / `ProbeReply` / `ProbeResult` 契约 + pull / publish helper）、`generated/{abi,block,filesystem,errno,probe}.rs`（schema 生成物）、`dma.rs`（`DmaDirection`）、`errno.rs`（`Errno` / `Result`）、`logging.rs`、`panic.rs`（组件私有 `#[panic_handler]`）、`alloc.rs`（feature `alloc` 的 `GlobalAlloc` → Core 共享堆）。
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
  store（cpio newc 解析）→ loader（段放置 + 重定位 + 入口校验）→ registry（生命周期）→ image（常驻镜像）
```

- **导出白名单**：`abi/core.toml` 声明 **38** 项 `kcore_*`；实现与解析在 `os/core/src/component/export.rs` + 生成的 `component/generated/exports.rs`。打包时按前缀校验（`UNDEF` 必须以 `kcore_` 开头），加载时精确名解析；未导出符号 → `UnresolvedSymbol`，整次加载失败。
- **构建列表真相**：`Makefile` 的 `KCOMP_SRCS`（Rust：`core_test kcomp_smoke scheduler_rr kcomp_panic drivers/virtio_blk driver_prober kbench drivers/ram_blk drivers/ram_blk_rw block_chain littlefs_chain`）与 `KCOMP_C_SRCS`（C：`kcomp_c_smoke filesystems/fatfs filesystems/littlefs filesystems/fs_consumer`）。

## 测试 / smoke vs 真实组件

- **真实策略 / 服务 / 驱动**：`scheduler_rr`（policy）、`driver_prober`（service）、`drivers/virtio_blk`（driver）、`filesystems/fatfs`（service）；`logger` 是尚未实现的真实服务 stub。
- **测试 / smoke / 基准**：`core_test`、`kcomp_smoke`、`kcomp_c_smoke`、`kcomp_panic`、`kcomp_min`（host fixture）、`kbench`（度量）。
- `filesystems/vfs` 是空占位，两者都不是。

## 明确不做

- **不建 shared Rust runtime**：每个 `.kcomp` 私有携带自己的 Rust 支撑；组件可链接的外部符号只有 `kcore_*` 白名单。
- **组件间不互链 flat ELF 符号**：只经 Interface binding（见 [`core/component.md`](core/component.md)）。
- loader 不是 Rust dynamic linker：只做段放置 + 对白名单 `kcore_*` 的重定位。
- 不做组件热插拔 / 依赖解析器 / 自动 ABI 兼容协商（推迟）。

## 代码在哪

| 路径 | 内容 |
|---|---|
| `os/components/<name>/` | 各组件 crate（见上表） |
| `os/components/kcomp-sdk/` | SDK / CRT：`include/`（C 头）、`c/kcomp_rt.c`、`src/`（Rust） |
| `tools/build-kcomp.sh` / `tools/build-kcomp-c.sh` | Rust / C 语言前端 |
| `tools/kcomp-link.sh` | 语言无关打包器 + 契约校验 |
| `tools/kabi/kabi_gen.py` | ABI schema 生成器（源 `abi/*.toml`） |
| `os/core/src/component/{store,loader,registry,image,export}.rs` | store / loader / registry / image / 导出白名单 |
| `os/core/build.rs` | host fixture 组件构建 + `.initpkg` 内嵌 |
