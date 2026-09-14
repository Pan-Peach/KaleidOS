#!/usr/bin/env python3
"""KaleidOS driver_prober end-to-end runner (host tooling, stdlib only).

Usage:  python3 tests/qemu/driver_prober_runner.py <rv64|rv32>

Boots `kaleidos-<arch>` (built by `make kernel ARCH=<arch>`) and drives the Core
Monitor with `load scheduler_rr` → `load driver_prober`.  The prober does a coarse
compatible match and auto-loads `virtio_blk`; the driver claims in its OWN init
context and does the fine protocol match.  Three scenarios:

  positive     1 MiB virtio-blk drive (MBR 0xaa55 @ 510): driver reads sector 0
               (`capacity: 2048 sectors`, `mbr sig=aa55`, `virtio_blk test passed`).
  no-device    no drive: the prober still loads the candidate; the driver finds no
               supported device and still reaches `Ready` (clean no-device).
  extra-device two same-class drives: first attach wins, no second driver instance.

Failure contract (any fails the run):
  - boot marker / banner missing (timeout), or `PANIC` / `trap fatal`
  - a required marker missing, `NoPolicy` present, or a second driver instance
  - QEMU exits before the verdict, or does not exit after shutdown

Raw serial output → tests/qemu/logs/<arch>-driver-prober-<case>-<stamp>.log.
Exit 0 = all cases PASS, non-zero = FAIL.
"""

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
        "kernel": "kaleidos-rv64",
        "mem": "4G",
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
PROMPT = "core> "
SCHEDULER_OK = "load scheduler_rr: OK"
PROBER_OK = "load driver_prober: OK"
SHUTDOWN_CMD = "shutdown\n"

# prober 的 coarse match + 自动加载（三个场景共有）。
PROBER_MARKERS = (
    "driver_prober: 1 candidate(s); dispatch queued",
    "driver_prober: loading candidate virtio_blk",
    "driver_prober: virtio_blk loaded (id=",
)
# positive / extra-device：驱动 fine match 成功并读到 sector 0。
ATTACH_MARKERS = (
    "virtio_blk capacity: 2048 sectors",
    "mbr sig=aa55",
    "virtio_blk test passed",
)
# no-device：驱动遍历完分配，干净地进入 Ready（无 attach）。
NO_DEVICE_MARKER = "virtio_blk: no supported block device; init ok, no device attached"

FATAL_MARKERS = ("PANIC", "trap fatal", "NoPolicy")
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
    """Drive `components` and assert <name> is listed with state=Ready."""
    send(proc, "components\n")
    joined = wait_for(proc, sink, [f"name={name}"], f"components/{name}")
    for line in joined.splitlines():
        if f"name={name}" in line:
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
    expected += [NO_DEVICE_MARKER] if case == "no-device" else list(ATTACH_MARKERS)
    joined = wait_for(proc, sink, expected, f"load driver_prober ({case})")

    if case == "no-device":
        if "virtio_blk capacity:" in joined:
            raise RunFailure("no-device: driver attached a device")
        component_is_ready(proc, sink, "virtio_blk")
    else:
        attachments = "\n".join(sink).count("virtio_blk capacity:")
        if attachments != 1:
            raise RunFailure(f"{case}: expected exactly 1 attach, saw {attachments}")

    # 第二次 load 必须是 already loaded（单实例）。
    send(proc, "load virtio_blk\n")
    wait_for(proc, sink, ["load virtio_blk: already loaded"], "second load")


def run_case(arch, case):
    conf = ARCH_CONF[arch]
    kernel = os.path.join(REPO, conf["kernel"])
    if not os.path.exists(kernel):
        raise RunFailure(f"kernel {kernel} not found (run `make kernel ARCH={arch}`)")

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


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] not in ARCH_CONF:
        print(__doc__)
        return 2
    arch = sys.argv[1]
    passed = True
    for case in CASES:
        ok, message = run_case(arch, case)
        print(message)
        passed = passed and ok
    print(f"[driver-prober-{arch}] ALL {'PASS' if passed else 'FAIL'}")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
