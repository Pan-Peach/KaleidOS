#!/usr/bin/env python3
"""Hardware contracts: each case boots a fresh guest and keeps its fault evidence."""
import argparse
import re
import sys
import time
from common import (FATAL_MARKERS, RunFailure, Session, add_arguments,
                    qemu_command)

# (case name, expected scause, required serial substring)
CASES = (
    ("isolated-heap", None, "isolated-heap: same artifact K/I"),
    ("isolated-domain-service", None, "isolated-domain-service: K/K K/I I/K I/I"),
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
    # window (args / config / out_state) and the fresh entry's `tp == 0`,
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
    # delivered DIRECTLY (shared Core mappings put the caller's descriptor and
    # buffers at the same VA->PA in the provider AS -- provider-side pointers are
    # the caller's, payloads read in place, output written in place), the provider
    # runs on its own root, and the Core root is restored after the
    # transition.  `isolated-service-fault` proves a provider fault (load from an
    # unmapped VA) is contained: the caller gets a typed error, the instance is
    # Failed with a retired AS and released windows, and Core survives.
    ("isolated-service", None, "isolated-service: gate OK"),
    ("isolated-service-fault", None, "isolated-service-fault: contained"),
    # The failure/restart acceptance matrix.  Every row asserts the
    # AGENTS.md failure contract ("组件失败 = 逻辑死亡、物理驻留"): the instance
    # reaches its documented state, its AS is retired or released, the
    # Core-prepared windows are returned, the
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

# Experimental ISA suite, explicitly selected outside the RISC-V baseline.
# Unsupported hardware must fail visibly. Current support is in the module docs.
NEW_ARCH_CASES = (
    ("boot", None, None),
    ("smp-boot", None, None),
    ("timer", None, None),
    ("external-irq", None, None),
)

# RISC-V SMP cases, selected separately without rerunning the base cases. Component scheduling is tested by CoreTest.
SMP_CASES = (
    ("smp-boot", None, None),
    ("smp-ipi", None, None),
    ("smp-percpu", None, None),
)



def select_cases(arch, smp_only=False, selected=None):
    if smp_only and arch != "rv64":
        raise RunFailure("the RISC-V SMP suite requires RV64")
    cases = SMP_CASES if smp_only else (CASES if arch in ("rv64", "rv32") else NEW_ARCH_CASES)
    if selected:
        cases = tuple(case for case in cases if case[0] == selected)
        if not cases:
            raise RunFailure(f"case {selected!r} is not in the selected suite")
    return cases


def run_case(args, name, expected_scause, required_text):
    with Session(args, f"archtest-{args.arch}-{name}") as session:
        proc = session.start(qemu_command(args, probe=False))
        _, ready = session.collect(proc, 30, ["[selftest] ready"], FATAL_MARKERS)
        if not ready:
            raise RunFailure("selftest readiness marker missing")
        session.send(proc, name + "\n")
        if expected_scause is None:
            expected = [f"[selftest] {name}: PASS"]
            if required_text:
                expected.append(required_text)
            _, ok = session.collect(proc, 30, expected, FATAL_MARKERS)
            if not ok:
                raise RunFailure(f"missing serial markers: {expected!r}")
            session.shutdown()
        else:
            marker = f"scause=0x{expected_scause:x}"
            _, ok = session.collect(proc, 30, ["PANIC:", marker], ["FAIL"])
            # Fault guests halt rather than shut down. Drain the diagnostic and
            # let an already-exiting emulator finish before deliberately killing it.
            deadline = time.monotonic() + 0.5
            while proc.poll() is None and time.monotonic() < deadline:
                session.read(0.05)
            causes = re.findall(r"scause=0x([0-9a-fA-F]+)", session.output)
            wrong_cause = any(int(cause, 16) != expected_scause for cause in causes)
            if not ok or "FAIL" in session.output or wrong_cause:
                raise RunFailure(f"expected PANIC plus {marker}, observed {causes}")
            if proc.poll() is not None and proc.returncode != 0:
                raise RunFailure(f"QEMU exited with {proc.returncode} after fault")
        print(f"[arch-{args.arch}] {name}: PASS")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", required=True,
                        choices=("rv64", "rv32", "x86_64", "aarch64", "loongarch64"))
    add_arguments(parser)
    parser.add_argument("--smp-only", action="store_true")
    parser.add_argument("--case", help="run exactly one case in this suite")
    parser.add_argument("--list", action="store_true")
    args = parser.parse_args()
    try:
        cases = select_cases(args.arch, args.smp_only, args.case)
    except RunFailure as error:
        parser.error(str(error))
    if args.list:
        for name, _, _ in cases:
            print(name)
        return 0
    if not args.kernel.is_file():
        parser.error(f"kernel not found: {args.kernel}")
    failures = 0
    for name, cause, text in cases:
        try:
            run_case(args, name, cause, text)
        except (RunFailure, OSError) as error:
            print(f"[arch-{args.arch}] {name}: FAIL ({error})")
            failures += 1
    print(f"[arch-{args.arch}] {len(cases) - failures}/{len(cases)} PASS")
    return int(failures != 0)


if __name__ == "__main__":
    sys.exit(main())
