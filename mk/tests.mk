.PHONY: test test-qemu test-init test-arch test-arch-smp-rv64 test-arch-new test-arch-x86_64 test-arch-aarch64 test-arch-loongarch64 boot-build boot-check _test-build
QEMU_TEST_ARGS = --qemu $(KCFG_QEMU) --memory $(KCFG_QEMU_MEM) --qemu-flags '$(KCFG_QEMU_FLAGS)' --work-dir $(BUILD_DIR)/runs --log-dir $(BUILD_DIR)/logs

# 交叉构建门禁：每个 profile 用一份私有 .config（互不污染，也不动用户的 .config）。
boot-build: init.kpkg
	cd $(BOOT_DIR) && KALEIDOS_INITPKG="$(BUILD_DIR)/init.kpkg" CONFIG_BOOT_COMPONENT="$(CONFIG_BOOT_COMPONENT)" CONFIG_MAX_CPUS="$(CONFIG_MAX_CPUS)" CONFIG_TRACE_CAPACITY="$(CONFIG_TRACE_CAPACITY)" RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --target-dir $(BUILD_DIR)/cargo --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET)

boot-check: init.kpkg
	cd $(BOOT_DIR) && KALEIDOS_INITPKG="$(BUILD_DIR)/init.kpkg" CONFIG_BOOT_COMPONENT="$(CONFIG_BOOT_COMPONENT)" CONFIG_MAX_CPUS="$(CONFIG_MAX_CPUS)" CONFIG_TRACE_CAPACITY="$(CONFIG_TRACE_CAPACITY)" cargo check --target-dir $(BUILD_DIR)/cargo --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET)

# 内部：两个架构的交叉构建门禁（`make check` 的一步）。
_test-build:
	@$(MAKE) O=build/check/rv64 qemu_rv64_defconfig
	@$(MAKE) O=build/check/rv64 boot-build
	@$(MAKE) O=build/check/rv32 qemu_rv32_defconfig
	@$(MAKE) O=build/check/rv32 boot-check

# 自动 QEMU：构建 + 启动 + 自动执行 core_test + 判定 PASS（输出进日志）。
# 每个架构先落到自己的 .config，再在子 make 里按该 profile 构建。
# runner 按 QEMU 机器拓扑跑两个场景（场景只控制硬件；判定在 CoreTest 内）：
#   default   挂 virtio-rng + 两块 1 MiB virtio-blk：prober 首个 Match 后 attach，
#             第二块盘验证拒绝二次 attach；
#   no-block  只挂 virtio-rng（无块设备）：prober 对每个候选 create + pull NoMatch、
#             干净结束，CoreTest 断言 NoMatch 路径而不是 attach。
_test-qemu-rv64:
	@$(MAKE) O=build/tests/qemu-rv64 qemu_rv64_defconfig
	@$(MAKE) O=build/tests/qemu-rv64 coretest_defconfig
	@$(MAKE) O=build/tests/qemu-rv64 _test-qemu-one

_test-qemu-rv32:
	@$(MAKE) O=build/tests/qemu-rv32 qemu_rv32_defconfig
	@$(MAKE) O=build/tests/qemu-rv32 coretest_defconfig
	@$(MAKE) O=build/tests/qemu-rv32 _test-qemu-one

_test-qemu-one: kernel
	@python3 tests/qemu/runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) $(QEMU_TEST_ARGS) --scenario default
	@python3 tests/qemu/runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) $(QEMU_TEST_ARGS) --scenario no-block

# 自动 init：实际 FAT 盘、无盘、坏 FAT 盘（挂载失败回 monitor）。
.PHONY: test-init _test-init-rv64 _test-init-rv32 _test-init-one
# Keep the default aggregate readable; each profile owns its artifacts.
.NOTPARALLEL: test test-qemu test-init test-arch
_test-init-rv64:
	@$(MAKE) O=build/tests/init-rv64 qemu_rv64_defconfig
	@$(MAKE) O=build/tests/init-rv64 _test-init-one

_test-init-rv32:
	@$(MAKE) O=build/tests/init-rv32 qemu_rv32_defconfig
	@$(MAKE) O=build/tests/init-rv32 _test-init-one

_test-init-one: kernel rootfs $(if $(filter rv64,$(KCFG_ARCH)),exec-fixtures,)
	@python3 tests/qemu/init_runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) $(QEMU_TEST_ARGS) --rootfs $(ROOTFS) --exec-fixtures $(BUILD_DIR)/exec-fixtures --scenario fat
	@python3 tests/qemu/init_runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) $(QEMU_TEST_ARGS) --rootfs $(ROOTFS) --exec-fixtures $(BUILD_DIR)/exec-fixtures --scenario dual-fat
	@if [ "$(KCFG_ARCH)" = rv64 ]; then python3 tests/qemu/init_runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) $(QEMU_TEST_ARGS) --rootfs $(ROOTFS) --exec-fixtures $(BUILD_DIR)/exec-fixtures --scenario oom; fi
	@python3 tests/qemu/init_runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) $(QEMU_TEST_ARGS) --rootfs $(ROOTFS) --exec-fixtures $(BUILD_DIR)/exec-fixtures --scenario no-block
	@python3 tests/qemu/init_runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) $(QEMU_TEST_ARGS) --rootfs $(ROOTFS) --exec-fixtures $(BUILD_DIR)/exec-fixtures --scenario bad-fat

