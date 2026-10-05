#!/usr/bin/env python3
"""Boot a test profile, validate CoreTest, then exercise the serial console."""
import argparse
import sys
import ksh
from common import (BOOT_MARKERS, FATAL_MARKERS, MONITOR_BANNER, RunFailure,
                    Session, add_arguments, make_disk, qemu_command)
from protocol import core_test_report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", required=True, choices=sorted(BOOT_MARKERS))
    parser.add_argument("--scenario", choices=("default", "no-block"), default="default")
    add_arguments(parser)
    args = parser.parse_args()
    if not args.kernel.is_file():
        parser.error(f"kernel not found: {args.kernel}")
    try:
        with Session(args, f"coretest-{args.arch}-{args.scenario}") as session:
            disks = []
            if args.scenario == "default":
                for index in range(2):
                    disk = session.directory / f"block-{index}.img"
                    make_disk(disk)
                    disks.append(disk)
            proc = session.start(qemu_command(args, disks))

            def expect(markers, timeout=30):
                output, ok = session.collect(proc, timeout, markers, FATAL_MARKERS)
                if not ok:
                    raise RunFailure(f"missing serial markers: {markers!r}")
                return output

            expect([BOOT_MARKERS[args.arch], MONITOR_BANNER], 45)
            for verb in ("load", "unload"):
                session.send(proc, f"{verb} kcomp_c_smoke\n")
                expect([f"{verb} kcomp_c_smoke: OK"])
            session.send(proc, "load core_test\n")
            report = expect(["load core_test: OK", "[core-test] all: PASS"], 60)
            cases = core_test_report(report)
            print(f"[coretest-{args.arch}/{args.scenario}] {len(cases)} checks PASS")
            ksh.run(proc, session.collect, session.send, RunFailure, FATAL_MARKERS)
            session.send(proc, "shutdown\n")
            session.shutdown()
    except (RunFailure, OSError) as error:
        print(f"FAIL coretest {args.arch}/{args.scenario}: {error}")
        return 1
    print(f"[coretest-{args.arch}/{args.scenario}] ALL PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
