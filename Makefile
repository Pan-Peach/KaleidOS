# KaleidOS 构建入口（Linux Kbuild 风格：根 Makefile 驱动，tools/ 放辅助脚本）
#
# 用法：
#   make kernel            # 构建 kaleidos.elf（bootstrap + core 链接）
#   make kernel ARCH=...   # 指定架构（当前只有 rv64）
#   make qemu              # 在 QEMU 上运行（Ctrl-A X 退出）
#   make clean

ARCH      ?= rv64
# ARCH 名 → 源码目录（rv64 → riscv64）
ifeq ($(ARCH),rv64)
BOOT_DIR  := bootstrap/riscv64
endif
BOOT_DIR  ?= bootstrap/$(ARCH)
KERNEL    := $(BOOT_DIR)/target/riscv64gc-unknown-none-elf/release/bootstrap
OUTPUT    := kaleidos-$(ARCH)

.PHONY: kernel qemu clean

# 构建单镜像并复制到仓库根（/kaleidos-* 已在 .gitignore）
kernel:
	cd $(BOOT_DIR) && cargo build --release
	cp $(KERNEL) $(OUTPUT)
	@echo "built: $(OUTPUT)"

# 调试看输出（串口打印 + Ctrl-A X 退出 QEMU）
qemu: kernel
	qemu-system-riscv64 -machine virt -bios default -kernel $(OUTPUT) -nographic

clean:
	rm -f $(OUTPUT)
	cd $(BOOT_DIR) && cargo clean --release
