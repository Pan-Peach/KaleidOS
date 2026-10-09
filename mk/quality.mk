# Shared development inventories live in components.mk.
.PHONY: fmt fmt-check clippy check test-host test-tools host-fixtures _host-fixture-package _test-kconfig bench abi-gen abi-check
CARGO_CHECKS := python3 scripts/build/cargo_checks.py
HOST_FIXTURE_DIR := $(CURDIR)/build/host-fixtures
TEST_ENV = KALEIDOS_TEST_FIXTURES="$(HOST_FIXTURE_DIR)/components" KALEIDOS_EXEC_FIXTURES="$(BUILD_DIR)/exec-fixtures"

fmt:
	$(CARGO_CHECKS) fmt $(ALL_RUST_CRATES)
fmt-check:
	$(CARGO_CHECKS) fmt-check $(ALL_RUST_CRATES)
clippy: host-fixtures $(if $(filter rv64,$(KCFG_ARCH)),exec-fixtures,)
	KALEIDOS_TEST_FIXTURES="$(HOST_FIXTURE_DIR)/components" $(CARGO_CHECKS) clippy os/core os/arch os/components/kcomp-sdk
	KALEIDOS_EXEC_FIXTURES="$(BUILD_DIR)/exec-fixtures" $(CARGO_CHECKS) clippy --target $(KCFG_TARGET) $(TARGET_CRATES)

# Real linked fixtures are prepared only for host integration tests.
host-fixtures:
	@$(MAKE) O=$(HOST_FIXTURE_DIR) qemu_rv64_defconfig
	@$(MAKE) O=$(HOST_FIXTURE_DIR) _host-fixture-package
_host-fixture-package: exec-fixtures
	$(PACKAGE) --output $(BUILD_DIR)/host.kpkg --rust $(HOST_FIXTURE_RUST) --catalog kcomp_smoke
	cp $(BUILD_DIR)/host.kpkg $(BUILD_DIR)/components/init.kpkg

test-host: test-tools host-fixtures
	KALEIDOS_TEST_FIXTURES="$(HOST_FIXTURE_DIR)/components" cargo test --workspace --features kernel/test-fixtures
	$(CARGO_CHECKS) test $(HOST_CRATES)

test-tools: _test-kconfig abi-check
	python3 tests/compat/test_runner.py
	python3 -m unittest discover -s tests/build -p 'test_*.py'
	python3 -m unittest discover -s tests/qemu -p 'test_*.py'
_test-kconfig:
	python3 tests/kconfig/test_glue.py

check: fmt-check clippy test-host
	$(MAKE) _test-build

# Compatibility applications are host-built references, independent of the
# kernel profile. The explicit testcase corpus is tests/compat/cases.txt.
COMPAT_LINUX_CC ?= cc
COMPAT_WINDOWS_CC ?= x86_64-w64-mingw32-gcc
COMPAT_LINKAGE ?= static
COMPAT_RUNNER := python3 tests/compat/runner.py
.PHONY: compat-linux compat-windows test-compat-linux test-compat-windows compat-package
compat-linux:
	$(COMPAT_RUNNER) build --target linux --cc "$(COMPAT_LINUX_CC)" --linkage $(COMPAT_LINKAGE)

compat-windows:
	$(COMPAT_RUNNER) build --target windows --cc "$(COMPAT_WINDOWS_CC)" --linkage $(COMPAT_LINKAGE)

test-compat-linux:
	$(COMPAT_RUNNER) test --target linux --cc "$(COMPAT_LINUX_CC)" --linkage $(COMPAT_LINKAGE)

test-compat-windows:
	$(COMPAT_RUNNER) test --target windows --cc "$(COMPAT_WINDOWS_CC)" --linkage $(COMPAT_LINKAGE)

compat-package:
	$(COMPAT_RUNNER) package

# —— KABI：ABI 单一来源生成（abi/*.toml → C / SDK-Rust / Core-Rust）——
# 生成物是**提交物**：普通构建只消费它们，绝不在 build 期生成。
# abi-gen 重生成（幂等）；abi-check 重生成到临时目录并逐文件 diff —— 内容漂移、
# 生成文件缺失、生成目录里出现计划外文件都会响失败（`make check` 已并入）。
KABI_GEN := python3 tools/kabi/kabi_gen.py
KABI_SCHEMAS := --schema abi/component.toml --schema abi/core.toml --schema abi/errno.toml --schema abi/block.toml --schema abi/echo.toml --schema abi/filesystem.toml --schema abi/vfs.toml --schema abi/network.toml --schema abi/posix.toml --schema abi/probe.toml --schema abi/scheduler.toml

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
