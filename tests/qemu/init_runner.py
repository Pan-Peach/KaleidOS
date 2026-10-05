#!/usr/bin/env python3
"""Boot-to-console workflow through the real init component and FAT root disk."""

import argparse
import os
import shutil
import subprocess
import sys

from common import (BOOT_MARKERS, FATAL_MARKERS, MONITOR_BANNER, RunFailure,
                    Session, add_arguments, make_disk, qemu_command)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", required=True, choices=sorted(BOOT_MARKERS))
    add_arguments(parser)
    parser.add_argument("--exec-fixtures", required=True)
    parser.add_argument("--rootfs", required=True)
    parser.add_argument("--scenario", choices=["fat", "no-block", "bad-fat", "oom"], default="fat")
    args = parser.parse_args()
    if args.scenario == "oom" and args.arch != "rv64":
        parser.error("the user memory pressure probe requires RV64")
    try:
        with Session(args, f"init-{args.arch}-{args.scenario}") as session:
            return run(args, session)
    except (RunFailure, OSError, subprocess.CalledProcessError) as error:
        print(f"FAIL init {args.arch}/{args.scenario}: {error}")
        return 1


def run(args, session):
    disks = []
    if args.scenario != "no-block":
        disk = session.directory / "root.img"
        if args.scenario in ("fat", "oom"):
            shutil.copyfile(args.rootfs, disk)
        else:
            make_disk(disk)
        disks.append(disk)
    if args.arch == "rv64" and args.scenario == "fat":
        for filename, source in [("EXIT0.ELF", "exit-zero"), ("EXIT7.ELF", "exit-seven"),
                                 ("WRITE.ELF", "write"), ("STACK.ELF", "stack-bss"),
                                 ("PRIV.ELF", "privileged"), ("TEXT.ELF", "text-write"),
                                 ("PROTECT.ELF", "protect")]:
            subprocess.run(["mcopy", "-o", "-i", disks[0],
                            os.path.join(args.exec_fixtures, source), "::/"+filename], check=True)
    if args.scenario == "oom":
        subprocess.run(["mcopy", "-o", "-i", disks[0],
                        os.path.join(args.exec_fixtures, "oom"), "::/OOM.ELF"], check=True)
    proc = session.start(qemu_command(args, disks,
                         memory="128M" if args.scenario == "oom" else None))

    def expect(markers):
        text, ok = session.collect(proc, 45, markers, FATAL_MARKERS)
        if not ok:
            raise RunFailure(f"missing {markers!r}")
        return text

    def command(line, markers):
        session.send(proc, line + "\n")
        return expect(markers)

    if args.scenario == "bad-fat":
        expect(["init: root mount failed:", "init: startup failed:",
                "monitor fallback", MONITOR_BANNER])
        text = command("components", ["fatfs", "Failed"])
        if not any("name=init " in line and "state=Failed" in line
                   for line in text.splitlines()):
            raise RunFailure("failed init record missing")
        # Partial composition is visible; monitor can still start a console.
        command("load ksh", ["load ksh: OK", "KaleidOS ksh"])
    else:
        stage = "init: FAT root mounted" if args.scenario in ("fat", "oom") else \
            "init: no block device; console session only"
        expect([stage, "[boot] init: ready", "KaleidOS ksh"])
        command("inspect init", ["Domain:   KernelNative", "State:    Ready"])
        if args.scenario == "oom":
            command("exec 0:/OOM.ELF", ["OOM_RECOVERED", "exec: exit=0"])
        elif args.scenario == "fat":
            command("cat 0:/HELLO.TXT", ["HELLO FROM KALEIDOS FAT ROOTFS"])
            command("cat 0:/DOCS/ABOUT.TXT", ["KaleidOS test fixture:"])
            if args.arch == "rv64":
                for filename, expected in [("EXIT0.ELF", "exec: exit=0"),
                                           ("EXIT7.ELF", "exec: exit=7"),
                                           ("WRITE.ELF", "exec: exit=0"),
                                           ("STACK.ELF probe", "exec: exit=0"),
                                           ("PRIV.ELF", "exec: signal=4"),
                                           ("TEXT.ELF", "exec: signal=11"),
                                           ("PROTECT.ELF", "exec: signal=11")]:
                    markers = [expected]
                    if filename == "WRITE.ELF": markers.append("EXEC_WRITE_OK")
                    command("exec 0:/"+filename, markers)
                command("exec 0:/HELLO.TXT", ["exec: ENOEXEC"])
                command("cat 0:/HELLO.TXT", ["HELLO FROM KALEIDOS FAT ROOTFS"])
        else:
            command("cat 0:/HELLO.TXT", ["cat: no filesystem provider"])
            command("exec missing", ["exec: ENODEV"])
        # init is a boot-anchor composer, not an application launched by ksh.
        command("load init", ["load init: ENOMEM" if args.scenario == "oom" else "load init: EINVAL"])
    command("echo INIT_SERIAL_OK", ["\nINIT_SERIAL_OK"])
    command("exit", ["ksh: exit"])
    # Published user backing stays resident in phase 1. At exhaustion,
    # destroy cannot acquire its Core-owned call stack; reject gracefully.
    command("unload ksh", ["DestroyFailed(-12)" if args.scenario == "oom" else "unload ksh: OK"])
    session.send(proc, "shutdown\n")
    session.shutdown()
    print(f"[init-{args.arch}/{args.scenario}] ALL PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
