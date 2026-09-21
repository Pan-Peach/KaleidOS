#!/usr/bin/env python3
"""KaleidOS C-component end-to-end runner (host tooling, stdlib only).

Usage:  python3 tests/qemu/c_smoke_runner.py --arch <rv64|rv32> --kernel <path>

Proves the **C component path** works end to end, i.e. that the unified
`kcomp-sdk` C runtime is loadable and callable:

  1. boot smoke  —— arch boot marker + Core Monitor banner + prompt;
  2. `load kcomp_c_smoke` —— clang-built freestanding C `.kcomp` passes the
     packer contract, the loader resolves it, and Core calls the C
     `kcomp_instance_create`. That entry uses the SDK C runtime:
     it calls `kcore_log_line` (Core export, resolved by the loader) and
     `kcore_machine_cpu_count` directly → `[c-smoke] hello from C`;
  3. `unload kcomp_c_smoke` —— Core calls the C `kcomp_instance_destroy` →
     `[c-smoke] exit`;
  4. shutdown —— expect QEMU to exit.

Failure contract (any of these fails the run):
  - boot marker / banner / prompt missing (timeout)  -> hang/regression
  - any output containing `PANIC` / `FAIL` / `trap fatal`
  - QEMU exits before the verdict, or does not exit after shutdown

Raw serial output is saved to tests/qemu/logs/<arch>-c-smoke-<timestamp>.log.
Exit code 0 = PASS, non-zero = FAIL.
"""

import argparse
import datetime
import os
import re
import select
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
LOGS_DIR = os.path.join(REPO, "tests", "qemu", "logs")

ARCH_CONF = {
    "rv64": {
        "qemu": "qemu-system-riscv64",
        "mem": "4G",
        "boot_ok": "[core] BOOT CORE OK",
    },
    "rv32": {
        "qemu": "qemu-system-riscv32",
        "mem": "1G",
        "boot_ok": "[bootstrap] RV32 CORE OK",
    },
}

MONITOR_BANNER = "KaleidOS Core Monitor"
PROMPT = "core> "

LOAD_CMD = "load kcomp_c_smoke\n"
LOAD_OK = "load kcomp_c_smoke: OK"
CREATE_MARKER = "[c-smoke] hello from C"
UNLOAD_CMD = "unload kcomp_c_smoke\n"
UNLOAD_OK = "unload kcomp_c_smoke: OK"
DESTROY_MARKER = "[c-smoke] exit"
SHUTDOWN_CMD = "shutdown\n"

FATAL_MARKERS = ("PANIC", "FAIL", "trap fatal")

# 终端控制序列（core_test 的 ANSI 颜色码）：判定用原始流，写日志前剥离。
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")

BOOT_TIMEOUT_S = 45
LOAD_TIMEOUT_S = 30
UNLOAD_TIMEOUT_S = 20
SHUTDOWN_TIMEOUT_S = 10


class RunFailure(Exception):
    pass


def parse_args():
    parser = argparse.ArgumentParser(
        description="Boot one KaleidOS artifact in QEMU and exercise the C component path.")
    parser.add_argument("--arch", required=True, choices=sorted(ARCH_CONF),
                        help="profile arch: selects the QEMU binary and boot marker")
    parser.add_argument("--kernel", required=True, metavar="PATH",
                        help="boot artifact to test (the Makefile passes $(OUTPUT))")
    return parser.parse_args()


def collect(proc, deadline_s, expected, forbid=FATAL_MARKERS):
    """Read QEMU output until every `expected` substring appears (or timeout)."""
    out = []
    buf = b""
    deadline = time.monotonic() + deadline_s
    while time.monotonic() < deadline:
        ready, _, _ = select.select([proc.stdout], [], [], 0.2)
        if ready:
            chunk = os.read(proc.stdout.fileno(), 4096)
            if not chunk:
                break
            buf += chunk
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                text = line.decode("utf-8", "replace")
                out.append(text)
                for marker in forbid:
                    if marker in text:
                        raise RunFailure(f"forbidden output {marker!r}: {text!r}")
        joined = "\n".join(out) + buf.decode("utf-8", "replace")
        if all(e in joined for e in expected):
            return joined, True
        if proc.poll() is not None:
            break
    text = "\n".join(out) + buf.decode("utf-8", "replace")
    return text, all(e in text for e in expected)


