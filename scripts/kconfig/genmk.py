#!/usr/bin/env python3
"""Generate a Make include fragment from a resolved KaleidOS .config.

This is the *generation* side of the Kconfig flow, and the ONLY place where the
config -> build mapping lives.  The Makefile just includes the result, keeping
the mapping in one commented place instead of duplicated across make logic.

Every emitted variable is `override`, so a stray command-line `make KCFG_...=`
cannot create a second source of truth: the resolved config always wins.

Run from the repository root:

    python3 scripts/kconfig/genmk.py --config .config --mk config.mk
"""

import argparse
import os
import re
import sys

# Reuse the pinned Kconfiglib submodule (third_party/Kconfiglib); fall back to an
# installed kconfiglib so a checkout without the submodule still works.
_HERE = os.path.dirname(os.path.abspath(__file__))
_KCONFIGLIB = os.path.join(_HERE, "..", "..", "third_party", "Kconfiglib")
if os.path.isdir(_KCONFIGLIB):
    sys.path.insert(0, _KCONFIGLIB)

try:
    import kconfiglib
except ImportError:
    sys.exit("error: kconfiglib not found; run "
             "`git submodule update --init --recursive`")

# Architecture -> (KCFG_ARCH, KCFG_TARGET, KCFG_LINKER, KCFG_QEMU, KCFG_QEMU_MEM,
#                   KCFG_BOOT_DIR).
# KCFG_BOOT_DIR is the per-arch boot/binary crate directory (see docs/modules/boot.md).
ARCH_MAP = {
    "CONFIG_ARCH_RISCV32": ("rv32", "riscv32imac-unknown-none-elf",
                            "linker32.ld", "qemu-system-riscv32", "1G",
                            "os/boot/riscv"),
    "CONFIG_ARCH_RISCV64": ("rv64", "riscv64gc-unknown-none-elf",
                            "linker.ld", "qemu-system-riscv64", "4G",
                            "os/boot/riscv"),
    # New-ISA skeletons (os/boot/<arch> are todo!() until brought up).
    "CONFIG_ARCH_X86_64": ("x86_64", "x86_64-unknown-none",
                           "linker.ld", "qemu-system-x86_64", "512M",
                           "os/boot/x86_64"),
    "CONFIG_ARCH_AARCH64": ("aarch64", "aarch64-unknown-none",
                            "linker.ld", "qemu-system-aarch64", "1G",
                            "os/boot/aarch64"),
    "CONFIG_ARCH_LOONGARCH64": ("loongarch64", "loongarch64-unknown-none",
                                "linker.ld", "qemu-system-loongarch64", "1G",
                                "os/boot/loongarch64"),
}

# Privilege / VM symbol -> the boot-crate feature spelling.
PRIV_MAP = {
    "CONFIG_PRIVILEGE_SUPERVISOR": "supervisor",
    "CONFIG_PRIVILEGE_MACHINE": "machine",
}
VM_MAP = {
    "CONFIG_VM_MMU": "vm-mmu",
    "CONFIG_VM_NOMMU": "vm-nommu",
}

# QEMU platform defaults are emitted with the resolved build configuration.
QEMU_FLAGS = {
    "rv32": "-machine virt -bios default",
    "rv64": "-machine virt -bios default",
    "x86_64": "-machine q35 -cpu qemu64",
    "aarch64": "-machine virt -cpu cortex-a57",
    "loongarch64": "-machine virt",
}


def parse_args():
    p = argparse.ArgumentParser(
        description="Emit a Make fragment from a resolved KaleidOS .config.")
    p.add_argument("--kconfig", default="Kconfig",
                   help="top-level Kconfig file (default: Kconfig)")
    p.add_argument("--config", default=".config",
                   help="resolved config to read (default: .config)")
    p.add_argument("--mk", required=True, help="Make include file to write")
    return p.parse_args()


def is_y(kconf, name):
    """True when a bool symbol is selected.  `name` carries the CONFIG_ prefix."""
    sym = kconf.syms.get(name[len("CONFIG_"):] if name.startswith("CONFIG_")
                         else name)
    return sym is not None and sym.str_value == "y"


def exactly_one(kconf, mapping, what):
    """Return the single selected key, or exit with a clear message."""
    chosen = [key for key in mapping if is_y(kconf, key)]
    if len(chosen) != 1:
        sys.exit("error: expected exactly one {} selected, found {}: {}".format(
            what, len(chosen), ", ".join(chosen) if chosen else "none"))
    return chosen[0]


def variables(kconf):
    """Compute the ordered (name, value) pairs emitted into the fragment."""
    arch = exactly_one(kconf, ARCH_MAP, "architecture (ARCH_RISCV32/64)")
    priv = exactly_one(kconf, PRIV_MAP, "privilege mode")
    vm = exactly_one(kconf, VM_MAP, "virtual-memory model")

    features = [PRIV_MAP[priv], VM_MAP[vm]]
    if is_y(kconf, "CONFIG_PREEMPT"):
        features.append("preempt")
    if is_y(kconf, "CONFIG_TRACE"):
        features.append("trace")
    if is_y(kconf, "CONFIG_SELFTEST"):
        features.append("selftest")

    arch_name, target, linker, qemu, qemu_mem, boot_dir = ARCH_MAP[arch]
    boot_component = kconf.syms["BOOT_COMPONENT"].str_value
    if not re.fullmatch(r"[A-Za-z0-9_-]*", boot_component):
        sys.exit("error: BOOT_COMPONENT must be an artifact basename (letters, digits, _ or -)")
    lines = [
        ("KCFG_ARCH", arch_name),
        ("KCFG_TARGET", target),
        ("KCFG_LINKER", linker),
        ("KCFG_QEMU", qemu),
        ("KCFG_QEMU_MEM", qemu_mem),
        ("KCFG_QEMU_FLAGS", QEMU_FLAGS[arch_name]),
        ("KCFG_BOOT_DIR", boot_dir),
        ("KCFG_BOOT_FEATURES", ",".join(features)),
        ("KCFG_SELFTEST", "y" if is_y(kconf, "CONFIG_SELFTEST") else "n"),
        ("CONFIG_BOOT_COMPONENT", boot_component),
    ]
    # Mirror every BOOL as CONFIG_<name>=y/n and every INT as its resolved
    # decimal value: 这是 Kconfig 符号自己的名字，Makefile 直接消费（例如把
    # CONFIG_TRACE_CAPACITY 作为环境变量转发给 os/core/build.rs）。不再为同一个
    # 数字造第二个名字。
    for sym in kconf.unique_defined_syms:
        if sym.type in (kconfiglib.BOOL, kconfiglib.INT):
            lines.append(("CONFIG_" + sym.name, sym.str_value))
    return lines


def main():
    args = parse_args()
    try:
        kconf = kconfiglib.Kconfig(args.kconfig, warn_to_stderr=False)
        kconf.load_config(args.config, replace=False)
    except (kconfiglib.KconfigError, OSError) as exc:
        sys.exit("error: cannot read '{}' via '{}': {}".format(
            args.config, args.kconfig, exc))

    lines = variables(kconf)
    width = max(len(name) for name, _ in lines)
    body = "\n".join("override {:<{}} := {}".format(name, width, value)
                     for name, value in lines)

    with open(args.mk, "w") as out:
        out.write("# Generated by scripts/kconfig/genmk.py from {}. "
                  "DO NOT EDIT.\n".format(args.config))
        out.write(body + "\n")
    print("wrote {}".format(args.mk))


if __name__ == "__main__":
    main()
