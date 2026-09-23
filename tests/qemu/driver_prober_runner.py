#!/usr/bin/env python3
"""KaleidOS driver_prober end-to-end runner (host tooling, stdlib only).

Usage:  python3 tests/qemu/driver_prober_runner.py --arch <rv64|rv32> --kernel <path>

Boots exactly the artifact given by --kernel (the Makefile passes $(OUTPUT)) and
drives the Core Monitor with `load scheduler_rr` → `load driver_prober`.  The
prober does a coarse compatible match, then provisions each candidate device with
`kcore_component_create(driver, DriverCreateConfig{device_id, 结果端口名})` —
the assignment travels *into* create (flat bytes); the driver never calls back
into the prober.  After create returns 0 the prober **pulls** the driver's
`probe.result` endpoint and records the outcome with a local cursor update.
Three scenarios:

  positive     1 MiB virtio-blk drive (MBR 0xaa55 @ 510): first assignment
               attaches (`capacity: 2048 sectors`, `mbr sig=aa55`,
               `probe.result published (outcome=0)`), the pull reports Match and
               the prober stops after its first attachment.
  no-device    no drive: every assignment is created and reports NoMatch
               (`probe.result published (outcome=1)`); the prober finishes
               cleanly without any attachment.
  extra-device two same-class drives: first assignment attaches; the prober
               stops, so exactly one driver instance is created.

Failure contract (any fails the run):
  - boot marker / banner missing (timeout), or `PANIC` / `trap fatal`
  - a required marker missing, `NoPolicy` present, or a second driver instance
  - any re-entrancy / EBUSY rejection marker (`Reentrant`, `EBUSY`) — the
    acyclic flow must never hit the Core re-entry gate
  - QEMU exits before the verdict, or does not exit after shutdown

Raw serial output → tests/qemu/logs/<arch>-driver-prober-<case>-<stamp>.log.
Exit 0 = all cases PASS, non-zero = FAIL.
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
BUILD_DIR = os.path.join(REPO, "build")
DISK_BYTES = 1024 * 1024  # 1 MiB = 2048 sectors
MBR_SIG_OFFSET = 510

ARCH_CONF = {
    "rv64": {
        "qemu": "qemu-system-riscv64",
        "mem": "4G",
        "boot_ok": "[core] BOOT CORE OK",
    },
    "rv32": {
        "qemu": "qemu-system-riscv32",
        "mem": "1G",  # 32 位地址空间放不下 4 GiB RAM（见 Makefile）
        "boot_ok": "[bootstrap] RV32 CORE OK",
    },
}

MONITOR_BANNER = "KaleidOS Core Monitor"
PROMPT = "core> "
SCHEDULER_OK = "load scheduler_rr: OK"
PROBER_OK = "load driver_prober: OK"
SHUTDOWN_CMD = "shutdown\n"

# prober 的 coarse match + 无环 provisioning（三个场景共有）：assignment 进 create
# config，结果由 prober 在 create 返回后 pull `probe.result`。
PROBER_MARKERS = (
    "driver_prober: 1 candidate(s); dispatch queued",
    "driver_prober: loading candidate virtio_blk",
    "driver_prober: create virtio_blk attempt=1 device_id=",
    "driver_prober: created virtio_blk instance=",
    "driver_prober: pull probe.result.1 endpoint=",
)
# positive / extra-device：第一个 assignment 直接匹配并 attach。
ATTACH_MARKERS = (
    "virtio_blk: assignment device_id=",
    "virtio_blk capacity: 2048 sectors",
    "mbr sig=aa55",
    "virtio_blk: probe.result published (outcome=0)",
    "driver_prober: attempt=1 outcome=0 (Match) detail=0",
    "driver_prober: attempt=1 Match; stopping after first attachment",
)
# no-device：每个 assignment 都 NoMatch（report-only 实例），prober 干净结束。
NO_DEVICE_MARKERS = (
    "virtio_blk: probe.result published (outcome=1)",
    "driver_prober: attempt=1 outcome=1 (NoMatch)",
    "driver_prober: no supported device; dispatch done",
)

FATAL_MARKERS = ("PANIC", "trap fatal", "NoPolicy")
# 无环流程绝不允许出现重入 / EBUSY 拒绝（旧回调模型才会撞上）。
REENTRANCY_MARKERS = ("Reentrant", "re-entr", "EBUSY")
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")

BOOT_TIMEOUT_S = 45
LOAD_TIMEOUT_S = 60
SHUTDOWN_TIMEOUT_S = 10

CASES = ("positive", "no-device", "extra-device")
DEFAULT_DISKS = {"positive": 1, "no-device": 0, "extra-device": 2}


class RunFailure(Exception):
    pass


def make_disk(path: str) -> None:
    """Create a 1 MiB raw disk image with MBR signature 0xaa55 at byte 510."""
    image = bytearray(DISK_BYTES)
    image[MBR_SIG_OFFSET] = 0x55
    image[MBR_SIG_OFFSET + 1] = 0xAA
    with open(path, "wb") as disk:
        disk.write(image)


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


def qemu_command(conf, kernel, disks):
    cmd = [
        conf["qemu"],
        "-machine", "virt",
        "-smp", "2",
        "-m", conf["mem"],
        "-bios", "default",
        "-kernel", kernel,
        "-nographic",
    ]
    for index, disk in enumerate(disks):
        cmd += [
            "-drive", f"file={disk},if=none,format=raw,id=hd{index}",
            "-device", f"virtio-blk-device,drive=hd{index}",
        ]
    return cmd


def wait_for(proc, sink, expected, label):
    """Collect a stage into `sink`; fail if any `expected` marker is missing.

    Completeness is checked against the whole `sink` (output already read in an
    earlier stage counts), but the await is only for this stage's new output.
    """
    text, _ = collect(proc, LOAD_TIMEOUT_S, expected)
    sink.append(text)
    joined = "\n".join(sink)
    missing = [m for m in expected if m not in joined]
    if missing:
        raise RunFailure(f"{label}: missing {missing!r}")
    return joined


def component_is_ready(proc, sink, name):
    """Drive `components` and assert the instance for <name>'s image is Ready."""
    send(proc, "components\n")
    joined = wait_for(proc, sink, [f"image={name}"], f"components/{name}")
    for line in joined.splitlines():
        if f"image={name}" in line:
            if "state=Ready" in line:
                return
            raise RunFailure(f"{name} not Ready: {line!r}")
    raise RunFailure(f"{name} line not found")


