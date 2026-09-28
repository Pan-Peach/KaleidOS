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

KCONFIG_MK := $(KCONFIG_CONFIG).mk

# Goals that must NOT create or parse a configuration: host-only tools, and
# `clean` / `rootfs` (both have to work on a fresh checkout where no .config
# exists yet — the FAT image is built by mkfs.vfat/mtools, not by Kconfig).
# `abi-gen` / `abi-check` are pure source transformations (abi/*.toml → C/Rust).
CONFIG_FREE_GOALS := clean distclean help fmt test-host bench _test-kconfig rootfs abi-gen abi-check

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
KERNEL := $(BOOT_DIR)/target/$(KCFG_TARGET)/release/bootstrap
OUTPUT := kaleidos-$(KCFG_ARCH)$(if $(filter y,$(KCFG_SELFTEST)),-selftest,)
# Core-only 开发镜像的产物名：故意与真实镜像区分，永远不会被误当成完整系统。
CORE_OUTPUT := kaleidos-$(KCFG_ARCH)-core

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
	@echo "  make core                        Core-only dev image (skips all components)"
	@echo "  make qemu-core                   run the Core-only dev image (no rootfs drive)"
	@echo "  make rootfs                      build the small FAT image attached to make qemu"
	@echo "  make check | test | test-host | test-qemu | test-arch"
	@echo "  make abi-gen | abi-check         ABI 单一来源：abi/*.toml → 生成 C/Rust（check 只校验）"

# Materialise a configuration on first use.
$(KCONFIG_CONFIG):
	@echo "no $(KCONFIG_CONFIG) yet; creating it from configs/qemu_rv64_defconfig"
	@$(CONFIGURE) --defconfig configs/qemu_rv64_defconfig --out $@

$(KCONFIG_MK): $(KCONFIG_CONFIG) scripts/kconfig/genmk.py $(KCONFIG_TREE)
	@$(GENMK) --config $(KCONFIG_CONFIG) --mk $@

# ————————————————————————— build —————————————————————————
.PHONY: kernel qemu core qemu-core rootfs clean distclean init.kpkg

# —— 组件 .kcomp 打包 + 内嵌（Linux insmod/depmod 模式）——
# 两段管线：语言前端（Rust: tools/build-kcomp.sh / C: tools/build-kcomp-c.sh）
# 各自编出 .o/.a，再交给语言无关的 tools/kcomp-link.sh 做 partial link +
# --gc-sections + -u 入口 → strip → 白名单/重定位契约校验，产出 ET_REL .kcomp。
# 列表是**相对 os/components 的源码目录**；.kcomp 名取目录 basename（`load <basename>`），
# 因此 test-only fixture 可以整体挪进 tests/ 而不改组件名。test-only fixture/组件
# 一律放 os/components/tests/（见 AGENTS.md），生产组件留在 os/components/。
# Phase 1 不迁移组件选择：列表留在 Makefile，直到 loader + manifest 里程碑。
KCOMP_SRCS   := tests/core_test tests/kcomp_smoke scheduler_rr tests/kcomp_panic tests/kcomp_isolated tests/kcomp_isolated_life tests/kcomp_isolated_svc tests/kcomp_isolated_bad tests/kcomp_isolated_direct tests/kcomp_isolated_unsupported drivers/virtio_blk driver_prober kbench tests/drivers/ram_blk tests/drivers/ram_blk_rw
# C 组件（freestanding，clang 前端；可选用 kcomp-c-src.txt 列 third_party 源文件）。
# SDK 的 C 运行时（kcomp-sdk/c/*.c）由 build-kcomp-c.sh 自动随每个 C 组件编入。
KCOMP_C_SRCS := tests/kcomp_c_smoke filesystems/fatfs filesystems/littlefs
# 构建暂存在仓库内的 build/（已 gitignore），不往 /tmp 或别处散。
KPKG_DIR     := $(CURDIR)/build/kpkg
KPKG_BUILD   := $(CURDIR)/build/kpkg-build
KPKG_BUILD_C := $(CURDIR)/build/kpkg-build-c

