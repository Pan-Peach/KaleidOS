#!/usr/bin/env python3
"""KaleidOS automated QEMU runner (host tooling, stdlib only).

Usage:  python3 tests/qemu/runner.py --arch <rv64|rv32> --kernel <path>

Boots exactly the artifact given by --kernel (the Makefile passes $(OUTPUT),
e.g. `kaleidos-rv64`), so the runner never has to guess which image belongs to
the selected profile.  Select a profile first, e.g.
`make qemu_<arch>_defconfig`, then `make kernel`:

1. boot smoke  —— wait for the arch boot marker, then the Core Monitor banner;
2. CoreTest —— type `load core_test`, require `load core_test: OK`, every
   expected `[core-test] <case>: PASS` line and the `[core-test] all: PASS`
   verdict.  CoreTest is the **single component/system integration
   orchestrator**: the filesystem chains (ram_blk -> fatfs -> filesystem
   endpoint; 2x ram_blk_rw -> littlefs), the driver_prober -> virtio_blk flow
   and the C-frontend smoke are asserted inside CoreTest, not here.  The runner
   only boots the machine and reads CoreTest's machine-readable verdict.
3. shutdown —— type `shutdown`, expect QEMU to exit.

The machine is booted with the device set CoreTest's scenarios need:
a virtio-rng (a `virtio,mmio` candidate that must report NoMatch) followed by
two 1 MiB virtio-blk disks (MBR 0xaa55 @ 510) — the first one is attached by
the driver_prober flow, the second one proves the driver refuses a second
attachment.

Failure contract (any of these fails the run):
  - boot marker or banner missing (timeout)  -> hang/regression
  - `load core_test` not OK, a case not PASS, or any output containing
    `PANIC` / `FAIL` / `trap fatal`
  - QEMU exits before the verdict
  - QEMU does not exit after shutdown

Raw serial output is saved to tests/qemu/logs/<arch>-<timestamp>.log (ANSI
color codes stripped). Exit code 0 = PASS, non-zero = FAIL.
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
# CoreTest 的驱动场景需要的 virtio-blk 盘：1 MiB raw + MBR 签名（两块的约定相同）。
DISK_BYTES = 1024 * 1024
MBR_SIG_OFFSET = 510
DISK_COUNT = 2

ARCH_CONF = {
    "rv64": {
        "qemu": "qemu-system-riscv64",
        "mem": "4G",
        # 启动完成 marker（实测 main64.rs / core::init 输出）
        "boot_ok": "[core] BOOT CORE OK",
    },
    "rv32": {
        "qemu": "qemu-system-riscv32",
        "mem": "1G",  # 32 位地址空间放不下 4 GiB RAM（见 Makefile）
        "boot_ok": "[bootstrap] RV32 CORE OK",
    },
}

MONITOR_BANNER = "KaleidOS Core Monitor"
CORE_TEST_CMD = "load core_test\n"
CORE_TEST_OK = "load core_test: OK"
CORE_TEST_ALL_PASS = "[core-test] all: PASS"
# CoreTest 必须报告的吸收场景（`<case>` 的 PASS 行；ANSI 已剥离）。
CORE_TEST_CASES = (
    "block-chain",
    "block-chain-direct",
    "littlefs-multi-instance",
    "littlefs-isolation",
    "littlefs-direct",
    "driver-candidates",
    "driver-prober-load",
    "driver-prober-dispatch",
    "driver-attach",
    "driver-no-match",
    "driver-multi-device",
    "c-frontend",
)
SHUTDOWN_CMD = "shutdown\n"

FATAL_MARKERS = ("PANIC", "FAIL", "trap fatal")

# 终端控制序列（core_test 的 ANSI 颜色码）：匹配与日志都用剥离后的纯文本，
# 让 case 行保持可 grep（`make qemu` 交互终端仍显示颜色）。
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")

# 各阶段超时（秒）：hang / 慢启动都算失败
BOOT_TIMEOUT_S = 45
CORE_TEST_TIMEOUT_S = 60
SHUTDOWN_TIMEOUT_S = 10


class RunFailure(Exception):
    pass


def parse_args():
    parser = argparse.ArgumentParser(
        description="Boot one KaleidOS artifact in QEMU and run CoreTest.")
    parser.add_argument("--arch", required=True, choices=sorted(ARCH_CONF),
                        help="profile arch: selects the QEMU binary and boot marker")
    parser.add_argument("--kernel", required=True, metavar="PATH",
                        help="boot artifact to test (the Makefile passes $(OUTPUT))")
    return parser.parse_args()


def collect(proc, deadline_s, expected, forbid=()):
    """读 QEMU 输出直到 `expected` 全部出现、遇到 forbidden 或超时。

    返回 (全部输出, 是否满足 expected)。每行先剥 ANSI 颜色码再匹配。
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
                text = ANSI_RE.sub("", line.decode("utf-8", "replace"))
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
    text = "\n".join(out) + ANSI_RE.sub("", buf.decode("utf-8", "replace"))
    return text, all(e in text for e in expected)