def drive_case(proc, sink, case):
    """Run one scenario to the second-load check; raise RunFailure on mismatch."""
    send(proc, "load scheduler_rr\n")
    wait_for(proc, sink, [SCHEDULER_OK], "load scheduler_rr")

    send(proc, "load driver_prober\n")
    expected = [*PROBER_MARKERS, PROBER_OK]
    expected += list(NO_DEVICE_MARKERS) if case == "no-device" else list(ATTACH_MARKERS)
    joined = wait_for(proc, sink, expected, f"load driver_prober ({case})")

    if case == "no-device":
        if "virtio_blk capacity:" in joined:
            raise RunFailure("no-device: driver attached a device")
        if "outcome=1 (NoMatch)" not in joined:
            raise RunFailure("no-device: no NoMatch result recorded")
        component_is_ready(proc, sink, "virtio_blk")
    else:
        attachments = "\n".join(sink).count("virtio_blk capacity:")
        if attachments != 1:
            raise RunFailure(f"{case}: expected exactly 1 attach, saw {attachments}")
        # 差分证据：prober 在首个 Match 之后**停止**——只创建过一个 driver 实例
        # （extra-device 的第二台盘从未被 create）。
        creates = "\n".join(sink).count("driver_prober: create virtio_blk ")
        if creates != 1:
            raise RunFailure(
                f"{case}: expected exactly 1 driver create (stop after first "
                f"attachment), saw {creates}"
            )
        if "attempt=1 Match; stopping after first attachment" not in joined:
            raise RunFailure(f"{case}: prober did not stop after the first attachment")

    # 无环证据：整段输出不得出现重入 / EBUSY 拒绝（旧同步回调模型才会撞上）。
    text = "\n".join(sink)
    for marker in REENTRANCY_MARKERS:
        if marker in text:
            raise RunFailure(
                f"{case}: re-entrancy rejection marker {marker!r} present in output"
            )

    # 第二次 load：monitor 的单实例便利语义拒绝重复加载（组件 ABI 仍支持多实例）。
    send(proc, "load virtio_blk\n")
    wait_for(proc, sink, ["load virtio_blk: already loaded"], "second load")


def run_case(arch, kernel, case):
    conf = ARCH_CONF[arch]
    os.makedirs(LOGS_DIR, exist_ok=True)
    os.makedirs(BUILD_DIR, exist_ok=True)
    disks = []
    for index in range(DEFAULT_DISKS[case]):
        disk = os.path.join(BUILD_DIR, f"driver-prober-{arch}-{case}-{index}.img")
        make_disk(disk)
        disks.append(disk)

    log_path = os.path.join(
        LOGS_DIR,
        f"{arch}-driver-prober-{case}-{datetime.datetime.now():%Y%m%d-%H%M%S}.log",
    )
    proc = subprocess.Popen(
        qemu_command(conf, kernel, disks),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    sink = []
    try:
        # 等 prompt（`core> ` 无换行，靠 collect 的 buf 匹配）再发命令，避免首个
        # 命令赶在 monitor 输入循环就绪之前丢失。
        wait_for(proc, sink, [conf["boot_ok"], MONITOR_BANNER, PROMPT], "boot smoke")
        drive_case(proc, sink, case)
        send(proc, SHUTDOWN_CMD)
        deadline = time.monotonic() + SHUTDOWN_TIMEOUT_S
        while time.monotonic() < deadline and proc.poll() is None:
            time.sleep(0.1)
        if proc.poll() is None:
            proc.kill()
            raise RunFailure("QEMU did not exit after shutdown (hang)")
    except RunFailure as error:
        with open(log_path, "w") as log:
            log.write(ANSI_RE.sub("", "\n".join(sink)))
            log.write("\n==== RUN FAILED ====\n" + str(error) + "\n")
        proc.kill()
        return False, f"FAIL ({arch}/{case}): {error}  (log: {log_path})"

    with open(log_path, "w") as log:
        log.write(ANSI_RE.sub("", "\n".join(sink)))
        log.write("\n==== RUN PASSED ====\n")
    return True, f"[driver-prober-{arch}] {case}: PASS (log: {log_path})"


def parse_args():
    parser = argparse.ArgumentParser(
        description="Run the driver_prober end-to-end scenarios in QEMU.")
    parser.add_argument("--arch", required=True, choices=sorted(ARCH_CONF),
                        help="profile arch: selects the QEMU binary and boot marker")
    parser.add_argument("--kernel", required=True, metavar="PATH",
                        help="boot artifact to test (the Makefile passes $(OUTPUT))")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    arch = args.arch
    # 产物身份由调用方显式给出（Makefile 传 $(OUTPUT)），不按 arch 猜镜像名。
    kernel = args.kernel if os.path.isabs(args.kernel) else os.path.join(REPO, args.kernel)
    if not os.path.exists(kernel):
        print(f"FAIL: kernel {kernel} not found "
              f"(select a profile, e.g. `make qemu_{arch}_defconfig`, then `make kernel`)")
        return 1
    passed = True
    for case in CASES:
        ok, message = run_case(arch, kernel, case)
        print(message)
        passed = passed and ok
    print(f"[driver-prober-{arch}] ALL {'PASS' if passed else 'FAIL'}")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
