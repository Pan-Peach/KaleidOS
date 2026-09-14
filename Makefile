# KaleidOS build entry (Linux Kbuild style: root Makefile drives, tools/ holds helpers).
#
# Configuration is Linux Kconfig style — `.config` is the single configuration
# truth (see docs/kconfig.md).  Pick a profile, then build:
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
BOOT_DIR := os/boot/riscv

KCONFIG_CONFIG ?= .config
KCONFIG_TOP    := Kconfig
KCONFIG_TREE   := Kconfig os/arch/Kconfig os/core/Kconfig os/components/Kconfig
OUT_DEFCONFIG  ?= defconfig.out
# Kconfig frontend: the pinned third_party/Kconfiglib submodule (no pip needed).
KCONFIGLIB     ?= third_party/Kconfiglib

# Kconfig glue entry points.  KCONFIG_TOP is passed explicitly so the scripts
# resolve the tree themselves instead of assuming the current directory.
CONFIGURE := python3 scripts/kconfig/configure.py --kconfig $(KCONFIG_TOP)
GENMK     := python3 scripts/kconfig/genmk.py --kconfig $(KCONFIG_TOP)

# Deprecated compatibility entry points ARCH= / VM=.  They are recognised ONLY
# when given on the command line (environment ARCH/VM are ignored), and they
# never carry build truth: they are translated into a PRIVATE resolved config
# and then validated by scripts/kconfig/configure.py, so an unsatisfiable
# request (e.g. RV64 + NoMMU) fails loudly instead of building something else.
LEGACY_ARCH := $(if $(filter command line,$(origin ARCH)),$(ARCH),)
LEGACY_VM   := $(if $(filter command line,$(origin VM)),$(VM),)

ifneq ($(strip $(LEGACY_ARCH)$(LEGACY_VM)),)
KCONFIG_CONFIG   := build/configs/legacy/.config
LEGACY_REQUEST   := build/configs/legacy/.request
LEGACY_BASE_ARGS := $(if $(wildcard .config),--base .config,--defconfig configs/qemu_rv64_defconfig)
LEGACY_SETS      := \
	$(if $(LEGACY_ARCH),--set CONFIG_ARCH_RISCV$(if $(filter rv32,$(LEGACY_ARCH)),32,64)=y) \
	$(if $(LEGACY_VM),--set CONFIG_VM_$(if $(filter nommu,$(LEGACY_VM)),NOMMU,MMU)=y)
endif

# Derived AFTER the legacy block so it tracks the final KCONFIG_CONFIG.
KCONFIG_MK := $(KCONFIG_CONFIG).mk

# Goals that must NOT create or parse a configuration: host-only tools, and
# `clean` (which has to work on a fresh checkout where no .config exists yet).
CONFIG_FREE_GOALS := clean distclean help fmt test-host bench

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
ifeq ($(filter $(CONFIG_FREE_GOALS),$(MAKECMDGOALS)),)
ifeq ($(filter $(CONFIG_ONLY_GOALS),$(MAKECMDGOALS)),)
include $(KCONFIG_MK)
endif
endif

# Explicit default goal: the generated-fragment rule above must not become it.
.DEFAULT_GOAL := kernel

# Consumed from the generated fragment only — never re-derived here.
BOOT_RUSTFLAGS := $(REMAP_RUSTFLAGS) -C link-arg=-T$(KCFG_LINKER)
KERNEL := $(BOOT_DIR)/target/$(KCFG_TARGET)/release/bootstrap
OUTPUT := kaleidos-$(KCFG_ARCH)$(if $(filter y,$(KCFG_SELFTEST)),-selftest,)

# ————————————————————————— configuration targets —————————————————————————
.PHONY: menuconfig olddefconfig savedefconfig syncconfig defconfig help FORCE

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
	@echo "  make olddefconfig                refresh .config with new defaults"
	@echo "KaleidOS — build"
	@echo "  make kernel                      build kaleidos-\$$(KCFG_ARCH)"
	@echo "  make qemu                        build + run in QEMU"
	@echo "  make check | test-host | test-build | test-qemu | test-arch | test-driver-prober"

# Materialise a configuration on first use.
ifeq ($(strip $(LEGACY_ARCH)$(LEGACY_VM)),)
$(KCONFIG_CONFIG):
	@echo "no $(KCONFIG_CONFIG) yet; creating it from configs/qemu_rv64_defconfig"
	@$(CONFIGURE) --defconfig configs/qemu_rv64_defconfig --out $@
else
# The private config is re-resolved (and re-validated) exactly when the legacy
# request changes, tracked by a stamp file.  The stamp recipe runs every time
# (FORCE) but only touches the file on a real change: were it to rewrite $@ each
# time, the config would count as changed on every restart and GNU make would
# re-exec forever.
$(LEGACY_REQUEST): FORCE
	@mkdir -p $(dir $@)
	@printf 'ARCH=%s VM=%s\n' '$(LEGACY_ARCH)' '$(LEGACY_VM)' > $@.tmp
	@cmp -s $@.tmp $@ || mv $@.tmp $@
	@rm -f $@.tmp