def send(proc, data: str) -> None:
    proc.stdin.write(data.encode())
    proc.stdin.flush()


def main() -> int:
    args = parse_args()
    arch = args.arch
    conf = ARCH_CONF[arch]

    kernel = args.kernel if os.path.isabs(args.kernel) else os.path.join(REPO, args.kernel)
    if not os.path.exists(kernel):
        print(f"FAIL: kernel {kernel} not found "
              f"(select a profile, e.g. `make qemu_{arch}_defconfig`, then `make kernel`)")
        return 1

    os.makedirs(LOGS_DIR, exist_ok=True)
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    log_path = os.path.join(LOGS_DIR, f"{arch}-c-smoke-{stamp}.log")

    cmd = [
        conf["qemu"],
        "-machine", "virt",
        "-smp", "2",
        "-m", conf["mem"],
        "-bios", "default",
        "-kernel", kernel,
        "-nographic",
    ]
    proc = subprocess.Popen(
        cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT
    )
    output = ""
    summary = []

    try:
        # -- 1) boot smoke ------------------------------------------------
        output, ok = collect(
            proc, BOOT_TIMEOUT_S, [conf["boot_ok"], MONITOR_BANNER, PROMPT]
        )
        if not ok:
            raise RunFailure(
                f"boot smoke failed: missing boot marker / banner / prompt\n"
                f"(wanted {conf['boot_ok']!r}, {MONITOR_BANNER!r}, {PROMPT!r})"
            )
        summary.append(f"boot smoke: PASS ({arch} reached Core Monitor)")

        # -- 2) load C component ------------------------------------------
        send(proc, LOAD_CMD)
        output2, ok = collect(proc, LOAD_TIMEOUT_S, [LOAD_OK, CREATE_MARKER])
        output += "\n" + output2
        if not ok:
            raise RunFailure("load kcomp_c_smoke did not produce the C create marker")
        summary.append(f"C create: PASS ({CREATE_MARKER!r}")

        # -- 3) unload -> C destroy ---------------------------------------
        send(proc, UNLOAD_CMD)
        output3, ok = collect(proc, UNLOAD_TIMEOUT_S, [UNLOAD_OK, DESTROY_MARKER])
        output += "\n" + output3
        if not ok:
            raise RunFailure("unload kcomp_c_smoke did not produce the C destroy marker")
        summary.append(f"C destroy: PASS ({DESTROY_MARKER})")

        # -- 4) shutdown ---------------------------------------------------
        send(proc, SHUTDOWN_CMD)
        deadline = time.monotonic() + SHUTDOWN_TIMEOUT_S
        while time.monotonic() < deadline and proc.poll() is None:
            time.sleep(0.1)
        if proc.poll() is None:
            proc.kill()
            raise RunFailure("QEMU did not exit after shutdown (hang)")
        summary.append("shutdown: PASS")

    except RunFailure as error:
        with open(log_path, "w") as log:
            log.write(ANSI_RE.sub("", output))
            log.write("\n==== RUN FAILED ====\n" + str(error) + "\n")
        print(f"FAIL ({arch}): {error}")
        print(f"log: {log_path}")
        proc.kill()
        return 1

    with open(log_path, "w") as log:
        log.write(ANSI_RE.sub("", output))
        log.write("\n==== RUN PASSED ====\n")
    for line in summary:
        print(f"[qemu-{arch}] {line}")
    print(f"[qemu-{arch}] ALL PASS  (log: {log_path})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