test-init: _test-init-rv64 _test-init-rv32

# 公开入口：CoreTest / ksh 流程 + 默认 init 启动流程，均覆盖两个架构。
test-qemu: _test-qemu-rv64 _test-qemu-rv32 test-init

# White-box architectural selftests use a separate image: the private archtest
# profile = the board defconfig + configs/selftest.fragment (CONFIG_SELFTEST=y),
# which drives the boot `selftest` feature in a private output directory.
_test-arch-rv64:
	@$(MAKE) O=build/tests/archtest-rv64 qemu_rv64_defconfig
	@$(MAKE) O=build/tests/archtest-rv64 selftest_defconfig
	@$(MAKE) O=build/tests/archtest-rv64 _test-arch-one

_test-arch-rv32:
	@$(MAKE) O=build/tests/archtest-rv32 qemu_rv32_defconfig
	@$(MAKE) O=build/tests/archtest-rv32 selftest_defconfig
	@$(MAKE) O=build/tests/archtest-rv32 _test-arch-one

_test-arch-one: kernel
	@python3 tests/qemu/arch_runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) $(QEMU_TEST_ARGS) $(if $(TEST_CASE),--case $(TEST_CASE),)

.PHONY: _test-arch-list
_test-arch-list:
	@python3 tests/qemu/arch_runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) $(QEMU_TEST_ARGS) --list

# 公开入口：两个架构都跑。
test-arch: _test-arch-rv64 _test-arch-rv32 test-arch-smp-rv64

# Opt-in new ISAs use Core-only images: component relocations remain RISC-V-only.
# Their supported hardware cases are documented in docs/modules/{arch,boot}.md.
_test-arch-core-one: core
	@python3 tests/qemu/arch_runner.py --arch $(KCFG_ARCH) --kernel $(CORE_OUTPUT) $(QEMU_TEST_ARGS)

_test-arch-x86_64:
	@$(MAKE) O=build/tests/archtest-x86_64 qemu_x86_64_defconfig
	@$(MAKE) O=build/tests/archtest-x86_64 selftest_defconfig
	@$(MAKE) O=build/tests/archtest-x86_64 _test-arch-core-one

_test-arch-aarch64:
	@$(MAKE) O=build/tests/archtest-aarch64 qemu_aarch64_defconfig
	@$(MAKE) O=build/tests/archtest-aarch64 selftest_defconfig
	@$(MAKE) O=build/tests/archtest-aarch64 _test-arch-core-one

_test-arch-loongarch64:
	@$(MAKE) O=build/tests/archtest-loongarch64 qemu_loongarch64_defconfig
	@$(MAKE) O=build/tests/archtest-loongarch64 selftest_defconfig
	@$(MAKE) O=build/tests/archtest-loongarch64 _test-arch-core-one

test-arch-x86_64: _test-arch-x86_64
test-arch-aarch64: _test-arch-aarch64
test-arch-loongarch64: _test-arch-loongarch64
# Explicitly select all three experimental ISA suites.
test-arch-new: _test-arch-x86_64 _test-arch-aarch64 _test-arch-loongarch64

# RISC-V SMP ArchTest: runs only the `smp-*` cases (multi-CPU
# QEMU).  The cases are always compiled now -- only the runner's case list gates
# whether they run -- so no extra config fragment is needed. SMP includes
# CPU bring-up, IPI and per-CPU hardware contracts. Component scheduling, remote
# wake and panic containment are CoreTest integration checks in test-qemu.
_test-arch-smp-rv64:
	@$(MAKE) O=build/tests/archtest-smp-rv64 qemu_rv64_defconfig
	@$(MAKE) O=build/tests/archtest-smp-rv64 selftest_defconfig
	@$(MAKE) O=build/tests/archtest-smp-rv64 _test-arch-smp-one

_test-arch-smp-one: kernel
	@python3 tests/qemu/arch_runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) $(QEMU_TEST_ARGS) --smp-only $(if $(TEST_CASE),--case $(TEST_CASE),)

test-arch-smp-rv64: _test-arch-smp-rv64

# 完整测试：host 单测 + 两个架构的 QEMU CoreTest + ArchTest。
test: test-host test-qemu test-arch
