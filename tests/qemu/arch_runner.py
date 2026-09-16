#!/usr/bin/env python3
"""KaleidOS architectural selftest runner (host tooling, stdlib only).

Usage:  python3 tests/qemu/arch_runner.py --arch <rv64|rv32> --kernel <path>

Boots exactly the artifact given by --kernel (the Makefile passes $(OUTPUT),
i.e. `kaleidos-<arch>-selftest` under the selftest profile) and drives the
feature-gated RISC-V architectural selftests: every case boots a fresh QEMU
process and must reach its serial-output contract (PASS marker, or PANIC plus
the expected scause), with firmware shutdown exiting QEMU.

Raw serial output → tests/qemu/logs/<arch>-archtest-<case>-<stamp>.log.
Exit 0 = all cases PASS, non-zero = FAIL.
"""

from __future__ import annotations

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
READY = "[selftest] ready"
CASE_TIMEOUT_S = 30
EXIT_TIMEOUT_S = 10

ARCH_CONF = {
    "rv64": {"qemu": "qemu-system-riscv64", "mem": "4G"},
    "rv32": {"qemu": "qemu-system-riscv32", "mem": "1G"},
}

# (case name, expected scause, required serial substring)
CASES = (
    ("mapping", None, None),
    ("context-switch", None, None),
    ("panic-containment", None, None),
    ("task-panic", None, None),
    # step 2 D: a real .kcomp panics via its own SDK panic adapter; Core must
    # survive and the adapter's diagnostic line must reach the serial console.
    ("panic-component", None, "[kcomp] panic"),
    ("illegal-instruction", 2, None),
    ("breakpoint", 3, None),
    ("load-fault", 13, None),
    ("store-readonly", 15, None),
    ("execute-nx", 12, None),
    # docs/testing.md §2: "TLB flush 是否正确" — a remap (and an invalidation)
    # must become visible only after sfence.vma; tlb-invalidate faults like
    # load-fault but fills the TLB first, so it additionally proves the stale
    # translation is gone.
    ("tlb-flush", None, None),
    ("tlb-invalidate", 13, None),
    ("timer", None, None),
    ("external-irq", None, None),
)


class RunFailure(Exception):
    """A QEMU case did not meet its observable serial-output contract."""


def read_output(proc: subprocess.Popen[bytes], output: str, timeout_s: float) -> str:
    """Append serial output available before the bounded wait expires."""
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        ready, _, _ = select.select([proc.stdout], [], [], 0.1)
        if ready:
            chunk = os.read(proc.stdout.fileno(), 4096)
            if not chunk:
                return output
            output += chunk.decode("utf-8", "replace")
            return output
        if proc.poll() is not None:
            return output
    return output


def wait_for_ready(proc: subprocess.Popen[bytes], output: str) -> str:
    """Wait until the kernel is ready to accept precisely one selftest name."""
    deadline = time.monotonic() + CASE_TIMEOUT_S
    while READY not in output and time.monotonic() < deadline:
        output = read_output(proc, output, 0.2)
        if proc.poll() is not None:
            break
    if READY not in output:
        raise RunFailure("selftest readiness marker missing")
    return output


def fail_if_fatal(output: str) -> None:
    """Reject a selftest-declared failure or an unexpected panic immediately."""
    if "[selftest]" in output and "FAIL" in output:
        raise RunFailure("selftest reported FAIL")


def wait_for_non_faulting_pass(proc: subprocess.Popen[bytes], output: str, name: str) -> str:
    """Require a named PASS marker, then require firmware shutdown to exit QEMU."""
    marker = f"[selftest] {name}: PASS"
    deadline = time.monotonic() + CASE_TIMEOUT_S
    while marker not in output and time.monotonic() < deadline:
        output = read_output(proc, output, 0.2)
        fail_if_fatal(output)
        if "PANIC:" in output:
            raise RunFailure("unexpected panic")
        if proc.poll() is not None:
            break
    if marker not in output:
        raise RunFailure(f"missing PASS marker {marker!r}")

    deadline = time.monotonic() + EXIT_TIMEOUT_S
    while proc.poll() is None and time.monotonic() < deadline:
        output = read_output(proc, output, 0.2)
        fail_if_fatal(output)
    if proc.poll() is None:
        raise RunFailure("QEMU did not exit after selftest shutdown")
    return output