# 构建所有组件 .kcomp → 统一打包 init.kpkg（cpio newc + manifest）
init.kpkg:
	@rm -rf $(KPKG_DIR)
	@mkdir -p $(KPKG_DIR) $(KPKG_BUILD) $(KPKG_BUILD_C) $(CURDIR)/tools/qemu
	@for src in $(KCOMP_SRCS); do \
		n=$$(basename $$src); \
		RUSTFLAGS="$(REMAP_RUSTFLAGS)" tools/build-kcomp.sh \
			$(CURDIR)/os/components/$$src $(KCFG_TARGET) \
			$(KPKG_DIR)/$$n.kcomp $(KPKG_BUILD) || exit 1; \
		echo $$n >> $(KPKG_DIR)/manifest; \
	done
	@for src in $(KCOMP_C_SRCS); do \
		n=$$(basename $$src); \
		if [ "$$n" = "fatfs" ]; then \
			CFLAGS="$${CFLAGS:-} -I$(CURDIR)/third_party/fatfs/source -include $(CURDIR)/os/components/filesystems/fatfs/ffconf.h" \
			tools/build-kcomp-c.sh \
				$(CURDIR)/os/components/$$src $(KCFG_TARGET) \
				$(KPKG_DIR)/$$n.kcomp $(KPKG_BUILD_C) || exit 1; \
		elif [ "$$n" = "littlefs" ]; then \
			CFLAGS="$${CFLAGS:-} -I$(CURDIR)/third_party/littlefs -DLFS_NO_MALLOC -DLFS_NO_ASSERT -DLFS_NO_DEBUG -DLFS_NO_WARN -DLFS_NO_ERROR" \
			tools/build-kcomp-c.sh \
				$(CURDIR)/os/components/$$src $(KCFG_TARGET) \
				$(KPKG_DIR)/$$n.kcomp $(KPKG_BUILD_C) || exit 1; \
		else \
			tools/build-kcomp-c.sh \
				$(CURDIR)/os/components/$$src $(KCFG_TARGET) \
				$(KPKG_DIR)/$$n.kcomp $(KPKG_BUILD_C) || exit 1; \
		fi; \
		echo $$n >> $(KPKG_DIR)/manifest; \
	done
	cd $(KPKG_DIR) && find . -type f | cpio -o -H newc --quiet > $(CURDIR)/tools/qemu/init.kpkg
	@echo "packed: tools/qemu/init.kpkg ($(KCOMP_SRCS) $(KCOMP_C_SRCS))"

# 发布形态：kaleidos.elf = bootstrap + core + .initpkg(kpkg 编译期内嵌)
# CONFIG_TRACE_CAPACITY：Kconfig 的 TRACE_CAPACITY 由生成的片段镜像成
# CONFIG_TRACE_CAPACITY；这不是 Cargo feature，作为环境变量传给 os/core/build.rs
# 校验后写入 OUT_DIR 常量（Kconfig 仍是唯一真相，见 docs/architecture/kconfig.md）。
kernel: init.kpkg
	cd $(BOOT_DIR) && CONFIG_TRACE_CAPACITY="$(CONFIG_TRACE_CAPACITY)" RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET) --release
	cp $(KERNEL) $(OUTPUT)
	@echo "built: $(OUTPUT) (features=$(KCFG_BOOT_FEATURES), target=$(KCFG_TARGET))"

