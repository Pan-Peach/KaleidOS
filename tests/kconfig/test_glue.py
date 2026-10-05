#!/usr/bin/env python3
"""Kconfig / Makefile 胶水契约测试（host-only，快速；`make _test-kconfig` 调用）。

只测这一层胶水，不编译任何 Rust：

- `configure.py`：显式请求（`--set` / defconfig / fragment）解析后必须存活，
  否则报错退出，且不写一份会撒谎的 `.config`；
- `genmk.py`：一个数字只有一个名字（`CONFIG_<symbol>` 镜像，不再有
  `KCFG_TRACE_CAPACITY`）；
- `Makefile`：`clean kernel` / `fmt check` 这类混合 goal 里的 build goal 仍拿到
  `KCFG_*`；config-free goal 在全新 checkout 上不创建 `.config`；`ARCH=` / `VM=`
  已经不起作用（没有兼容层）。

所有用例只在临时目录里写配置（`KCONFIG_CONFIG` 指向 tmp），不碰仓库的
`.config` 和 `build/`；`make` 相关用例一律用 `-n`（只解析、不执行构建），断言
打印出的命令行。
"""

import os
import re
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CONFIGURE = [sys.executable, "scripts/kconfig/configure.py", "--kconfig", "Kconfig"]
GENMK = [sys.executable, "scripts/kconfig/genmk.py", "--kconfig", "Kconfig"]


class CheckFailed(Exception):
    pass


def run(command):
    return subprocess.run(command, cwd=REPO, capture_output=True, text=True)


def read(path):
    with open(path) as source:
        return source.read()


def configure(config, *sources):
    """Run configure.py; `sources` are its --defconfig/--fragment/--set arguments."""
    return run(CONFIGURE + list(sources) + ["--out", config])


def expect_ok(result, what):
    if result.returncode != 0:
        raise CheckFailed(f"{what}: expected success\n{result.stderr}")


def expect_fail(result, what, *needles):
    if result.returncode == 0:
        raise CheckFailed(f"{what}: expected failure, got success")
    for needle in needles:
        if needle not in result.stderr:
            raise CheckFailed(f"{what}: stderr misses {needle!r}\n{result.stderr}")


def mk_var(text, name):
    """Value of an `override NAME := ...` line in a generated fragment."""
    match = re.search(rf"^override {name}[ \t]*:=[ \t]*(.*)$", text, re.MULTILINE)
    if match is None:
        raise CheckFailed(f"{name} missing from the generated fragment")
    return match.group(1).strip()


def build_config(directory, board):
    """Resolve configs/<board>_defconfig into <directory>/.config + .config.mk."""
    os.makedirs(directory, exist_ok=True)
    config = os.path.join(directory, ".config")
    expect_ok(configure(config, "--defconfig", f"configs/{board}_defconfig"), board)
    expect_ok(run(GENMK + ["--config", config, "--mk", config + ".mk"]), board)
    return config


def check_profiles_and_archtest_stack_resolve(tmp):
    """三个 profile 与 test-arch 的 fragment 栈都能解析（防过度拒绝）。"""
    for board, arch, target in (
        ("qemu_rv64", "rv64", "riscv64gc-unknown-none-elf"),
        ("qemu_rv32", "rv32", "riscv32imac-unknown-none-elf"),
        ("qemu_rv32_nommu", "rv32", "riscv32imac-unknown-none-elf"),
        # New-ISA skeletons: the profiles must resolve and map to the right
        # KCFG_TARGET (they are not built by CI until brought up).
        ("qemu_x86_64", "x86_64", "x86_64-unknown-none"),
        ("qemu_aarch64", "aarch64", "aarch64-unknown-none"),
        ("qemu_loongarch64", "loongarch64", "loongarch64-unknown-none"),
    ):
        config = build_config(os.path.join(tmp, board), board)
        text = read(config + ".mk")
        if mk_var(text, "KCFG_ARCH") != arch or mk_var(text, "KCFG_TARGET") != target:
            raise CheckFailed(f"{board}: wrong KCFG_ARCH / KCFG_TARGET")
    for arch in ("rv64", "rv32"):
        directory = os.path.join(tmp, f"archtest-{arch}")
        config = build_config(directory, f"qemu_{arch}")
        expect_ok(
            configure(config, "--base", config, "--fragment",
                      "configs/selftest.fragment"),
            f"archtest-{arch} selftest stack",
        )
        expect_ok(run(GENMK + ["--config", config, "--mk", config + ".mk"]),
                  f"archtest-{arch} selftest stack")
        if mk_var(read(config + ".mk"), "KCFG_SELFTEST") != "y":
            raise CheckFailed(f"archtest-{arch}: KCFG_SELFTEST is not y")


