# KaleidOS 构建入口（Linux Kbuild 风格：根 Makefile 驱动，tools/ 放辅助脚本）
#
# 用法：
#   make kernel            # 构建 kaleidos.elf（os/riscv64 + core 链接）
#   make kernel ARCH=...   # 指定架构（当前只有 rv64）
#   make qemu              # 在 QEMU 上运行（Ctrl-A X 退出）
#   make clean

ARCH      ?= rv64
# Rust embeds source locations in panic messages.  Keep them independent of
# the checkout path while retaining the boot crate's linker script when
# RUSTFLAGS from the environment overrides Cargo's target-specific flags.
PROJECT_ROOT := $(abspath $(CURDIR))
REMAP_RUSTFLAGS := $(RUSTFLAGS) --remap-path-prefix=$(PROJECT_ROOT)=.
BOOT_RUSTFLAGS := $(REMAP_RUSTFLAGS) -C link-arg=-Tlinker.ld

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
	cd $(BOOT_DIR) && RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --release
	cp $(KERNEL) $(OUTPUT)
	@echo "built: $(OUTPUT)"

# —— 组件 .kcomp 打包 + 内嵌（Linux insmod/depmod 模式）——
# 组件名 → 源码目录；每个组件编译成 ET_REL 对象（= .kcomp）
KCOMP_COMPONENTS := core_test kcomp_smoke
KCOMP_DIRS := $(addprefix os/components/,$(KCOMP_COMPONENTS))
KPKG_DIR   := /tmp/opencode/kpkg

# 构建所有组件对象（ET_REL）→ 统一打包 init.kpkg（cpio newc + manifest）
init.kpkg:
	@for d in $(KCOMP_DIRS); do \
		( cd $$d && RUSTFLAGS="$(REMAP_RUSTFLAGS)" cargo rustc --release --target riscv64gc-unknown-none-elf -- --emit=obj ); \
	done
	@mkdir -p $(KPKG_DIR)
	@for d in $(KCOMP_DIRS); do \
		name=$$(basename $$d); \
		obj=$$(find $$d/target/riscv64gc-unknown-none-elf/release/deps target/riscv64gc-unknown-none-elf/release/deps -maxdepth 1 -name "$$name-*.o" 2>/dev/null | head -1); \
		cp $$obj $(KPKG_DIR)/$$name.kcomp; \
	done
	@echo "$(KCOMP_COMPONENTS)" | tr ' ' '\n' > $(KPKG_DIR)/manifest
	cd $(KPKG_DIR) && find . -type f | cpio -o -H newc --quiet > $(CURDIR)/tools/qemu/init.kpkg
	@echo "packed: tools/qemu/init.kpkg ($(KCOMP_COMPONENTS))"

# 发布形态：kaleidos.elf = bootstrap + core + .initpkg(kpkg 编译期内嵌)
kernel: init.kpkg
	cd $(BOOT_DIR) && RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --release
	cp $(KERNEL) $(OUTPUT)
	@echo "built: $(OUTPUT) (with embedded init.kpkg)"

# 调试看输出（串口打印 + Ctrl-A X 退出 QEMU）
# -smp 2: 2 核（hart 0 boot，hart 1 被 OpenSBI park）；-m 4G: 4GB RAM
qemu: kernel
	qemu-system-riscv64 -machine virt -smp 2 -m 4G -bios default \
		-kernel $(OUTPUT) -nographic

clean:
	rm -f $(OUTPUT)
	rm -f tools/qemu/init.kpkg
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
# 依赖 $(INITPKG_O)：boot 链接需要 .initpkg 对象存在
check: init.kpkg
	cargo fmt $(OUR_CRATES) -- --check
	cd os/boot/riscv64 && cargo fmt -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace
	cd os/boot/riscv64 && RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build