# —— Core-only 开发镜像：跳过全部组件，只验证 Core + boot 能否在真实目标启动 ——
# 用途：`os/components/**` 暂时损坏时，仍能构建 / 启动 / host 测试 Core。
# KALEIDOS_CORE_ONLY=1 让 os/core/build.rs 跳过 fixture 组件构建（core_test /
# kcomp_smoke / kcomp_min）——依赖它们的测试被 cfg(no_kcomp) 门控。
# boot crate 用 include_bytes!("../../../../tools/qemu/init.kpkg") 内嵌组件归档，
# 所以这里必须让它存在且是合法归档：空 newc 归档只含 TRAILER!!!，store 解析到
# trailer 即返回空目录，boot 不会 eager 解析 manifest，空 catalog 也能启动。
# init.kpkg 是 .PHONY，下一次 `make kernel` 会无条件重建真实归档——本目标对它的
# 覆盖是自愈的。
core:
	@mkdir -p $(CURDIR)/tools/qemu
	printf '' | cpio -o -H newc --quiet > $(CURDIR)/tools/qemu/init.kpkg
	cd $(BOOT_DIR) && KALEIDOS_CORE_ONLY=1 CONFIG_TRACE_CAPACITY="$(CONFIG_TRACE_CAPACITY)" RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET) --release
	cp $(KERNEL) $(CORE_OUTPUT)
	@echo "built: $(CORE_OUTPUT) (core-only, features=$(KCFG_BOOT_FEATURES), target=$(KCFG_TARGET))"
	@echo "WARNING: tools/qemu/init.kpkg is now EMPTY; the next 'make kernel' rebuilds the real one (init.kpkg is .PHONY)."

# —— 交互运行默认挂载的小 FAT 盘（块设备驱动有真实设备可读）——
# 文本源在 tests/fixtures/rootfs/（mtools 按目录树原样拷进镜像根），成品进
# build/（已 gitignore，不提交二进制）。`--invariant` 让镜像字节可复现。
# 仅在源文件较新时重建：先写 .tmp 再改名，mkfs/mcopy 中途失败不会留下
# "比源新" 的坏镜像挡住下一次重试（幂等、便宜）。
ROOTFS_DIR  := tests/fixtures/rootfs
ROOTFS_MB   := 8
ROOTFS      := $(CURDIR)/build/rootfs.fat
ROOTFS_SRCS := $(shell find $(ROOTFS_DIR) -type f)

rootfs: $(ROOTFS)

