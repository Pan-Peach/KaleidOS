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
    # panic-component: a real .kcomp panics via its own SDK panic adapter; Core must
    # survive and the adapter's diagnostic line must reach the serial console.
    ("panic-component", None, "[kcomp] panic"),
    ("illegal-instruction", 2, None),
    ("breakpoint", 3, None),
    ("load-fault", 13, None),
    ("store-readonly", 15, None),
    ("execute-nx", 12, None),
    # docs/development/testing.md §2: "TLB flush 是否正确" — a remap (and an invalidation)
    # must become visible only after sfence.vma; tlb-invalidate faults like
    # load-fault but fills the TLB first, so it additionally proves the stale
    # translation is gone.
    ("tlb-flush", None, None),
    ("tlb-invalidate", 13, None),
    ("timer", None, None),
    ("external-irq", None, None),
    # The Isolated-domain minimal cross-AS trampoline.  `isolated-transition`
    # proves the Core -> private AS -> Core round-trip (register/tp/gp save-
    # restore + component-visible private root); `isolated-timer` proves a
    # returning timer interrupt taken inside the private AS, handled on the
    # normal Core trap path / Core trap stack, then resumed; `isolated-fault`
    # proves a recoverable component page fault (Core policy maps the missing
    # page and the component retries); `isolated-fault-abandon` proves component
    # identity alone is NOT recoverable (Core refuses, the trampoline returns
    # to the suspended Core caller).
    ("isolated-transition", None, None),
    ("isolated-timer", None, None),
    ("isolated-fault", None, None),
    ("isolated-fault-abandon", None, None),
    # Per-domain loading of a real `.kcomp` into a private address
    # space with page-separated permissions.  `isolated-image` proves the placed
    # image executes through the minimal cross-AS trampoline in its own root
    # (shared Core mappings same VA->PA + image segments + harness pages;
    # other instances' private backing unreachable).
    # `isolated-perm-text` / `isolated-perm-data` prove the page tables really
    # enforce segment permissions: a store to the R+X text page faults with
    # scause 15 and an instruction fetch from the R+W data page faults with
    # scause 12 — both observed by the Core fault policy and then abandoned.
    # The required substrings carry the observed scause into the runner verdict.
    ("isolated-image", None, "isolated-image: private AS OK"),
    ("isolated-image-wrong-env", None, None),
    ("isolated-perm-text", None, "scause=0xf"),
    ("isolated-perm-data", None, "scause=0xc"),
    # The shared-Core-mapping model: every Isolated AS carries the same Core
    # mappings at the same VA -> PA; private backing is excluded from the
    # identity aliases of every live root; Core code is callable directly from
    # the instance root with no satp switch.
    ("isolated-shared-mappings", None, "isolated-shared-mappings: same VA->PA"),
    ("isolated-private-unreachable", None, "isolated-private-unreachable: privacy held"),
    ("isolated-core-direct", None, "isolated-core-direct: direct OK"),
    # The Isolated lifecycle end-to-end through the
    # production entry points.  `isolated-lifecycle` creates a real `.kcomp`
    # instance via `create_component(.., IsolatedNative)`, proves its
    # `kcomp_instance_create` ran inside the private AS with the ABI-transit
    # window (args / config / out_state) and the per-instance runtime slot (tp),
    # proves the window is reachable only from that instance's AS, then stops it
    # through `stop_component` and proves `kcomp_instance_destroy` really ran
    # (window-side destroy marker) and the AS was retired.  The required
    # substring carries the observed instance window into the runner verdict.
    # `isolated-lifecycle-fail` / `isolated-lifecycle-fault` prove both
    # create-entry failure modes (non-zero return and an in-AS fault caught by
    # the normal Core trap path's containment) leave a `Failed` tombstone with a
    # retired AS and the Core-prepared window/stack released.
    ("isolated-lifecycle", None, "isolated-lifecycle: private AS OK"),
    ("isolated-lifecycle-fail", None, None),
    ("isolated-lifecycle-fault", None, None),
    # The KernelNative -> Isolated service Gate.  A KernelNative
    # caller invokes a real `.kcomp` provider's `kcomp_service_dispatch` in its
    # own private AS through the minimal cross-AS trampoline: the flat frame is
    # COPIED through a Core-owned mailbox (provider-side pointers are all mailbox
    # VAs, payloads equal the caller's, output copied back), the provider runs on
    # its own root/slot, caller memory is unreachable from the instance AS, and
    # the Core root is restored after the transition.  `isolated-service-limits`
    # proves an over-capacity frame is rejected (`-EMSGSIZE`) before any copy
    # (provider never runs).  `isolated-service-fault` proves a provider fault
    # (load from a caller-domain address) is contained: the caller gets a typed
    # error, the instance is Failed with a retired AS and released windows, and
    # Core survives.
    ("isolated-service", None, "isolated-service: gate OK"),
    ("isolated-service-limits", None, None),
    ("isolated-service-fault", None, "isolated-service-fault: contained"),
    # The failure/restart acceptance matrix.  Every row asserts the
    # AGENTS.md failure contract ("组件失败 = 逻辑死亡、物理驻留"): the instance
    # reaches its documented state, its AS is retired or released, the
    # Core-prepared windows are returned, the runtime slot is cleared, the
    # endpoint (if any) is dead, the caller gets a typed error, Core stays alive,
    # and the KernelNative path keeps working.
    #   isolated-load-reject    placement failure (17 MiB .bss beyond the image
    #                           window) and the unsupported-import envelope are
    #                           rejected BEFORE any component/AS is declared;
    #   isolated-config-reject  an over-capacity config fails the create stage
    #                           with the same cleanup as other create failures;
    #   isolated-prepare-reject `isolated::prepare` rejects a non-executable
    #                           entry / unwritable stack / retired AS with typed
    #                           errors and never mutates the instance truth;
    #   isolated-destroy-fault  a destroy-entry trap yields DestroyPanicked +
    #                           Failed + retired AS (windows stay resident per
    #                           current contract) and is never retried;
    #   isolated-stale-access   a resolved-but-dead endpoint is blocked at the
    #                           Core boundary (the provider never runs again);
    #   isolated-ready-fault    a fault on a component that already served a call
    #                           is contained, then the same artifact is
    #                           re-instantiated as a genuinely independent
    #                           component (fresh AS / backing / window / slot);
    #   isolated-restart        re-instantiating the same artifact runs to Ready
    #                           after Failed/Stopped tombstones (fresh AS / backing
    #                           / window / slot), and a second concurrent component
    #                           of the same artifact is accepted with its own
    #                           independent AS / backing.
    ("isolated-load-reject", None, "isolated-load-reject: rejected before declare"),
    ("isolated-config-reject", None, None),
    ("isolated-prepare-reject", None, "isolated-prepare-reject: typed rejections held"),
    ("isolated-destroy-fault", None, "isolated-destroy-fault: contained"),
    ("isolated-stale-access", None, "isolated-stale-access: blocked"),
    ("isolated-ready-fault", None, "isolated-ready-fault: contained + restarted"),
    ("isolated-restart", None, "isolated-restart: fresh instances"),
    # Direct Core imports for Isolated components: a real .kcomp calls the
    # supported diagnostic/read-only exports directly (satp stays the instance
    # root; relocation targets the shared low alias) and destroys cleanly; a
    # component panic escapes through kcore_panic_escape into the cross-AS
    # continuation and is contained as CreateFaulted.
    ("isolated-direct-imports", None, "[kcomp] direct-ok"),
    ("isolated-panic-escape", None, "[kcomp] panic"),
    # Nested AS switching: a Core helper called from AS_A enters AS_B (one
    # switch out, one back).  The healthy path returns to A; the fault path
    # abandons only B (attributed through B's cross-AS context) and A resumes.
    ("isolated-nested-as", None, None),
    ("isolated-nested-fault", None, None),
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