def check_nommu_selftest_fragment_fails(tmp):
    """NoMMU profile 上叠 selftest fragment 必须报错（SELFTEST depends on VM_MMU）。"""
    config = os.path.join(tmp, ".config")
    result = configure(config, "--defconfig", "configs/qemu_rv32_nommu_defconfig",
                       "--fragment", "configs/selftest.fragment")
    expect_fail(result, "NoMMU + selftest.fragment", "CONFIG_SELFTEST", "depends on")
    if os.path.exists(config):
        raise CheckFailed("a config that lies was written anyway")


def check_rv64_nommu_set_fails(tmp):
    """RV64 + NoMMU 请求不可表达：--set 源也必须失败。"""
    expect_fail(
        configure(os.path.join(tmp, ".config"), "--defconfig",
                  "configs/qemu_rv64_defconfig", "--set", "CONFIG_VM_NOMMU=y"),
        "RV64 + NoMMU via --set", "CONFIG_VM_NOMMU")


def check_rv64_nommu_fragment_fails(tmp):
    """同一个请求走 fragment 源（被 `depends on` 截断）也要失败。"""
    fragment = os.path.join(tmp, "nommu.fragment")
    with open(fragment, "w") as out:
        out.write("# stale request that cannot survive on RV64\nCONFIG_VM_NOMMU=y\n")
    expect_fail(
        configure(os.path.join(tmp, ".config"), "--defconfig",
                  "configs/qemu_rv64_defconfig", "--fragment", fragment),
        "RV64 + NoMMU via fragment", "CONFIG_VM_NOMMU")


def check_unknown_set_fails(tmp):
    """--set 未知 symbol 直接失败（不写 config）。"""
    expect_fail(
        configure(os.path.join(tmp, ".config"), "--defconfig",
                  "configs/qemu_rv64_defconfig", "--set", "CONFIG_NO_SUCH_SYMBOL=y"),
        "unknown --set", "CONFIG_NO_SUCH_SYMBOL")


def check_trace_capacity_has_one_name(tmp):
    """genmk 只输出 CONFIG_TRACE_CAPACITY，不再造 KCFG_TRACE_CAPACITY。"""
    config = build_config(tmp, "qemu_rv64")
    text = read(config + ".mk")
    if mk_var(text, "CONFIG_TRACE_CAPACITY") != "1024":
        raise CheckFailed(f"CONFIG_TRACE_CAPACITY is not 1024:\n{text}")
    if "KCFG_TRACE_CAPACITY" in text:
        raise CheckFailed("genmk still emits the bespoke KCFG_TRACE_CAPACITY")


def check_clean_kernel_gets_config(tmp):
    """`make clean kernel` 里 kernel 仍拿到 KCFG_*（豁免只对整条命令成立）。"""
    config = build_config(tmp, "qemu_rv64")
    result = run(["make", "-n", f"KCONFIG_CONFIG={config}", "clean", "kernel"])
    expect_ok(result, "make clean kernel")
    if "--features supervisor,vm-mmu" not in result.stdout:
        raise CheckFailed("kernel lost its config in `make clean kernel`:\n"
                          + result.stdout)


def check_initial_component_selection(tmp):
    """The profile owns boot selection, including explicit monitor fallback."""
    config = build_config(tmp, "qemu_rv64")
    if mk_var(read(config + ".mk"), "CONFIG_BOOT_COMPONENT") != "init":
        raise CheckFailed("normal RISC-V profile does not select init")
    result = run(["make", "-n", f"KCONFIG_CONFIG={config}",
                  "CONFIG_BOOT_COMPONENT=ignored", "kernel"])
    expect_ok(result, "boot component transport")
    if 'CONFIG_BOOT_COMPONENT="init"' not in result.stdout:
        raise CheckFailed("boot component lost its resolved config:\n" + result.stdout)
    expect_ok(configure(config, "--base", config, "--fragment",
                        "configs/monitor.fragment"), "monitor fragment")
    expect_ok(run(GENMK + ["--config", config, "--mk", config + ".mk"]), "monitor")
    if mk_var(read(config + ".mk"), "CONFIG_BOOT_COMPONENT") != "":
        raise CheckFailed("monitor fragment did not disable initial composition")
    for name in ["../init", "init;echo", "$(shell false)"]:
        expect_ok(configure(config, "--base", config, "--set",
                            f'CONFIG_BOOT_COMPONENT="{name}"'), "invalid basename config")
        expect_fail(run(GENMK + ["--config", config, "--mk", config + ".mk"]),
                    "unsafe boot component", "BOOT_COMPONENT", "basename")