$(ROOTFS): $(ROOTFS_SRCS)
	@mkdir -p $(@D)
	dd if=/dev/zero of=$@.tmp bs=1M count=$(ROOTFS_MB) status=none
	mkfs.vfat --invariant -n KALEIDOS $@.tmp
	mcopy -s -i $@.tmp $(ROOTFS_DIR)/* ::
	mv -f $@.tmp $@

# 调试看输出（串口打印 + Ctrl-A X 退出 QEMU）
# -smp 2: 2 核（boot hart 由 OpenSBI 选择）；内存按 profile（rv64=4G，rv32=1G，
# 32 位地址空间放不下 4 GiB RAM，见 configs/ 与 genmk.py 的 KCFG_QEMU_MEM）。
# 默认挂上 ROOTFS（virtio-blk）：第 0 扇区是 FAT 引导记录（510=0x55, 511=0xAA），
# virtio_blk 因此有真实设备可读；自动化测试不用它（test-* 各自起 QEMU）。
qemu: kernel rootfs
	$(KCFG_QEMU) -machine virt -smp 2 -m $(KCFG_QEMU_MEM) -bios default \
		-kernel $(OUTPUT) -nographic \
		-drive file=$(ROOTFS),if=none,format=raw,id=rootfs \
		-device virtio-blk-device,drive=rootfs

# Core-only 镜像没有组件可读盘，因此不挂 rootfs drive（其余 flags 与 qemu 一致）。
qemu-core: core
	$(KCFG_QEMU) -machine virt -smp 2 -m $(KCFG_QEMU_MEM) -bios default \
		-kernel $(CORE_OUTPUT) -nographic

clean:
	rm -f kaleidos-*
	rm -f tools/qemu/init.kpkg
	rm -rf $(CURDIR)/build
	cd $(BOOT_DIR) && cargo clean --release

# 连配置一起清掉（`.config` 是用户数据，clean 不动它）。
distclean: clean
	rm -f .config .config.old .config.mk

# —— 质量工具链（fmt / clippy / check / 测试通道）——
# 公开测试入口只有 5 个（`make help` 只列这些）：
#   make check      CI 快车道 = fmt + clippy + abi-check + Kconfig 胶水 + host 单测 + 交叉构建
#   make test       完整测试 = test-host + test-qemu + test-arch
#   make test-host  host 单测（快速，日常主力，与 .config 无关）
#   make test-qemu  自动 QEMU（RV64 + RV32；boot smoke + 自动 CoreTest）
#   make test-arch  ArchTest 白盒 selftest（RV64 + RV32）
# 其余测试目标都是内部助手：`_` 前缀，不进 `make help`，也不是对外契约。
.PHONY: fmt clippy check abi-gen abi-check test test-host test-qemu _test-qemu-rv64 _test-qemu-rv32 _test-qemu-one test-arch _test-arch-rv64 _test-arch-rv32 _test-arch-one _test-arch-x86_64 _test-arch-aarch64 _test-arch-loongarch64 test-arch-x86_64 test-arch-aarch64 test-arch-loongarch64 test-arch-new _test-arch-core-one _test-arch-smp-rv64 _test-arch-smp-one test-arch-smp-rv64 smp_defconfig _test-build _test-kconfig bench boot-build boot-check

# 自己的 crate（显式列出；third_party 是 submodule，不归我们 fmt/clippy）
OUR_CRATES := -p kernel -p arch -p scheduler_rr -p core_test

# 代码格式化（rustfmt）；kcomp-sdk 是独立 workspace（root exclude），单独 fmt。
fmt:
	cargo fmt $(OUR_CRATES)
	cd os/components/kcomp-sdk && cargo fmt
	cd os/components/driver_prober && cargo fmt
	cd os/components/kbench && cargo fmt
	cd os/components/tests/drivers/ram_blk && cargo fmt
	cd os/components/tests/drivers/ram_blk_rw && cargo fmt
	cd os/boot/riscv && cargo fmt
	cd os/boot/x86_64 && cargo fmt
	cd os/boot/aarch64 && cargo fmt
	cd os/boot/loongarch64 && cargo fmt

# lint（clippy，只查我们自己：third_party 已 exclude，失败即失败）
# core_test / scheduler_rr 的 lib 是 staticlib（最终产物，需裸机 panic handler），
# host 无法完成其静态链接，因此对它们按真实目标 $(KCFG_TARGET) 做 clippy。
clippy:
	cargo clippy --workspace --all-targets --exclude core_test --exclude scheduler_rr
	cargo clippy -p core_test -p scheduler_rr --target $(KCFG_TARGET)
	cd os/components/kcomp-sdk && cargo clippy --all-targets
	cd os/components/driver_prober && cargo clippy --target $(KCFG_TARGET)
	cd os/components/kbench && cargo clippy --target $(KCFG_TARGET)
	cd os/components/tests/drivers/ram_blk && cargo clippy --target $(KCFG_TARGET)
	cd os/components/tests/drivers/ram_blk_rw && cargo clippy --target $(KCFG_TARGET)

# host 单测：Core truth / parser / property / backend 纯逻辑（不需要 QEMU，不读 .config）
test-host:
	cargo test --workspace
	cd os/components/kcomp-sdk && cargo test
	cd os/components/driver_prober && cargo test
	cd os/components/kbench && cargo test
	cd os/components/tests/drivers/ram_blk && cargo test

# 内部：Kconfig / Makefile 胶水契约（host-only，快速；见 tests/kconfig/test_glue.py）。
_test-kconfig:
	python3 tests/kconfig/test_glue.py

# —— KABI：ABI 单一来源生成（abi/*.toml → C / SDK-Rust / Core-Rust）——
# 生成物是**提交物**：普通构建只消费它们，绝不在 build 期生成。
# abi-gen 重生成（幂等）；abi-check 重生成到临时目录并逐文件 diff —— 内容漂移、
# 生成文件缺失、生成目录里出现计划外文件都会响失败（`make check` 已并入）。
KABI_GEN := python3 tools/kabi/kabi_gen.py
KABI_SCHEMAS := --schema abi/component.toml --schema abi/core.toml --schema abi/errno.toml --schema abi/block.toml --schema abi/filesystem.toml --schema abi/probe.toml --schema abi/scheduler.toml

abi-gen:
	$(KABI_GEN) generate $(KABI_SCHEMAS) --out-root .

abi-check:
	$(KABI_GEN) selftest
	$(KABI_GEN) check $(KABI_SCHEMAS) --out-root .

# 性能基线（host release，手动跑）：统一走 kernel::bench harness（见 os/core/src/bench）。
# - trace 关掉：CONFIG_TRACE 的探针正好落在被测路径上，开着会污染数字
#   （顺带也就验证了"关掉即零成本"）。
# - 带上 git commit：BENCH-ENV 行才能把数字和代码版本对上。
# - `--test-threads=1`：bench 必须串行跑，否则多个 primitive 的 stdout 会交错，
#   报告就不再是机器可解析的（而且并行本身也会互相污染计时）。
bench:
	KALEIDOS_GIT_COMMIT="$$(git rev-parse --short=12 HEAD)" \
		cargo test --release -p kernel --lib \
			--no-default-features --features supervisor,vm-mmu \
			bench -- --ignored --nocapture --test-threads=1

# 交叉构建门禁：每个 profile 用一份私有 .config（互不污染，也不动用户的 .config）。
boot-build:
	cd $(BOOT_DIR) && CONFIG_TRACE_CAPACITY="$(CONFIG_TRACE_CAPACITY)" RUSTFLAGS="$(BOOT_RUSTFLAGS)" cargo build --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET)

boot-check:
	cd $(BOOT_DIR) && CONFIG_TRACE_CAPACITY="$(CONFIG_TRACE_CAPACITY)" cargo check --no-default-features --features $(KCFG_BOOT_FEATURES) --target $(KCFG_TARGET)

# 内部：两个架构的交叉构建门禁（`make check` 的一步）。
_test-build:
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv64/.config qemu_rv64_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv64/.config boot-build
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv32/.config qemu_rv32_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv32/.config boot-check

# 自动 QEMU：构建 + 启动 + 自动执行 core_test + 判定 PASS（输出进日志）。
# 每个架构先落到自己的 .config，再在子 make 里按该 profile 构建。
# runner 按 QEMU 机器拓扑跑两个场景（场景只控制硬件；判定在 CoreTest 内）：
#   default   挂 virtio-rng + 两块 1 MiB virtio-blk：prober 首个 Match 后 attach，
#             第二块盘验证拒绝二次 attach；
#   no-block  只挂 virtio-rng（无块设备）：prober 对每个候选 create + pull NoMatch、
#             干净结束，CoreTest 断言 NoMatch 路径而不是 attach。
_test-qemu-rv64:
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv64/.config qemu_rv64_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv64/.config _test-qemu-one

_test-qemu-rv32:
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv32/.config qemu_rv32_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/qemu-rv32/.config _test-qemu-one

_test-qemu-one: kernel
	@python3 tests/qemu/runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) --scenario default
	@python3 tests/qemu/runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) --scenario no-block

# 公开入口：两个架构各跑两个场景。
test-qemu: _test-qemu-rv64 _test-qemu-rv32

# White-box architectural selftests use a separate image: the private archtest
# profile = the board defconfig + configs/selftest.fragment (CONFIG_SELFTEST=y),
# which drives both the boot `selftest` feature and the `-selftest` output name.
_test-arch-rv64:
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv64/.config qemu_rv64_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv64/.config selftest_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv64/.config _test-arch-one

_test-arch-rv32:
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv32/.config qemu_rv32_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv32/.config selftest_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-rv32/.config _test-arch-one

_test-arch-one: kernel
	@python3 tests/qemu/arch_runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT)

# 公开入口：两个架构都跑。
test-arch: _test-arch-rv64 _test-arch-rv32

# Opt-in new-ISA ArchTest (skeleton).  These FAIL until the ISA boot path
# (os/boot/<arch>) is implemented — that is the point.  Deliberately NOT part of
# `test-arch` / `test` / `check`, so the RISC-V baseline and CI stay green.
# New-ISA targets use the Core-only image: `.kcomp` components are still
# RISC-V-relocation-only, so building the full `kernel` (init.kpkg) for a new ISA
# would fail.  The skeleton selftest cases need no components.
_test-arch-core-one: core
	@python3 tests/qemu/arch_runner.py --arch $(KCFG_ARCH) --kernel $(CORE_OUTPUT)

_test-arch-x86_64:
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-x86_64/.config qemu_x86_64_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-x86_64/.config selftest_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-x86_64/.config _test-arch-core-one

_test-arch-aarch64:
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-aarch64/.config qemu_aarch64_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-aarch64/.config selftest_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-aarch64/.config _test-arch-core-one

_test-arch-loongarch64:
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-loongarch64/.config qemu_loongarch64_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-loongarch64/.config selftest_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-loongarch64/.config _test-arch-core-one

test-arch-x86_64: _test-arch-x86_64
test-arch-aarch64: _test-arch-aarch64
test-arch-loongarch64: _test-arch-loongarch64
# All three skeleton ISAs at once (expected to fail until implemented).
test-arch-new: _test-arch-x86_64 _test-arch-aarch64 _test-arch-loongarch64

# Opt-in RISC-V SMP ArchTest (CONFIG_SMP=y).  The three `smp-*` cases are
# `todo!()` until SMP is implemented, so this is NOT part of `test-arch`.
smp_defconfig:
	@$(CONFIGURE) --base $(KCONFIG_CONFIG) --fragment configs/smp.fragment --out $(KCONFIG_CONFIG)

_test-arch-smp-rv64:
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-smp-rv64/.config qemu_rv64_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-smp-rv64/.config selftest_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-smp-rv64/.config smp_defconfig
	@$(MAKE) KCONFIG_CONFIG=build/configs/archtest-smp-rv64/.config _test-arch-smp-one

_test-arch-smp-one: kernel
	@python3 tests/qemu/arch_runner.py --arch $(KCFG_ARCH) --kernel $(OUTPUT) --smp

test-arch-smp-rv64: _test-arch-smp-rv64

# 完整测试：host 单测 + 两个架构的 QEMU CoreTest + ArchTest。
test: test-host test-qemu test-arch

# 一键质量门禁：任何一步失败即整体失败（CI 可直接用）
check: init.kpkg
	cargo fmt $(OUR_CRATES) -- --check
	cd os/components/kcomp-sdk && cargo fmt -- --check
	cd os/components/driver_prober && cargo fmt -- --check
	cd os/components/kbench && cargo fmt -- --check
	cd os/components/tests/drivers/ram_blk && cargo fmt -- --check
	cd os/boot/riscv && cargo fmt -- --check
	cd os/boot/x86_64 && cargo fmt -- --check
	cd os/boot/aarch64 && cargo fmt -- --check
	cd os/boot/loongarch64 && cargo fmt -- --check
	cargo clippy --workspace --all-targets --exclude core_test --exclude scheduler_rr -- -D warnings
	cargo clippy -p core_test -p scheduler_rr --target $(KCFG_TARGET) -- -D warnings
	cd os/components/kcomp-sdk && cargo clippy --all-targets -- -D warnings
	cd os/components/driver_prober && cargo clippy --target $(KCFG_TARGET) -- -D warnings
	cd os/components/kbench && cargo clippy --target $(KCFG_TARGET) -- -D warnings
	cd os/components/tests/drivers/ram_blk && cargo clippy --target $(KCFG_TARGET) -- -D warnings
	$(MAKE) _test-kconfig
	$(MAKE) abi-check
	$(MAKE) test-host
	$(MAKE) _test-build

FORCE:
