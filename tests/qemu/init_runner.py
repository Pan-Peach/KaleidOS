#!/usr/bin/env python3
"""Boot-to-console workflow through the real init component and FAT root disk."""

import argparse
import datetime
import os
import shutil
import subprocess
import sys

import runner


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", required=True, choices=sorted(runner.ARCH_CONF))
    parser.add_argument("--kernel", required=True)
    parser.add_argument("--rootfs", required=True)
    parser.add_argument("--scenario", choices=["fat", "no-block", "bad-fat"], default="fat")
    args = parser.parse_args()
    os.makedirs(runner.BUILD_DIR, exist_ok=True)
    os.makedirs(runner.LOGS_DIR, exist_ok=True)
    disks = []
    if args.scenario != "no-block":
        disk = os.path.join(runner.BUILD_DIR, f"init-{args.arch}-{args.scenario}.img")
        if args.scenario == "fat":
            shutil.copyfile(args.rootfs, disk)
        else:
            runner.make_disk(disk)
        disks.append(disk)
    conf = runner.ARCH_CONF[args.arch]
    proc = subprocess.Popen(
        runner.qemu_command(conf, os.path.abspath(args.kernel), disks),
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    output = []
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    log_path = os.path.join(runner.LOGS_DIR, f"init-{args.arch}-{args.scenario}-{stamp}.log")

    def expect(markers):
        text, ok = runner.collect(proc, 45, markers, runner.FATAL_MARKERS)
        output.append(text)
        if not ok:
            raise runner.RunFailure(f"missing {markers!r}")
        return text

    def command(line, markers):
        runner.send(proc, line + "\n")
        return expect(markers)

    try:
        if args.scenario == "bad-fat":
            expect(["init: root mount failed:", "init: startup failed:",
                    "monitor fallback", runner.MONITOR_BANNER])
            text = command("components", ["fatfs", "Failed"])
            if not any("name=init " in line and "state=Failed" in line
                       for line in text.splitlines()):
                raise runner.RunFailure("failed init record missing")
            # Partial composition is visible; monitor can still start a console.
            command("load ksh", ["load ksh: OK", "KaleidOS ksh"])
        else:
            stage = "init: FAT root mounted" if args.scenario == "fat" else \
                "init: no block device; console session only"
            expect([stage, "[boot] init: ready", "KaleidOS ksh"])
            command("inspect init", ["Domain:   KernelNative", "State:    Ready"])
            if args.scenario == "fat":
                command("cat 0:/HELLO.TXT", ["HELLO FROM KALEIDOS FAT ROOTFS"])
                command("cat 0:/DOCS/ABOUT.TXT", ["KaleidOS test fixture:"])
            else:
                command("cat 0:/HELLO.TXT", ["cat: no filesystem provider"])
            # init is a boot-anchor composer, not an application launched by ksh.
            command("load init", ["load init: EINVAL"])
        command("echo INIT_SERIAL_OK", ["\nINIT_SERIAL_OK"])
        command("exit", ["ksh: exit"])
        command("unload ksh", ["unload ksh: OK"])
        runner.send(proc, "shutdown\n")
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired as error:
            raise runner.RunFailure("shutdown hung") from error
        if proc.returncode != 0:
            raise runner.RunFailure(f"QEMU exited with {proc.returncode}")
    except runner.RunFailure as error:
        output.append(f"FAIL: {error}")
        print(f"FAIL init {args.arch}/{args.scenario}: {error}")
        return 1
    finally:
        if proc.poll() is None:
            proc.kill()
        proc.wait()
        with open(log_path, "w") as log:
            log.write("\n".join(output) + "\n")
        print(f"log: {log_path}")
    print(f"[init-{args.arch}/{args.scenario}] ALL PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
