#!/usr/bin/env python3
"""Boot-to-console workflow through the real init component and FAT root disk."""

import argparse
import os
import shutil
import subprocess
import sys
import time

from common import (BOOT_MARKERS, FATAL_MARKERS, MONITOR_BANNER, RunFailure,
                    Session, add_arguments, make_disk, qemu_command)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", required=True, choices=sorted(BOOT_MARKERS))
    add_arguments(parser)
    parser.add_argument("--exec-fixtures", required=True)
    parser.add_argument("--rootfs", required=True)
    parser.add_argument("--scenario", choices=["fat", "dual-fat", "no-block", "bad-fat", "oom"], default="fat")
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
        if args.scenario in ("fat", "dual-fat", "oom"):
            shutil.copyfile(args.rootfs, disk)
        else:
            make_disk(disk)
        disks.append(disk)
    if args.arch == "rv64" and args.scenario in ("fat", "dual-fat"):
        for filename, source in [("EXIT0.ELF", "exit-zero"), ("EXIT7.ELF", "exit-seven"),
                                 ("WRITE.ELF", "write"), ("STACK.ELF", "stack-bss"),
                                 ("PRIV.ELF", "privileged"), ("TEXT.ELF", "text-write"),
                                 ("PROTECT.ELF", "protect")]:
            subprocess.run(["mcopy", "-o", "-i", disks[0],
                            os.path.join(args.exec_fixtures, source), "::/"+filename], check=True)
    if args.scenario == "oom":
        subprocess.run(["mcopy", "-o", "-i", disks[0],
                        os.path.join(args.exec_fixtures, "oom"), "::/OOM.ELF"], check=True)
    if args.scenario == "dual-fat":
        # Root is the first discovered block in this profile; extra raw storage
        # remains separate and must not make the root selection ambiguous.
        scratch = session.directory / "raw.img"
        make_disk(scratch, marker=0x42, signature=False)
        disks.append(scratch)
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
        stage = "init: FAT root mounted" if args.scenario in ("fat", "dual-fat", "oom") else \
            "init: no block device; console session only"
        expect([stage, "[boot] init: ready", "KaleidOS ksh"])
        if args.scenario == "dual-fat" and "dispatch done; matched=2" not in session.output:
            raise RunFailure("both block devices must be attached automatically")
        command("inspect init", ["Domain:   KernelNative", "State:    Ready"])
        command("devices", ["DEVICE (PRIMARY COMPATIBLE)", "ns16550a", "virtio,mmio", "primary MMIO:", "device record(s)"] +
                (["Claimed", "virtio_blk#"] if args.scenario in ("fat", "dual-fat", "oom") else ["Unclaimed"]))
        if args.scenario == "oom":
            command("exec 0:/OOM.ELF", ["OOM_RECOVERED", "exec: exit=0"])
        elif args.scenario in ("fat", "dual-fat"):
            command("cat /local/README.TXT", ["KaleidOS local filesystem"])
            command("cat /fat/HELLO.TXT", ["HELLO FROM KALEIDOS FAT ROOTFS"])
            command("cat 0:/HELLO.TXT", ["HELLO FROM KALEIDOS FAT ROOTFS"])
            command('cat "0:/HELLO.TXT"', ["HELLO FROM KALEIDOS FAT ROOTFS"])
            command("cat 0:/HELLO\\.TXT", ["HELLO FROM KALEIDOS FAT ROOTFS"])
            command("cat 0:/DOCS/ABOUT.TXT", ["KaleidOS test fixture:"])
            if args.arch == "rv64":
                for filename, expected in [("EXIT0.ELF", "exec: exit=0"),
                                           ("EXIT7.ELF", "exec: exit=7"),
                                           ("WRITE.ELF", "exec: exit=0"),
                                           ('STACK.ELF "probe"', "exec: exit=0"),
                                           ("PRIV.ELF", "exec: signal=4"),
                                           ("TEXT.ELF", "exec: signal=11"),
                                           ("PROTECT.ELF", "exec: signal=11")]:
                    markers = [expected]
                    if filename == "WRITE.ELF": markers.append("EXEC_WRITE_OK")
                    command("exec 0:/"+filename, markers)
                command("exec 0:/HELLO.TXT", ["exec: ENOEXEC"])
                command("cat 0:/HELLO.TXT", ["HELLO FROM KALEIDOS FAT ROOTFS"])
        else:
            command("cat /local/README.TXT", ["KaleidOS local filesystem"])
            command("cat 0:/HELLO.TXT", ["cat: open failed:"])
            command("exec missing", ["exec: ENOENT"])
        # init is a boot-anchor composer, not an application launched by ksh.
        command("load init", ["load init: ENOMEM" if args.scenario == "oom" else "load init: EINVAL"])
    command("echo INIT_SERIAL_OK", ["\nINIT_SERIAL_OK"])
    if args.scenario == "oom":
        command("\x1b[A", ["\nINIT_SERIAL_OK"])
        command('echo "OOM  SHELL_OK"', ["\nOOM  SHELL_OK"])
        command("history", ["  echo INIT_SERIAL_OK"])
        command("devices", ["virtio_blk#", "device record(s)"])
    command("exit", ["ksh: exit"])
    if args.scenario == "oom":
        # Exhaustion need not consume every allocatable order. A changed image
        # footprint may still leave enough space for the destroy call stack.
        start = len(session.output)
        session.send(proc, "unload ksh\n")
        deadline = time.monotonic() + 30
        while True:
            stopped = session.output[start:]
            if "unload ksh: OK" in stopped or "DestroyFailed(-12)" in stopped:
                break
            if any(marker in stopped for marker in FATAL_MARKERS):
                raise RunFailure("fatal output while unloading under memory pressure")
            if time.monotonic() >= deadline or proc.poll() is not None:
                raise RunFailure("unexpected unload result under memory pressure")
            session.read()
    else:
        command("unload ksh", ["unload ksh: OK"])
    session.send(proc, "shutdown\n")
    session.shutdown()
    print(f"[init-{args.arch}/{args.scenario}] ALL PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
