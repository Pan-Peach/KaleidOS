# KaleidOS 构建入口（Linux Kbuild 风格：根 Makefile 驱动，tools/ 放辅助脚本）
#
# 用法：
#   make kernel            # 构建 kaleidos.elf（os/riscv64 + core 链接）
#   make kernel ARCH=...   # 指定架构（当前只有 rv64）
#   make qemu              # 在 QEMU 上运行（Ctrl-A X 退出）
#   make clean

ARCH      ?= rv64
# ARCH 名 → 源码目录（rv64 → riscv64）
ifeq ($(ARCH),rv64)
BOOT_DIR  := os/boot/riscv64
endif
BOOT_DIR  ?= os/boot/$(ARCH)
KERNEL    := $(BOOT_DIR)/target/riscv64gc-unknown-none-elf/release/bootstrap
OUTPUT    := kaleidos-$(ARCH)

.PHONY: kernel qemu clean

# 构建单镜像并复制到仓库根（/kaleidos-* 已在 .gitignore）
kernel:
	cd $(BOOT_DIR) && cargo build --release
	cp $(KERNEL) $(OUTPUT)
	@echo "built: $(OUTPUT)"

# 调试看输出（串口打印 + Ctrl-A X 退出 QEMU）
# -smp 2: 2 核（hart 0 boot，hart 1 被 OpenSBI park）；-m 4G: 4GB RAM
qemu: kernel
	qemu-system-riscv64 -machine virt -smp 2 -m 4G -bios default -kernel $(OUTPUT) -nographic

clean:
	rm -f $(OUTPUT)
	cd $(BOOT_DIR) && cargo clean --release

# —— 质量工具链（fmt / clippy / check）——
.PHONY: fmt clippy check

# 自己的 crate（显式列出；third_party 是 submodule，不归我们 fmt/clippy）
OUR_CRATES := -p kernel -p arch -p scheduler_rr -p allocator_simple -p core_test -p logger

# 代码格式化（rustfmt）
fmt:
	cargo fmt $(OUR_CRATES)
	cd os/boot/riscv64 && cargo fmt

# lint（clippy，只查我们自己：third_party 已 exclude，失败即失败）
clippy:
	cargo clippy --workspace --all-targets

# 一键质量门禁：任何一步失败即整体失败（CI 可直接用）
check:
	cargo fmt $(OUR_CRATES) -- --check
	cd os/boot/riscv64 && cargo fmt -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace
	cd os/boot/riscv64 && cargo build
