# KaleidOS build entry (Linux Kbuild style: root Makefile drives, tools/ holds helpers).
#
# Configuration is Linux Kconfig style — `.config` is the single configuration
# truth (see docs/architecture/kconfig.md).  Pick a profile, then build:
#
#   make qemu_rv64_defconfig        # RV64 / supervisor / MMU
#   make qemu_rv32_defconfig        # RV32 / supervisor / MMU
#   make qemu_rv32_nommu_defconfig  # RV32 / supervisor / NoMMU
#   make qemu                       # build + run in QEMU (Ctrl-A X to exit)
#
#   make menuconfig                 # edit .config interactively
#   make olddefconfig               # refresh .config with new defaults
#   make defconfig                  # default profile (RV64)
#
# `.config` is turned into Make variables by scripts/kconfig/genmk.py — the
# single place where the config -> build mapping lives.  This Makefile only
# consumes those generated KCFG_* variables and never re-derives them.  Cargo
# features are an internal transport detail, not a user interface.

# ————————————————————————— configuration plumbing —————————————————————————
PROJECT_ROOT := $(abspath $(CURDIR))
# Rust embeds source locations in panic messages.  Keep them independent of the
# checkout path while retaining the boot crate's linker script when RUSTFLAGS
# from the environment overrides Cargo's target-specific flags.
REMAP_RUSTFLAGS := $(RUSTFLAGS) --remap-path-prefix=$(PROJECT_ROOT)=.
# Per-arch boot/binary crate dir.  Default is the RISC-V one so config-free
# goals (clean/fmt) still work without a .config; build goals override it from
# the generated fragment's KCFG_BOOT_DIR (the single config -> build mapping).
BOOT_DIR := os/boot/riscv

# O selects a complete build directory; the default keeps the user's .config.
ifdef O
KCONFIG_CONFIG ?= $(O)/.config
else
KCONFIG_CONFIG ?= .config
O := $(if $(filter .config,$(KCONFIG_CONFIG)),build/default,$(dir $(KCONFIG_CONFIG)))
endif
override BUILD_DIR := $(abspath $(O))
KCONFIG_TOP    := Kconfig
KCONFIG_TREE   := Kconfig os/arch/Kconfig os/core/Kconfig os/components/Kconfig
OUT_DEFCONFIG  ?= defconfig.out
# Kconfig frontend: the pinned third_party/Kconfiglib submodule (no pip needed).
KCONFIGLIB     ?= third_party/Kconfiglib

# Kconfig glue entry points.  KCONFIG_TOP is passed explicitly so the scripts
# resolve the tree themselves instead of assuming the current directory.
CONFIGURE := python3 scripts/kconfig/configure.py --kconfig $(KCONFIG_TOP)
GENMK     := python3 scripts/kconfig/genmk.py --kconfig $(KCONFIG_TOP)

KCONFIG_MK := $(KCONFIG_CONFIG).mk

# Goals that must NOT create or parse a configuration: host-only tools, and
# `clean` / `rootfs` (both have to work on a fresh checkout where no .config
# exists yet — the FAT image is built by mkfs.vfat/mtools, not by Kconfig).
# `abi-gen` / `abi-check` are pure source transformations (abi/*.toml → C/Rust).
CONFIG_FREE_GOALS := clean distclean help fmt test-host test-tools host-fixtures fmt-check bench _test-kconfig rootfs abi-gen abi-check \
                     compat-linux compat-windows test-compat-linux test-compat-windows compat-package

# Goals that CREATE a configuration: they must not generate/parse one, and they
# cannot be combined with build goals in a single invocation.
CONFIG_ONLY_GOALS := menuconfig olddefconfig savedefconfig syncconfig defconfig \
                     $(filter %_defconfig,$(MAKECMDGOALS))

ifneq ($(filter $(CONFIG_ONLY_GOALS),$(MAKECMDGOALS)),)
ifneq ($(filter-out $(CONFIG_ONLY_GOALS) $(CONFIG_FREE_GOALS),$(MAKECMDGOALS)),)
$(error do not combine configuration goals ($(filter $(CONFIG_ONLY_GOALS),$(MAKECMDGOALS))) with build goals in one invocation)
endif
endif

# Build invocations pull in the generated configuration fragment.  It lives next
# to its own .config, so switching profiles can never reuse a stale fragment.
#
# MAKECMDGOALS is global to the whole invocation, so the exemption is only valid
# when *every* goal is config-free / config-only: otherwise `make clean kernel`
# or `make fmt check` would strip KCFG_* from its build goal (the same class of
# bug the config-only guard above rejects in the other direction).  `make` with
# no goal builds kernel, so that counts as a build goal too.
KCFG_GOALS := $(if $(MAKECMDGOALS),$(MAKECMDGOALS),kernel)
ifneq ($(strip $(filter-out $(CONFIG_FREE_GOALS) $(CONFIG_ONLY_GOALS),$(KCFG_GOALS))),)
include $(KCONFIG_MK)
# Build goals select the boot dir from the resolved config (KCFG_BOOT_DIR is
# emitted by genmk.py).  `override` keeps a stray command-line BOOT_DIR from
# creating a second source of truth, mirroring the KCFG_* discipline.
override BOOT_DIR := $(KCFG_BOOT_DIR)
endif

