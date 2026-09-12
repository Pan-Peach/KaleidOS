# KaleidOS 构建入口（Linux Kbuild 风格：根 Makefile 驱动，tools/ 放辅助脚本）
#
# 用法：
#   make kernel            # 构建 kaleidos-rv64（os/boot/riscv + core 链接）
#   make kernel ARCH=rv32  # 构建 RV32/Sv32 profile
#   make qemu              # 在 QEMU 上运行（Ctrl-A X 退出）
#   make clean

ARCH      ?= rv64
# Rust embeds source locations in panic messages.  Keep them independent of
# the checkout path while retaining the boot crate's linker script when
# RUSTFLAGS from the environment overrides Cargo's target-specific flags.
PROJECT_ROOT := $(abspath $(CURDIR))
REMAP_RUSTFLAGS := $(RUSTFLAGS) --remap-path-prefix=$(PROJECT_ROOT)=.

# Profile → build contract.  Keep the matrix small until more machines exist.
ifeq ($(ARCH),rv64)
TARGET    := riscv64gc-unknown-none-elf
BOOT_DIR  := os/boot/riscv
LINKER    := linker.ld
QEMU      := qemu-system-riscv64
QEMU_MEM  := 4G
else ifeq ($(ARCH),rv32)
TARGET    := riscv32imac-unknown-none-elf
BOOT_DIR  := os/boot/riscv
LINKER    := linker32.ld
QEMU      := qemu-system-riscv32
# RV32 地址空间只有 4 GiB，且内核位于 0x80000000：
# >1 GiB 的 RAM 会让 FDT 区间在 32 位 usize 下回绕（boot 无法初始化帧区）。
QEMU_MEM  := 1G
endif
BOOT_DIR  ?= os/boot/$(ARCH)
TARGET    ?= riscv64gc-unknown-none-elf
LINKER    ?= linker.ld
QEMU      ?= qemu-system-riscv64
BOOT_RUSTFLAGS := $(REMAP_RUSTFLAGS) -C link-arg=-T$(LINKER)
KERNEL    := $(BOOT_DIR)/target/$(TARGET)/release/bootstrap
OUTPUT    := kaleidos-$(ARCH)

.PHONY: kernel qemu clean init.kpkg

# —— 组件 .kcomp 打包 + 内嵌（Linux insmod/depmod 模式）——
# 每个组件经共享管线 tools/build-kcomp.sh 构建成**链接后的** .kcomp（ET_REL 组件程序）：
# staticlib → rust-lld -r --gc-sections -u kcomp_init → strip → 白名单/重定位契约校验。
KCOMP_COMPONENTS := core_test kcomp_smoke scheduler_rr kcomp_panic
KPKG_DIR   := /tmp/opencode/kpkg
KPKG_BUILD := /tmp/opencode/kpkg-build

# 构建所有组件 .kcomp → 统一打包 init.kpkg（cpio newc + manifest）
init.kpkg:
	@set -e; mkdir -p $(KPKG_DIR); \
	for name in $(KCOMP_COMPONENTS); do \
		RUSTFLAGS="$(REMAP_RUSTFLAGS)" tools/build-kcomp.sh \
			$(CURDIR)/os/components/$$name $$name $(TARGET) \
			$(KPKG_DIR)/$$name.kcomp $(KPKG_BUILD); \
	done
	@echo "$(KCOMP_COMPONENTS)" | tr ' ' '\n' > $(KPKG_DIR)/manifest
	@mkdir -p $(CURDIR)/tools/qemu
	cd $(KPKG_DIR) && find . -type f | cpio -o -H newc --quiet > $(CURDIR)/tools/qemu/init.kpkg
	@echo "packed: tools/qemu/init.kpkg ($(KCOMP_COMPONENTS))"

# 发布形态：kaleidos.elf = bootstrap + core + .initpkg(kpkg 编译期内嵌)
kernel: init.kpkg
	cd $(BOOT_DIR) && RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --target $(TARGET) --release
	cp $(KERNEL) $(OUTPUT)
	@echo "built: $(OUTPUT) (with embedded init.kpkg)"

# 调试看输出（串口打印 + Ctrl-A X 退出 QEMU）
# -smp 2: 2 核（boot hart 由 OpenSBI 选择）；内存按 ARCH（rv64=4G，rv32=1G，
# 32 位地址空间放不下 4 GiB RAM，见上方 QEMU_MEM 注释）
qemu: kernel
	$(QEMU) -machine virt -smp 2 -m $(QEMU_MEM) -bios default \
		-kernel $(OUTPUT) -nographic