$(KCONFIG_CONFIG): $(LEGACY_REQUEST) $(if $(wildcard .config),.config,)
	@echo "warning: ARCH=/VM= are deprecated; prefer 'make <board>_defconfig' then 'make qemu'" >&2
	@$(CONFIGURE) $(LEGACY_BASE_ARGS) $(LEGACY_SETS) --out $@
endif

$(KCONFIG_MK): $(KCONFIG_CONFIG) scripts/kconfig/genmk.py $(KCONFIG_TREE)
	@$(GENMK) --config $(KCONFIG_CONFIG) --mk $@

# ————————————————————————— build —————————————————————————
.PHONY: kernel qemu clean distclean init.kpkg

# —— 组件 .kcomp 打包 + 内嵌（Linux insmod/depmod 模式）——
# 每个组件经共享管线 tools/build-kcomp.sh 构建成**链接后的** .kcomp（ET_REL 组件程序）：
# staticlib → rust-lld -r --gc-sections -u kcomp_init → strip → 白名单/重定位契约校验。
# 列表是**相对 os/components 的源码目录**；.kcomp 名取目录 basename（`load <basename>`）。
# Phase 1 不迁移组件选择：列表留在 Makefile，直到 loader + manifest 里程碑。
KCOMP_SRCS := core_test kcomp_smoke scheduler_rr kcomp_panic drivers/virtio_blk driver_prober
# 构建暂存在仓库内的 build/（已 gitignore），不往 /tmp 或别处散。
KPKG_DIR   := $(CURDIR)/build/kpkg
KPKG_BUILD := $(CURDIR)/build/kpkg-build

# 构建所有组件 .kcomp → 统一打包 init.kpkg（cpio newc + manifest）
init.kpkg:
	@rm -rf $(KPKG_DIR)
	@mkdir -p $(KPKG_DIR) $(KPKG_BUILD) $(CURDIR)/tools/qemu
	@for src in $(KCOMP_SRCS); do \
		n=$$(basename $$src); \
		RUSTFLAGS="$(REMAP_RUSTFLAGS)" tools/build-kcomp.sh \
			$(CURDIR)/os/components/$$src $(KCFG_TARGET) \
			$(KPKG_DIR)/$$n.kcomp $(KPKG_BUILD) || exit 1; \
		echo $$n >> $(KPKG_DIR)/manifest; \
	done
	cd $(KPKG_DIR) && find . -type f | cpio -o -H newc --quiet > $(CURDIR)/tools/qemu/init.kpkg
	@echo "packed: tools/qemu/init.kpkg ($(KCOMP_SRCS))"

# 发布形态：kaleidos.elf = bootstrap + core + .initpkg(kpkg 编译期内嵌)
kernel: init.kpkg
	cd $(BOOT_DIR) && RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET) --release
	cp $(KERNEL) $(OUTPUT)
	@echo "built: $(OUTPUT) (features=$(KCFG_BOOT_FEATURES), target=$(KCFG_TARGET))"

# 调试看输出（串口打印 + Ctrl-A X 退出 QEMU）
# -smp 2: 2 核（boot hart 由 OpenSBI 选择）；内存按 profile（rv64=4G，rv32=1G，
# 32 位地址空间放不下 4 GiB RAM，见 configs/ 与 genmk.py 的 KCFG_QEMU_MEM）。
qemu: kernel
	$(KCFG_QEMU) -machine virt -smp 2 -m $(KCFG_QEMU_MEM) -bios default \
		-kernel $(OUTPUT) -nographic

clean:
	rm -f kaleidos-*
	rm -f tools/qemu/init.kpkg
	rm -rf $(CURDIR)/build
	cd $(BOOT_DIR) && cargo clean --release

# 连配置一起清掉（`.config` 是用户数据，clean 不动它）。
distclean: clean
	rm -f .config .config.old .config.mk

# —— 质量工具链（fmt / clippy / check / 测试通道）——
# 测试入口显式分层（testing.md 金字塔落地）：
#   make test-host    host 单测（快速，日常主力，与 .config 无关）
#   make test-build   两个架构的交叉构建门禁
#   make test-qemu-rv64 / test-qemu-rv32   自动 QEMU（boot smoke + 自动 CoreTest）
#   make test-qemu    两个架构都跑
#   make test-driver-prober  driver_prober 组件端到端（positive / no-device / extra-device）
#   make bench        host release 性能基线（手动跑，不进 CI）
#   make check        CI 全量门禁 = fmt + clippy + test-host + test-build
.PHONY: fmt clippy check test-host bench test-build test-build-rv64 test-build-rv32 boot-build boot-check test-qemu test-qemu-rv64 test-qemu-rv32 test-qemu-one test-driver-prober test-driver-prober-rv64 test-driver-prober-rv32 test-driver-prober-one test-arch test-arch-rv64 test-arch-rv32 test-arch-one