# Explicit default goal: the generated-fragment rule above must not become it.
.DEFAULT_GOAL := kernel

# Consumed from the generated fragment only — never re-derived here.
BOOT_RUSTFLAGS := $(REMAP_RUSTFLAGS) -C link-arg=-T$(KCFG_LINKER)
KERNEL := $(BUILD_DIR)/cargo/$(KCFG_TARGET)/release/bootstrap
OUTPUT := $(BUILD_DIR)/kaleidos.elf
# Core-only 开发镜像的产物名：故意与真实镜像区分，永远不会被误当成完整系统。
CORE_OUTPUT := $(BUILD_DIR)/core/kaleidos.elf

# ————————————————————————— configuration targets —————————————————————————
.PHONY: menuconfig olddefconfig savedefconfig syncconfig defconfig monitor_defconfig selftest_defconfig coretest_defconfig help FORCE

menuconfig:
	@KCONFIG_CONFIG=$(KCONFIG_CONFIG) python3 $(KCONFIGLIB)/menuconfig.py $(KCONFIG_TOP)

olddefconfig:
	@KCONFIG_CONFIG=$(KCONFIG_CONFIG) python3 $(KCONFIGLIB)/olddefconfig.py $(KCONFIG_TOP)

savedefconfig:
	@KCONFIG_CONFIG=$(KCONFIG_CONFIG) python3 $(KCONFIGLIB)/savedefconfig.py --kconfig $(KCONFIG_TOP) --out $(OUT_DEFCONFIG)

# Refresh $(KCONFIG_MK) after .config was edited by hand (or by menuconfig).
syncconfig:
	@$(GENMK) --config $(KCONFIG_CONFIG) --mk $(KCONFIG_MK)

# Default board profile.
defconfig:
	@$(CONFIGURE) --defconfig configs/qemu_rv64_defconfig --out $(KCONFIG_CONFIG)
	@echo "defconfig: $(KCONFIG_CONFIG) = qemu_rv64 (supervisor, MMU)"

# <board>_defconfig -> configs/<board>_defconfig
# e.g. `make qemu_rv32_nommu_defconfig`.
%_defconfig: FORCE
	@$(CONFIGURE) --defconfig configs/$@ --out $(KCONFIG_CONFIG)
	@echo "$@: $(KCONFIG_CONFIG) written"

# Leave composition to the monitor (CoreTest uses this fragment).
coretest_defconfig:
	@$(CONFIGURE) --base $(KCONFIG_CONFIG) --fragment configs/coretest.fragment --out $(KCONFIG_CONFIG)

monitor_defconfig:
	@$(CONFIGURE) --base $(KCONFIG_CONFIG) --fragment configs/monitor.fragment --out $(KCONFIG_CONFIG)

# Lay the selftest fragment (CONFIG_SELFTEST=y) on top of the current profile.
selftest_defconfig:
	@$(CONFIGURE) --base $(KCONFIG_CONFIG) --fragment configs/selftest.fragment --out $(KCONFIG_CONFIG)

help:
	@echo "KaleidOS — configuration"
	@echo "  make menuconfig                  edit .config interactively"
	@echo "  make defconfig                   default profile (RV64, supervisor, MMU)"
	@echo "  make qemu_rv64_defconfig         RV64 / supervisor / MMU"
	@echo "  make qemu_rv32_defconfig         RV32 / supervisor / MMU"
	@echo "  make qemu_rv32_nommu_defconfig   RV32 / supervisor / NoMMU"
	@echo "  make monitor_defconfig           disable initial component; boot Core Monitor"
	@echo "  make coretest_defconfig          monitor + test component inventory"
	@echo "  make olddefconfig                refresh .config with new defaults"
	@echo "KaleidOS — build"
	@echo "  make kernel                      build $(OUTPUT)"
	@echo "  make O=build/<name> ...           private configuration and output directory"
	@echo "  make qemu                        build + run in QEMU"
	@echo "  make core                        Core-only dev image (skips all components)"
	@echo "  make qemu-core                   run the Core-only dev image (no rootfs drive)"
	@echo "  make rootfs                      build the small FAT image attached to make qemu"
	@echo "  make check | test | test-host | test-qemu | test-arch"
	@echo "  make test-init                   RV64/RV32 automatic boot composition workflows"
	@echo "  make compat-linux | compat-windows   build upstream libc-test ELF / PE programs"
	@echo "  make test-compat-linux | test-compat-windows   run on the native reference OS"
	@echo "  make compat-package              package both builds for future application exec"
	@echo "  make exec-fixtures               build RV64 static ELF execution probes"
	@echo "  make abi-gen | abi-check         ABI 单一来源：abi/*.toml → 生成 C/Rust（check 只校验）"

# Materialise a configuration on first use.
$(KCONFIG_CONFIG):
	@mkdir -p $(@D)
	@echo "no $(KCONFIG_CONFIG) yet; creating it from configs/qemu_rv64_defconfig"
	@$(CONFIGURE) --defconfig configs/qemu_rv64_defconfig --out $@

$(KCONFIG_MK): $(KCONFIG_CONFIG) scripts/kconfig/genmk.py $(KCONFIG_TREE)
	@$(GENMK) --config $(KCONFIG_CONFIG) --mk $@