def wait_for_fault(proc: subprocess.Popen[bytes], output: str, expected: int) -> str:
    """Require PANIC plus exactly the requested scause; reject every mismatch."""
    deadline = time.monotonic() + CASE_TIMEOUT_S
    expected_marker = f"scause=0x{expected:x}"
    while time.monotonic() < deadline:
        output = read_output(proc, output, 0.2)
        fail_if_fatal(output)
        scauses = re.findall(r"scause=0x([0-9a-fA-F]+)", output)
        if any(int(value, 16) != expected for value in scauses):
            raise RunFailure(
                f"unexpected scause(s) {scauses!r}; wanted {expected_marker}"
            )
        if "PANIC:" in output and expected_marker in output:
            return output
        if proc.poll() is not None:
            break
    raise RunFailure(f"missing PANIC plus {expected_marker}")


def log_path(arch: str, name: str) -> str:
    """Construct the per-case raw-serial log path."""
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S-%f")
    return os.path.join(LOGS_DIR, f"{arch}-archtest-{name}-{stamp}.log")


def run_case(
    arch: str,
    kernel: str,
    name: str,
    expected_scause: int | None,
    required_text: str | None,
) -> None:
    """Boot a fresh QEMU process and judge exactly one architectural test."""
    conf = ARCH_CONF[arch]
    command = [
        conf["qemu"],
        "-machine", "virt",
        "-smp", "2",
        "-m", conf["mem"],
        "-bios", "default",
        "-kernel", kernel,
        "-nographic",
    ]
    proc = subprocess.Popen(
        command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT
    )
    output = ""
    path = log_path(arch, name)
    verdict = "FAILED"

    try:
        output = wait_for_ready(proc, output)
        proc.stdin.write(f"{name}\n".encode())
        proc.stdin.flush()
        if expected_scause is None:
            output = wait_for_non_faulting_pass(proc, output, name)
            if required_text is not None and required_text not in output:
                raise RunFailure(f"missing required serial text {required_text!r}")
            print(f"[arch-{arch}] {name}: PASS")
        else:
            output = wait_for_fault(proc, output, expected_scause)
            print(f"[arch-{arch}] {name}: PASS (scause=0x{expected_scause:x})")
        verdict = "PASSED"
    except RunFailure as error:
        print(f"[arch-{arch}] {name}: FAIL ({error})")
        raise
    finally:
        if proc.poll() is None:
            deadline = time.monotonic() + 1
            while time.monotonic() < deadline:
                output = read_output(proc, output, 0.2)
            proc.kill()
        proc.wait()
        with open(path, "w", encoding="utf-8") as log:
            log.write(output)
            log.write(f"\n==== ARCHTEST {verdict} ====\n")
        print(f"[arch-{arch}] log: {path}")


def parse_args():
    parser = argparse.ArgumentParser(
        description="Boot one KaleidOS selftest artifact and judge every ArchTest case.")
    parser.add_argument("--arch", required=True, choices=sorted(ARCH_CONF),
                        help="profile arch: selects the QEMU binary")
    parser.add_argument("--kernel", required=True, metavar="PATH",
                        help="boot artifact to test (the Makefile passes $(OUTPUT))")
    return parser.parse_args()


def main() -> int:
    """Run all cases for one RISC-V architecture and return a shell verdict."""
    args = parse_args()
    arch = args.arch
    # 产物身份由调用方显式给出（Makefile 传 $(OUTPUT)），不再按 arch 猜镜像名。
    kernel = args.kernel if os.path.isabs(args.kernel) else os.path.join(REPO, args.kernel)
    if not os.path.exists(kernel):
        print(f"FAIL: selftest kernel {kernel} not found")
        return 1

    os.makedirs(LOGS_DIR, exist_ok=True)
    failures = 0
    for name, expected_scause, required_text in CASES:
        try:
            run_case(arch, kernel, name, expected_scause, required_text)
        except RunFailure:
            failures += 1

    if failures:
        print(f"[arch-{arch}] FAIL ({failures} case(s))")
        return 1
    print(f"[arch-{arch}] ALL PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
