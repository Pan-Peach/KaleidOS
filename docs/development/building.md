# 构建与运行

构建入口是根 Makefile；配置契约见 [Kconfig](../architecture/kconfig.md)。

## 工具

当前默认开发与 CI 使用 Linux。

| 工具 | 使用位置 |
|---|---|
| Rust / Cargo，RV64 与 RV32 裸机 target | Core、boot 与 Rust 组件 |
| rustfmt / clippy | 格式与 lint |
| Python 3、Make、Bash、Git | 配置、打包与测试 |
| Clang、llvm-objcopy、llvm-readelf | C 组件、`.kcomp` 链接与校验 |
| rust-lld（取 Rust sysroot） | `.kcomp` partial link |
| riscv64-unknown-elf-gcc | RV64 用户 ELF 测试工件 |
| qemu-system-riscv64 / qemu-system-riscv32 | 启动与硬件测试 |
| mkfs.vfat、mcopy（dosfstools / mtools） | FAT 根盘与 init 流程 |

文件系统 provider 的 host 测试同样需要 `dosfstools` / `mtools`。
RV32 的 `-bios default` 需要 QEMU 固件目录中的
`opensbi-riscv32-generic-fw_dynamic.bin`。Ubuntu 的 QEMU 包可能缺少它；
CI 使用 `scripts/ci/install-rv32-firmware.sh` 下载 QEMU v8.2.2 随附的
OpenSBI 镜像并校验 SHA-256。本地 Ubuntu 缺少该固件时，可在仓库根目录运行
`sudo bash scripts/ci/install-rv32-firmware.sh /usr/share/qemu`。

```sh
git submodule update --init --recursive
rustup target add riscv64gc-unknown-none-elf riscv32imac-unknown-none-elf
rustup component add rustfmt clippy
```

Kconfiglib 来自固定版本的 submodule。普通 Core build 不构建测试工件。
`make test-host` 显式准备真实 `.kcomp`，因此仍需要 RV64 target 与链接工具；
只测 Core 纯逻辑可直接运行 `cargo test -p kernel --lib`。

## 选择配置并启动

```sh
make qemu_rv64_defconfig
make qemu
```

默认启动 init → 驱动 / FAT 根盘 → ksh。`help` 查看命令，
`cat 0:/HELLO.TXT` 读取示例，`exit` 回到 monitor；退出 QEMU 按 `Ctrl-A`、`X`。
RV32 用 `make qemu_rv32_defconfig`。RV32 NoMMU 和其他 ISA 不属于默认回归矩阵，
具体范围见 [Arch](../modules/arch.md) 与 [boot](../modules/boot.md)。

`make menuconfig` 编辑当前配置，`make olddefconfig` 补齐默认值。
配置与构建分两次执行，不能合成 `make qemu_rv64_defconfig kernel`。

## 输出目录

默认使用根 `.config`，产物写入 `build/default/`。
`O=` 同时选择独立的配置与输出目录，便于交替构建：

```sh
make O=build/my-rv64 qemu_rv64_defconfig
make O=build/my-rv64 kernel
make O=build/my-rv32 qemu_rv32_defconfig
make O=build/my-rv32 kernel
```

| 每个输出目录内的路径 | 内容 |
|---|---|
| `.config` / `.config.mk` | `O=` 构建的 resolved 配置与生成片段 |
| `kaleidos.elf` | 完整 boot + Core 镜像 |
| `init.kpkg` / `components/*.kcomp` | 该配置选择的组件包与工件 |
| `cargo/` / `component-rust/` / `component-c/` | boot 与组件的构建缓存 |
| `core/kaleidos.elf` / `core/init.kpkg` | 独立 Core-only 镜像与空包 |
| `exec-fixtures/` / `rootfs.fat` | 用户 ELF 工件与示例 FAT 盘 |
| `runs/` / `logs/` | 每次 guest 的临时目录与保留日志 |

`KCONFIG_CONFIG=<path>` 仍可选择配置；未给 `O=` 时产物归该配置所在目录。
根 `.config` 特例使用 `build/default/`。同时指定两者时，配置位于指定路径，产物归 `O=`。
Host loader 工件统一放 `build/host-fixtures/`，与系统 profile 无关。

## 镜像与开发入口

```sh
make monitor_defconfig  # 关闭自动组合，进入 Core Monitor
make qemu
make kernel            # 完整系统，普通配置只打包生产组件
make core              # Core + boot，独立空包与缓存
make qemu-core
make rootfs
```

组件库存只在 [mk/components.mk](../../mk/components.mk) 维护，不代表运行图。
`CONFIG_TEST_COMPONENTS` 选择是否包含测试组件；CoreTest 使用
`configs/coretest.fragment`，ArchTest 使用 `configs/selftest.fragment`。
普通系统默认不含测试组件。`core` 不会覆盖完整系统的包。

`make clean` 清理所选输出目录的产物，保留配置；`make distclean` 还删除所选配置及片段。
测试见 [测试指南](testing.md)，用户 ELF 见 [userspace](userspace.md)。