def check_fmt_check_gets_config(tmp):
    """`make fmt check` 里 check 仍拿到 KCFG_*（config 驱动的递归构建门禁）。

    只断言契约本身：混合 goal 里 `check` 仍以递归 make 调用配置驱动的构建门禁
    （`_test-build`），不钉死任何 recipe 文本（例如 clippy 的 target triple）。

    MAKE=true：-n 下含 `$(MAKE)` 的递归行仍会被执行，把它变成 no-op，避免
    测试递归进 test-host / _test-build（那会碰共享 build/）。
    """
    config = build_config(tmp, "qemu_rv64")
    result = run(["make", "-n", f"KCONFIG_CONFIG={config}", "MAKE=true", "fmt", "check"])
    expect_ok(result, "make fmt check")
    if "_test-build" not in result.stdout:
        raise CheckFailed(
            "check no longer drives the config-dependent recursive build "
            "(_test-build) in `make fmt check`:\n" + result.stdout
        )


def check_clean_on_fresh_checkout(tmp):
    """没有 .config 时 `make clean` 仍可用，且不创建 .config。"""
    config = os.path.join(tmp, "fresh", ".config")
    result = run(["make", "-n", f"KCONFIG_CONFIG={config}", "clean"])
    expect_ok(result, "make clean (no .config)")
    if os.path.exists(config):
        raise CheckFailed("`make clean` created a .config on a fresh checkout")
    if "rm -f " not in result.stdout or "kaleidos.elf" not in result.stdout:
        raise CheckFailed("`make clean` did not evaluate its recipe:\n" + result.stdout)


def check_fresh_tree_autocreates_default_profile(tmp):
    """没有 .config 时 build goal 仍自举出默认 profile（qemu_rv64）。"""
    config = os.path.join(tmp, "fresh", ".config")
    result = run(["make", "-n", f"KCONFIG_CONFIG={config}", "kernel"])
    expect_ok(result, "make kernel (no .config)")
    if not os.path.exists(config):
        raise CheckFailed("the default-profile fallback no longer creates .config")
    text = read(config)
    if "CONFIG_ARCH_RISCV64=y" not in text or "CONFIG_VM_MMU=y" not in text:
        raise CheckFailed("the auto-created config is not qemu_rv64:\n" + text)


def check_arch_vm_vars_have_no_effect(tmp):
    """命令行 ARCH= / VM= 不再参与配置，也不打印弃用警告。"""
    config = build_config(tmp, "qemu_rv64")
    result = run(["make", "-n", f"KCONFIG_CONFIG={config}",
                  "ARCH=rv32", "VM=nommu", "qemu"])
    expect_ok(result, "make ARCH=rv32 VM=nommu qemu")
    if "riscv64gc-unknown-none-elf" not in result.stdout or "qemu-system-riscv64" not in result.stdout:
        raise CheckFailed("the resolved config did not drive the build:\n" + result.stdout)
    if "riscv32imac-unknown-none-elf" in result.stdout or "deprecated" in result.stderr.lower():
        raise CheckFailed("ARCH=/VM= still influence the build:\n" + result.stdout)


CHECKS = (
    check_profiles_and_archtest_stack_resolve,
    check_nommu_selftest_fragment_fails,
    check_rv64_nommu_set_fails,
    check_rv64_nommu_fragment_fails,
    check_unknown_set_fails,
    check_trace_capacity_has_one_name,
    check_clean_kernel_gets_config,
    check_initial_component_selection,
    check_fmt_check_gets_config,
    check_clean_on_fresh_checkout,
    check_fresh_tree_autocreates_default_profile,
    check_arch_vm_vars_have_no_effect,
)


def main() -> int:
    failed = []
    for check in CHECKS:
        with tempfile.TemporaryDirectory(prefix="kaleidos-kcfg-") as tmp:
            try:
                check(tmp)
            except CheckFailed as error:
                failed.append(check.__name__)
                print(f"FAIL {check.__name__}: {error}")
                continue
        print(f"PASS {check.__name__}")
    if failed:
        print(f"_test-kconfig: {len(failed)}/{len(CHECKS)} FAILED: {', '.join(failed)}")
        return 1
    print(f"_test-kconfig: {len(CHECKS)}/{len(CHECKS)} PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
