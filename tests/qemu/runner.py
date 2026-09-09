#!/usr/bin/env python3
"""KaleidOS automated QEMU test runner (host tooling, stdlib only).

Usage:  python3 tests/qemu/runner.py <rv64|rv32>

Drives a QEMU boot of `kaleidos-<arch>` (built by `make kernel ARCH=<arch>`):

1. boot smoke  —— wait for the arch boot marker, then the Core Monitor banner;
2. auto CoreTest —— type `load core_test`, require every `[core-test] ... PASS`
   line plus the `all: PASS` summary and `load core_test: OK`;
3. shutdown —— type `shutdown`, expect QEMU to exit.

Failure contract (any of these fails the run):
  - boot marker or banner missing (timeout)  -> hang/regression
  - any output containing `PANIC` / `FAIL` / `trap fatal`
  - QEMU exits before PASS verdict
  - QEMU does not exit after shutdown

Raw serial output is saved to tests/qemu/logs/<arch>-<timestamp>.log.
Exit code 0 = PASS, non-zero = FAIL. Markers below were captured empirically
from current develop (do not guess markers from old docs).
"""

import datetime
import os
import select
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
LOGS_DIR = os.path.join(REPO, "tests", "qemu", "logs")

ARCH_CONF = {
    "rv64": {
        "qemu": "qemu-system-riscv64",
        "kernel": "kaleidos-rv64",
        "mem": "4G",
        # 启动完成 marker（实测 main64.rs / core::init 输出）
        "boot_ok": "[core] BOOT CORE OK",
    },
    "rv32": {
        "qemu": "qemu-system-riscv32",
        "kernel": "kaleidos-rv32",
        "mem": "1G",  # 32 位地址空间放不下 4 GiB RAM（见 Makefile）
        "boot_ok": "[bootstrap] RV32 CORE OK",
    },
}

MONITOR_BANNER = "KaleidOS Core Monitor"
CORE_TEST_CMD = "load core_test\n"
CORE_TEST_OK = "load core_test: OK"
CORE_TEST_ALL_PASS = "[core-test] all: PASS"
SHUTDOWN_CMD = "shutdown\n"

FATAL_MARKERS = ("PANIC", "FAIL", "trap fatal")

# 各阶段超时（秒）：hang / 慢启动都算失败
BOOT_TIMEOUT_S = 45
CORE_TEST_TIMEOUT_S = 30
SHUTDOWN_TIMEOUT_S = 10


class RunFailure(Exception):
    pass


def collect(proc, deadline_s, expected, forbid=()):
    """读 QEMU 输出直到 `expected` 全部出现、遇到 forbidden 或超时。

    返回 (全部输出, 是否满足 expected)。
    """
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
            # 按行推进，保留半行在 buf 里
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                text = line.decode("utf-8", "replace")
                out.append(text)
                for marker in forbid:
                    if marker in text:
                        raise RunFailure(
                            f"forbidden output {marker!r} in line: {text!r}"
                        )
        if all(e in "\n".join(out) for e in expected):
            return "\n".join(out), True
        if proc.poll() is not None:
            break
    text = "\n".join(out) + buf.decode("utf-8", "replace")
    return text, all(e in text for e in expected)


def send(proc, data: str):
    proc.stdin.write(data.encode())
    proc.stdin.flush()


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] not in ARCH_CONF:
        print(__doc__)
        return 2
    arch = sys.argv[1]
    conf = ARCH_CONF[arch]

    kernel = os.path.join(REPO, conf["kernel"])
    if not os.path.exists(kernel):
        print(f"FAIL: kernel {kernel} not found (run `make kernel ARCH={arch}`)")
        return 1

    os.makedirs(LOGS_DIR, exist_ok=True)
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    log_path = os.path.join(LOGS_DIR, f"{arch}-{stamp}.log")

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
    summary = []

    try:
        # -- 1) boot smoke ------------------------------------------------
        output, ok = collect(proc, BOOT_TIMEOUT_S, [conf["boot_ok"], MONITOR_BANNER], FATAL_MARKERS)
        if not ok:
            raise RunFailure(
                f"boot smoke failed: missing boot marker or monitor banner\n"
                f"(wanted {conf['boot_ok']!r} and {MONITOR_BANNER!r})"
            )
        summary.append(f"boot smoke: PASS ({arch} reached Core Monitor)")

        # -- 2) auto CoreTest ---------------------------------------------
        send(proc, CORE_TEST_CMD)
        output2, ok = collect(
            proc,
            CORE_TEST_TIMEOUT_S,
            [CORE_TEST_OK, CORE_TEST_ALL_PASS],
            FATAL_MARKERS,
        )
        output += "\n" + output2
        core_test_lines = [
            line for line in output2.splitlines() if line.startswith("[core-test]")
        ]
        if not ok:
            raise RunFailure(
                f"core_test did not pass (lines: {core_test_lines!r})"
            )
        summary.append(
            f"core_test: PASS ({len(core_test_lines)} report lines, all: PASS)"
        )

        # -- 3) shutdown ---------------------------------------------------
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
            log.write(output)
            log.write("\n==== RUN FAILED ====\n" + str(error) + "\n")
        print(f"FAIL ({arch}): {error}")
        print(f"log: {log_path}")
        proc.kill()
        return 1

    with open(log_path, "w") as log:
        log.write(output)
        log.write("\n==== RUN PASSED ====\n")
    for line in summary:
        print(f"[qemu-{arch}] {line}")
    print(f"[qemu-{arch}] ALL PASS  (log: {log_path})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