clean:
	rm -f $(OUTPUT)
	rm -f tools/qemu/init.kpkg
	cd $(BOOT_DIR) && cargo clean --release

# —— 质量工具链（fmt / clippy / check / 测试通道）——
# 测试入口显式分层（testing.md 金字塔落地）：
#   make test-host    host 单测（快速，日常主力）
#   make test-build   两个架构的交叉构建门禁
#   make test-qemu-rv64 / test-qemu-rv32   自动 QEMU（boot smoke + 自动 CoreTest）
#   make test-qemu    两个架构都跑
#   make bench        host release 性能基线（手动跑，不进 CI）
#   make check        CI 全量门禁 = fmt + clippy + test-host + test-build
.PHONY: fmt clippy check test-host bench test-build test-qemu test-qemu-rv64 test-qemu-rv32 test-arch test-arch-rv64 test-arch-rv32 test-arch-one

# 自己的 crate（显式列出；third_party 是 submodule，不归我们 fmt/clippy）
OUR_CRATES := -p kernel -p arch -p scheduler_rr -p allocator_simple -p core_test -p logger

# 代码格式化（rustfmt）；kcomp-sdk 是独立 workspace（root exclude），单独 fmt。
fmt:
	cargo fmt $(OUR_CRATES)
	cd os/components/kcomp-sdk && cargo fmt
	cd os/boot/riscv && cargo fmt

# lint（clippy，只查我们自己：third_party 已 exclude，失败即失败）
# core_test / scheduler_rr 的 lib 是 staticlib（最终产物，需裸机 panic handler），
# host 无法完成其静态链接，因此对它们按真实目标 $(TARGET) 做 clippy。
clippy:
	cargo clippy --workspace --all-targets --exclude core_test --exclude scheduler_rr
	cargo clippy -p core_test -p scheduler_rr --target $(TARGET)
	cd os/components/kcomp-sdk && cargo clippy --all-targets

# host 单测：Core truth / parser / property / backend 纯逻辑（不需要 QEMU）
test-host:
	cargo test --workspace

# 性能基线（host release，手动跑）：ns/call 量级；基线用例见 handle/mmio.rs bench_*
bench:
	cargo test --release -p kernel --lib bench -- --ignored --nocapture

# 交叉构建门禁：RV64 链接 + RV32 检查（不运行）
test-build:
	cd os/boot/riscv && RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build
	cd os/boot/riscv && cargo check --target riscv32imac-unknown-none-elf

# 自动 QEMU：构建 + 启动 + 自动执行 core_test + 判定 PASS（输出进日志）。
# 每个架构用子 make 传 ARCH：ifeq 在解析期就固定 TARGET/BOOT_DIR，
# target-specific 变量来不及生效，直接 $(MAKE) ARCH=… 才是可靠的。
test-qemu-rv64:
	$(MAKE) ARCH=rv64 test-qemu-one

test-qemu-rv32:
	$(MAKE) ARCH=rv32 test-qemu-one

test-qemu-one: kernel
	@python3 tests/qemu/runner.py $(ARCH)

test-qemu: test-qemu-rv64 test-qemu-rv32

# White-box architectural selftests use a separate feature-gated image.  Keep
# the normal `kernel` target feature-free so `make qemu` still enters Monitor.
test-arch-rv64:
	$(MAKE) ARCH=rv64 test-arch-one

test-arch-rv32:
	$(MAKE) ARCH=rv32 test-arch-one

test-arch-one: init.kpkg
	cd $(BOOT_DIR) && RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --features selftest --target $(TARGET) --release
	cp $(KERNEL) $(OUTPUT)-selftest
	@python3 tests/qemu/arch_runner.py $(ARCH)

test-arch: test-arch-rv64 test-arch-rv32

# 一键质量门禁：任何一步失败即整体失败（CI 可直接用）
# 依赖 $(INITPKG_O)：boot 链接需要 .initpkg 对象存在
check: init.kpkg
	cargo fmt $(OUR_CRATES) -- --check
	cd os/components/kcomp-sdk && cargo fmt -- --check
	cd os/boot/riscv && cargo fmt -- --check
	cargo clippy --workspace --all-targets --exclude core_test --exclude scheduler_rr -- -D warnings
	cargo clippy -p core_test -p scheduler_rr --target $(TARGET) -- -D warnings
	cd os/components/kcomp-sdk && cargo clippy --all-targets -- -D warnings
	$(MAKE) test-host
	$(MAKE) test-build