# 自己的 crate（显式列出；third_party 是 submodule，不归我们 fmt/clippy）
OUR_CRATES := -p kernel -p arch -p scheduler_rr -p allocator_simple -p core_test -p logger

# 代码格式化（rustfmt）；kcomp-sdk 是独立 workspace（root exclude），单独 fmt。
fmt:
	cargo fmt $(OUR_CRATES)
	cd os/components/kcomp-sdk && cargo fmt
	cd os/components/driver_prober && cargo fmt
	cd os/boot/riscv && cargo fmt

# lint（clippy，只查我们自己：third_party 已 exclude，失败即失败）
# core_test / scheduler_rr 的 lib 是 staticlib（最终产物，需裸机 panic handler），
# host 无法完成其静态链接，因此对它们按真实目标 $(KCFG_TARGET) 做 clippy。
clippy:
	cargo clippy --workspace --all-targets --exclude core_test --exclude scheduler_rr
	cargo clippy -p core_test -p scheduler_rr --target $(KCFG_TARGET)
	cd os/components/kcomp-sdk && cargo clippy --all-targets
	cd os/components/driver_prober && cargo clippy --target $(KCFG_TARGET)

# host 单测：Core truth / parser / property / backend 纯逻辑（不需要 QEMU，不读 .config）
test-host:
	cargo test --workspace
	cd os/components/kcomp-sdk && cargo test
	cd os/components/driver_prober && cargo test

# 性能基线（host release，手动跑）：ns/call 量级；基线用例见 handle/mmio.rs bench_*
bench:
	cargo test --release -p kernel --lib bench -- --ignored --nocapture

# 交叉构建门禁：每个 profile 用一份私有 .config（互不污染，也不动用户的 .config）。
boot-build:
	cd $(BOOT_DIR) && RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET)

boot-check:
	cd $(BOOT_DIR) && cargo check --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET)

test-build-rv64:
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv64/.config qemu_rv64_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv64/.config boot-build

test-build-rv32:
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv32/.config qemu_rv32_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv32/.config boot-check

test-build: test-build-rv64 test-build-rv32

# 自动 QEMU：构建 + 启动 + 自动执行 core_test + 判定 PASS（输出进日志）。
# 每个架构先落到自己的 .config，再在子 make 里按该 profile 构建。
test-qemu-rv64:
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv64/.config qemu_rv64_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv64/.config test-qemu-one

test-qemu-rv32:
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv32/.config qemu_rv32_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv32/.config test-qemu-one

test-qemu-one: kernel
	@python3 tests/qemu/runner.py $(KCFG_ARCH)

test-qemu: test-qemu-rv64 test-qemu-rv32

# driver_prober 端到端：加载 scheduler_rr → driver_prober，prober 自动 load
# virtio_blk；runner 内跑三个场景：
#   positive     挂 1 MiB virtio-blk 盘（MBR 签名 0xaa55 @ 510）→ 读到 sector 0；
#   no-device    不挂盘 → prober 仍加载候选，驱动无支持设备但干净进入 Ready；
#   extra-device 挂两块同类盘 → 首次 attach 生效，不产生第二个实例。
test-driver-prober-rv64:
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv64/.config qemu_rv64_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv64/.config test-driver-prober-one

test-driver-prober-rv32:
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv32/.config qemu_rv32_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv32/.config test-driver-prober-one

test-driver-prober-one: kernel
	@python3 tests/qemu/driver_prober_runner.py $(KCFG_ARCH)

test-driver-prober: test-driver-prober-rv64 test-driver-prober-rv32

# White-box architectural selftests use a separate image: the private archtest
# profile = the board defconfig + configs/selftest.fragment (CONFIG_SELFTEST=y),
# which drives both the boot `selftest` feature and the `-selftest` output name.
test-arch-rv64:
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv64/.config qemu_rv64_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv64/.config selftest_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv64/.config test-arch-one

test-arch-rv32:
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv32/.config qemu_rv32_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv32/.config selftest_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv32/.config test-arch-one

test-arch-one: kernel
	@python3 tests/qemu/arch_runner.py $(KCFG_ARCH)

test-arch: test-arch-rv64 test-arch-rv32

# 一键质量门禁：任何一步失败即整体失败（CI 可直接用）
check: init.kpkg
	cargo fmt $(OUR_CRATES) -- --check
	cd os/components/kcomp-sdk && cargo fmt -- --check
	cd os/components/driver_prober && cargo fmt -- --check
	cd os/boot/riscv && cargo fmt -- --check
	cargo clippy --workspace --all-targets --exclude core_test --exclude scheduler_rr -- -D warnings
	cargo clippy -p core_test -p scheduler_rr --target $(KCFG_TARGET) -- -D warnings
	cd os/components/kcomp-sdk && cargo clippy --all-targets -- -D warnings
	cd os/components/driver_prober && cargo clippy --target $(KCFG_TARGET) -- -D warnings
	$(MAKE) test-host
	$(MAKE) test-build

FORCE:
