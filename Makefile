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
# -fw_cfg: 把 init.kpkg（cpio 归档）作为 fw_cfg 文件传给内核 —— 
#          "内核可以直接找"的组件仓库（开发模式：kaleidos.elf + 外部 init.kpkg）
qemu: kernel init.kpkg
	qemu-system-riscv64 -machine virt -smp 2 -m 4G -bios default \
		-kernel $(OUTPUT) -nographic \
		-fw_cfg file=tools/qemu/init.kpkg,name=opt/kaleid/init.kpkg

# —— 组件 .kcomp 打包（Linux insmod/depmod 模式）——
# 开发模式：kaleidos.elf（内核）+ init.kpkg（组件归档）分开；
# 发布模式：init.kpkg 内嵌进 kaleidos.elf 的 .initpkg（重打包，未来）
KCOMP_SRC  := os/components/kcomp_smoke
KCOMP_OBJ  := $(shell find $(KCOMP_SRC)/target/riscv64gc-unknown-none-elf/release/deps -name "*.o" 2>/dev/null | head -1)
KPKG_DIR   := /tmp/opencode/kpkg

# 编译组件（ET_REL 对象 = .kcomp）→ cpio newc 归档 + 文本 manifest
init.kpkg:
	cd $(KCOMP_SRC) && cargo rustc --release --target riscv64gc-unknown-none-elf -- --emit=obj
	mkdir -p $(KPKG_DIR)
	cp $(KCOMP_OBJ) $(KPKG_DIR)/kcomp_smoke.kcomp
	printf "kcomp_smoke.kcomp\n" > $(KPKG_DIR)/manifest
	cd $(KPKG_DIR) && find . -type f | cpio -o -H newc --quiet > $(CURDIR)/tools/qemu/init.kpkg
	@echo "packed: tools/qemu/init.kpkg"

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
check:
	cargo fmt $(OUR_CRATES) -- --check
	cd os/boot/riscv64 && cargo fmt -- --check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test --workspace
	cd os/boot/riscv64 && cargo build