def send(proc, data: str):
    proc.stdin.write(data.encode())
    proc.stdin.flush()


def make_disk(path: str) -> None:
    """Create a 1 MiB raw disk image with MBR signature 0xaa55 at byte 510."""
    image = bytearray(DISK_BYTES)
    image[MBR_SIG_OFFSET] = 0x55
    image[MBR_SIG_OFFSET + 1] = 0xAA
    with open(path, "wb") as disk:
        disk.write(image)


def qemu_command(conf, kernel, disks):
    # virtio-rng 在盘之前：它是 prober 的第一个 `virtio,mmio` 候选，必须
    # NoMatch（非块设备），随后的第一块盘 attach、第二块盘验证拒绝二次 attach。
    cmd = [
        conf["qemu"],
        "-machine", "virt",
        "-smp", "2",
        "-m", conf["mem"],
        "-bios", "default",
        "-kernel", kernel,
        "-nographic",
        "-device", "virtio-rng-device",
    ]
    for index, disk in enumerate(disks):
        cmd += [
            "-drive", f"file={disk},if=none,format=raw,id=hd{index}",
            "-device", f"virtio-blk-device,drive=hd{index}",
        ]
    return cmd


def main() -> int:
    args = parse_args()
    arch = args.arch
    conf = ARCH_CONF[arch]

    # 产物身份由调用方显式给出（Makefile 传 $(OUTPUT)），不再用 mtime 猜镜像；
    # 未来若要做更强身份校验，可在产物里内嵌 config hash / init.kpkg hash（TODO）。
    kernel = args.kernel if os.path.isabs(args.kernel) else os.path.join(REPO, args.kernel)
    if not os.path.exists(kernel):
        print(f"FAIL: kernel {kernel} not found "
              f"(select a profile, e.g. `make qemu_{arch}_defconfig`, then `make kernel`)")
        return 1

    os.makedirs(LOGS_DIR, exist_ok=True)
    os.makedirs(BUILD_DIR, exist_ok=True)
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    log_path = os.path.join(LOGS_DIR, f"{arch}-{stamp}.log")
    # CoreTest 的驱动场景需要真实的 virtio-blk 设备（prober 的 assignment 要
    # attach 成功、二次 attach 要被拒绝）。
    disks = []
    for index in range(DISK_COUNT):
        disk = os.path.join(BUILD_DIR, f"runner-{arch}-virtio-blk-{index}.img")
        make_disk(disk)
        disks.append(disk)

    proc = subprocess.Popen(
        qemu_command(conf, kernel, disks),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    # collect() 可能在第一次赋值之前就抛 RunFailure（例如启动即 PANIC），而下面的
    # 失败处理会写 `output`——必须先初始化，否则失败路径自己 UnboundLocalError。
    output = ""
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

        # -- 2) CoreTest: 唯一的组件/系统集成判定 --------------------------
        expected = [CORE_TEST_OK, CORE_TEST_ALL_PASS]
        expected += [f"[core-test]   {case}: PASS" for case in CORE_TEST_CASES]
        send(proc, CORE_TEST_CMD)
        output2, ok = collect(proc, CORE_TEST_TIMEOUT_S, expected, FATAL_MARKERS)
        output += "\n" + output2
        if not ok:
            core_test_lines = [
                line for line in output2.splitlines() if line.startswith("[core-test]")
            ]
            missing = [marker for marker in expected if marker not in output2]
            raise RunFailure(
                f"core_test did not pass; missing {missing!r}\n"
                f"report lines: {core_test_lines!r}"
            )
        core_test_lines = [
            line for line in output2.splitlines() if line.startswith("[core-test]")
        ]
        summary.append(
            f"core_test: PASS ({len(core_test_lines)} report lines, "
            f"{len(CORE_TEST_CASES)} absorbed cases + all: PASS)"
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
