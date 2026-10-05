.PHONY: kernel core qemu qemu-core init.kpkg rootfs exec-fixtures clean distclean
PACKAGE := python3 scripts/build/package.py --target $(KCFG_TARGET)
BOOT_ENV = CONFIG_BOOT_COMPONENT="$(CONFIG_BOOT_COMPONENT)" CONFIG_MAX_CPUS="$(CONFIG_MAX_CPUS)" CONFIG_TRACE_CAPACITY="$(CONFIG_TRACE_CAPACITY)"

init.kpkg: $(if $(and $(filter rv64,$(KCFG_ARCH)),$(filter y,$(CONFIG_TEST_COMPONENTS))),exec-fixtures,)
	$(PACKAGE) --output $(BUILD_DIR)/init.kpkg --rust $(KCOMP_SRCS) --c $(KCOMP_C_SRCS)

kernel: init.kpkg
	cd $(BOOT_DIR) && $(BOOT_ENV) KALEIDOS_INITPKG="$(BUILD_DIR)/init.kpkg" RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --target-dir $(BUILD_DIR)/cargo --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET) --release
	cp $(KERNEL) $(OUTPUT)

# A separate artifact directory: no production/test package is overwritten.
core:
	$(PACKAGE) --output $(BUILD_DIR)/core/init.kpkg
	cd $(BOOT_DIR) && $(BOOT_ENV) KALEIDOS_INITPKG="$(BUILD_DIR)/core/init.kpkg" RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --target-dir $(BUILD_DIR)/core/cargo --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET) --release
	cp $(BUILD_DIR)/core/cargo/$(KCFG_TARGET)/release/bootstrap $(CORE_OUTPUT)

EXEC_FIXTURE_CC ?= riscv64-unknown-elf-gcc
exec-fixtures:
	python3 tests/compat/exec_fixtures.py --arch $(KCFG_ARCH) --cc $(EXEC_FIXTURE_CC) --out $(BUILD_DIR)/exec-fixtures

ROOTFS_DIR := tests/fixtures/rootfs
ROOTFS := $(BUILD_DIR)/rootfs.fat
ROOTFS_SRCS := $(shell find $(ROOTFS_DIR) -type f)
rootfs: $(ROOTFS)
$(ROOTFS): $(ROOTFS_SRCS)
	@mkdir -p $(@D)
	dd if=/dev/zero of=$@.tmp bs=1M count=8 status=none
	mkfs.vfat --invariant -n KALEIDOS $@.tmp
	mcopy -s -i $@.tmp $(ROOTFS_DIR)/* ::
	mv -f $@.tmp $@

qemu: kernel rootfs
	$(KCFG_QEMU) $(KCFG_QEMU_FLAGS) -smp 2 -m $(KCFG_QEMU_MEM) -kernel $(OUTPUT) -nographic -drive file=$(ROOTFS),if=none,format=raw,id=rootfs -device virtio-blk-device,drive=rootfs
qemu-core: core
	$(KCFG_QEMU) $(KCFG_QEMU_FLAGS) -smp 2 -m $(KCFG_QEMU_MEM) -kernel $(CORE_OUTPUT) -nographic

clean:
	rm -rf $(addprefix $(BUILD_DIR)/,cargo core components component-rust component-c exec-fixtures runs logs)
	rm -f $(addprefix $(BUILD_DIR)/,kaleidos.elf init.kpkg init.kpkg.tmp host.kpkg host.kpkg.tmp rootfs.fat rootfs.fat.tmp)
distclean: clean
	rm -f $(KCONFIG_CONFIG) $(KCONFIG_CONFIG).old $(KCONFIG_MK)
