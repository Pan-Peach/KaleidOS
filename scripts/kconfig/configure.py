#!/usr/bin/env python3
"""Build a resolved KaleidOS .config from defconfigs, fragments and --set.

This is the *creation* side of the Kconfig flow.  It resolves a full,
normalized .config through kconfiglib and refuses to silently normalize away a
value that was explicitly requested: if a value requested by `--set`, a
defconfig or a fragment cannot survive Kconfig resolution (unmet `depends on`,
invisible symbol, unknown symbol), the script exits non-zero instead of writing
a config that lies.

All paths are relative to the current working directory.  Run from the repo
root, e.g.:

    python3 scripts/kconfig/configure.py \
        --defconfig configs/qemu_rv64_defconfig --out .config
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


def parse_args():
    p = argparse.ArgumentParser(
        description="Resolve a KaleidOS .config through kconfiglib.")
    p.add_argument("--kconfig", default="Kconfig",
                   help="top-level Kconfig file (default: Kconfig)")
    p.add_argument("--base", default=None,
                   help="optional config loaded first, replacing all values")
    p.add_argument("--defconfig", action="append", default=[], metavar="FILE",
                   help="defconfig merged in order (repeatable)")
    p.add_argument("--fragment", action="append", default=[], metavar="FILE",
                   help="fragment merged in order, after defconfigs (repeatable)")
    p.add_argument("--set", action="append", default=[], dest="sets",
                   metavar="SYM=VALUE", help="explicit assignment (repeatable)")
    p.add_argument("--out", required=True, help="path of the resolved .config")
    return p.parse_args()


def parse_set(item):
    """Split one --set argument into (SYMBOL, VALUE)."""
    if "=" not in item:
        sys.exit("error: --set expects SYM=VALUE, got '{}'".format(item))
    name, value = item.split("=", 1)
    name, value = name.strip(), value.strip()
    if not name or not value:
        sys.exit("error: --set expects SYM=VALUE, got '{}'".format(item))
    return name, value


def lookup(kconf, name):
    """Resolve SYM to a symbol, tolerating an optional CONFIG_ prefix."""
    sym = kconf.syms.get(name)
    if sym is None and name.startswith("CONFIG_"):
        sym = kconf.syms.get(name[len("CONFIG_"):])
    return sym


def reason(sym):
    """One-line explanation for a value that did not survive resolution."""
    if sym.visibility == 0:
        return "not visible: its `depends on` condition is unmet"
    if sym.choice is not None:
        return "the choice kept another member selected"
    return "value truncated by an unmet dependency or a lower-precedence default"


# `.config` 格式里的两类显式赋值行（与 kconfiglib load_config 的解析一致）：
#   CONFIG_FOO=value
#   # CONFIG_FOO is not set
_ASSIGN_RE = re.compile(r"CONFIG_([^=]+)=")
_UNSET_RE = re.compile(r"# CONFIG_([^ ]+) is not set")


def requested_symbols(path):
    """Yield (SYMBOL, is_assignment) for every explicit assignment in a file."""
    with open(path) as source:
        for line in source:
            line = line.rstrip()
            match = _ASSIGN_RE.match(line)
            if match:
                yield match.group(1), True
                continue
            match = _UNSET_RE.match(line)
            if match:
                yield match.group(1), False


def verify_requests(kconf, paths):
    """defconfig / fragment 的显式请求在解析后必须存活。

    load_config 已把请求值归一化进 `user_value`（bool → y/n），所以比较
    `str_value`（依赖 / choice 截断后的实际值）与 `user_value` 即可：不等 =
    请求被静默归一化（unmet `depends on`、choice 选了别人）。--base 不做逐项
    校验：它是上一份 resolved config（默认值也写成行），Kconfig 演进时会误报；
    --set 的请求由 apply_sets 校验。
    """
    for path in paths:
        for name, is_assignment in requested_symbols(path):
            sym = lookup(kconf, "CONFIG_" + name)
            if sym is None:
                sys.exit("error: unknown symbol 'CONFIG_{}' requested by {}"
                         .format(name, path))
            if not is_assignment and sym.orig_type not in (kconfiglib.BOOL,
                                                           kconfiglib.TRISTATE):
                # `# CONFIG_FOO is not set` 对非 bool 只是注释，kconfiglib 同样忽略。
                continue
            requested = kconfiglib.TRI_TO_STR.get(sym.user_value, sym.user_value)
            if requested is None:
                sys.exit("error: CONFIG_{} in {} was assigned an invalid value "
                         "and ignored".format(name, path))
            if sym.str_value != requested:
                sys.stderr.write(
                    "error: CONFIG_{}={} requested by {} resolves to {}\n"
                    "  reason: {}\n".format(name, requested, path, sym.str_value,
                                           reason(sym)))
                sys.exit(1)


def resolve(args):
    """Create + populate the Kconfig object from every input source."""
    try:
        kconf = kconfiglib.Kconfig(args.kconfig, warn_to_stderr=True)
    except (kconfiglib.KconfigError, OSError) as exc:
        sys.exit("error: failed to parse '{}': {}".format(args.kconfig, exc))

    try:
        if args.base:
            kconf.load_config(args.base, replace=True)
        for path in args.defconfig:
            kconf.load_config(path, replace=False)
        for path in args.fragment:
            kconf.load_config(path, replace=False)
    except OSError as exc:
        sys.exit("error: cannot load configuration: {}".format(exc))
    return kconf


def apply_sets(kconf, assignments):
    """Set every requested value, then verify it actually survived."""
    for name, value in assignments:
        sym = lookup(kconf, name)
        if sym is None:
            sys.exit("error: unknown symbol '{}' requested by --set".format(name))
        # set_value() only validates the *form* of the value (bool y/n), so its
        # return value is ignored here on purpose; the real check is str_value
        # below, which reflects dependencies and choice resolution.
        sym.set_value(value)

    for name, value in assignments:
        sym = lookup(kconf, name)
        resolved = sym.str_value
        if resolved != value:
            sys.stderr.write(
                "error: {}={} was requested but resolves to {}\n"
                "  reason: {}\n".format(name, value, resolved, reason(sym)))
            sys.exit(1)


def main():
    args = parse_args()
    assignments = [parse_set(item) for item in args.sets]

    kconf = resolve(args)
    apply_sets(kconf, assignments)
    verify_requests(kconf, args.defconfig + args.fragment)

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    kconf.write_config(args.out, header="# KaleidOS configuration (generated)\n")
    print("configuration written to {}".format(args.out))


if __name__ == "__main__":
    main()
