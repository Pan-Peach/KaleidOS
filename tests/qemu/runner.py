#!/usr/bin/env python3
"""KaleidOS automated QEMU test runner (host tooling, stdlib only).

Usage:  python3 tests/qemu/runner.py --arch <rv64|rv32> --kernel <path>

Boots exactly the artifact given by --kernel (the Makefile passes $(OUTPUT),
e.g. `kaleidos-rv64`), so the runner never has to guess which image belongs to
the selected profile.  Select a profile first, e.g.
`make qemu_<arch>_defconfig`, then `make kernel`:

1. boot smoke  —— wait for the arch boot marker, then the Core Monitor banner;
2. auto CoreTest —— type `load core_test`, require every `[core-test] ... PASS`
   line plus the `all: PASS` summary and `load core_test: OK`;
2b. first complete chain —— type `load block_chain`: the composer creates the
   Rust RAM block provider (`ram_blk`), resolves its endpoint, probes the Gate
   transport once, and creates the C consumer (`fatfs`) with that EndpointId as
   create config. FatFs binds (Core picks the mechanism). FatFs then publishes
   its **filesystem endpoint**; the composer resolves it, probes the Gate
   transport once (`mount`), and creates the C filesystem test consumer
   (`fs_consumer`) with that EndpointId as create config. The consumer binds,
   mounts, opens and reads `HELLO.TXT` through the uniform wrapper and verifies
   the exact bytes. Evidence required:
     * provider business reads (`ram_blk: read lba=4`) + FatFs business log
       (`[fatfs] read`) + consumer success lines (`[fs_consumer] read ok` /
       `close ok` / `unmount ok`) + the exact content line
       (`KALEIDOS BLOCK CHAIN OK`) -> correct bytes over the chain, and the
       consumer task ran to completion before shutdown;
     * exactly one `ram_blk: gate dispatch` line, `method=0` (the composer's
       capacity probe) and exactly one `[fatfs] gate dispatch` line, `method=0`
       (the composer's filesystem mount probe)  -> every business read in both
       chains is Direct: no `kcore_endpoint_call`, no per-call service stack.
2c. acyclic prober -> driver flow —— with one 1 MiB virtio-blk drive attached,
   type `load driver_prober`: the prober enumerates the `virtio,mmio` candidates
   and provisions the driver by `kcore_component_create(virtio_blk,
   DriverCreateConfig{device_id, 结果端口名})`. The assignment travels INTO
   create (flat bytes); the driver never calls back into the prober. After
   create returns 0 the prober PULLS the driver's `probe.result` endpoint
   (`driver_prober: pull probe.result.1 endpoint=`) and records the outcome
   locally. Evidence required:
     * `driver_prober: create virtio_blk attempt=1 device_id=` then
       `driver_prober: created virtio_blk instance=` and the pull line;
     * the driver attached (`virtio_blk capacity: 2048 sectors`, `mbr sig=aa55`,
       `virtio_blk: probe.result published (outcome=0)`) and the pull reports
       `outcome=0 (Match)`, after which the prober stops
       (`attempt=1 Match; stopping after first attachment`, exactly one create);
     * NO re-entrancy / EBUSY rejection marker anywhere (`Reentrant`, `re-entr`,
       `EBUSY`) -> the flow is acyclic; the old prober callback cycle is
       gone rather than the re-entry gate being weakened.
3. shutdown —— type `shutdown`, expect QEMU to exit.

Failure contract (any of these fails the run):
  - boot marker or banner missing (timeout)  -> hang/regression
  - any output containing `PANIC` / `FAIL` / `trap fatal`
  - QEMU exits before PASS verdict
  - QEMU does not exit after shutdown

Raw serial output is saved to tests/qemu/logs/<arch>-<timestamp>.log.
Exit code 0 = PASS, non-zero = FAIL. Markers below were captured empirically
from current develop (do not guess markers from old docs).
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
# 2c 的 virtio-blk 盘：1 MiB raw + MBR 签名（与 driver_prober_runner 同一约定）。
DISK_BYTES = 1024 * 1024
MBR_SIG_OFFSET = 510

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
# First complete chain: Rust BlockDevice provider -> C FatFs -> filesystem
# endpoint -> C filesystem consumer.
CHAIN_CMD = "load block_chain\n"
CHAIN_LOAD_OK = "load block_chain: OK"
CHAIN_GATE_PROBE = "block_chain: gate probe ok"
CHAIN_PROVIDER_READ = "ram_blk: read lba=4"
CHAIN_CONTENT = "KALEIDOS BLOCK CHAIN OK"
CHAIN_GATE_DISPATCH = "ram_blk: gate dispatch"
# filesystem chain (FatFs publishes a filesystem endpoint; fs_consumer reads
# HELLO.TXT through the uniform wrapper).
FS_CHAIN_GATE_PROBE = "block_chain: fs gate probe ok"
FS_PROVIDER_READ = "[fatfs] read"
FS_CONSUMER_OK = "[fs_consumer] read ok"
FS_CONSUMER_CLOSE_OK = "[fs_consumer] close ok"
FS_CONSUMER_UNMOUNT_OK = "[fs_consumer] unmount ok"
FS_GATE_DISPATCH = "[fatfs] gate dispatch"
# Acyclic prober -> driver flow (2c): assignment via create config, result by pull.
PROBER_CMD = "load driver_prober\n"
PROBER_LOAD_OK = "load driver_prober: OK"
PROBER_QUEUED = "driver_prober: 1 candidate(s); dispatch queued"
PROBER_CREATE = "driver_prober: create virtio_blk attempt=1 device_id="
PROBER_CREATED = "driver_prober: created virtio_blk instance="
PROBER_PULL = "driver_prober: pull probe.result.1 endpoint="
PROBER_ASSIGNMENT = "virtio_blk: assignment device_id="
PROBER_RESULT_MATCH = "virtio_blk: probe.result published (outcome=0)"
PROBER_ATTACH = "virtio_blk capacity: 2048 sectors"
PROBER_MATCH = "driver_prober: attempt=1 outcome=0 (Match) detail=0"
PROBER_STOPPED = "driver_prober: attempt=1 Match; stopping after first attachment"
SHUTDOWN_CMD = "shutdown\n"

FATAL_MARKERS = ("PANIC", "FAIL", "trap fatal")
# 无环流程绝不允许出现重入 / EBUSY 拒绝（旧回调模型才会撞上；见 2c）。
REENTRANCY_MARKERS = ("Reentrant", "re-entr", "EBUSY")

# 终端控制序列（core_test 的 ANSI 颜色码）：判定用原始流，写日志前剥离，
# 让日志文件保持可 grep 的纯文本（`make qemu` 交互终端仍显示颜色）。
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")

# 各阶段超时（秒）：hang / 慢启动都算失败
BOOT_TIMEOUT_S = 45
CORE_TEST_TIMEOUT_S = 30
CHAIN_TIMEOUT_S = 30
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

    返回 (全部输出, 是否满足 expected)。
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
                text = line.decode("utf-8", "replace")
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
    text = "\n".join(out) + buf.decode("utf-8", "replace")
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
    # 2c 需要一台真实的 virtio-blk 设备（prober 的 assignment 要 attach 成功）。
    disk = os.path.join(BUILD_DIR, f"runner-{arch}-virtio-blk.img")
    make_disk(disk)

    cmd = [
        conf["qemu"],
        "-machine", "virt",
        "-smp", "2",
        "-m", conf["mem"],
        "-bios", "default",
        "-kernel", kernel,
        "-nographic",
        "-drive", f"file={disk},if=none,format=raw,id=hd0",
        "-device", "virtio-blk-device,drive=hd0",
    ]
    proc = subprocess.Popen(
        cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT
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

        # -- 2) auto CoreTest ---------------------------------------------
        send(proc, CORE_TEST_CMD)
        output2, ok = collect(
            proc,
            CORE_TEST_TIMEOUT_S,
            [CORE_TEST_OK, CORE_TEST_ALL_PASS],
            FATAL_MARKERS,
        )
        output += "\n" + output2
        core_test_lines = [
            line for line in output2.splitlines() if line.startswith("[core-test]")
        ]
        if not ok:
            raise RunFailure(
                f"core_test did not pass (lines: {core_test_lines!r})"
            )
        summary.append(
            f"core_test: PASS ({len(core_test_lines)} report lines, all: PASS)"
        )

        # -- 2b) first complete chain: Rust provider -> C FatFs -> FS consumer --
        send(proc, CHAIN_CMD)
        output3, ok = collect(
            proc,
            CHAIN_TIMEOUT_S,
            [
                CHAIN_LOAD_OK,
                CHAIN_GATE_PROBE,
                CHAIN_PROVIDER_READ,
                CHAIN_CONTENT,
                FS_CHAIN_GATE_PROBE,
                FS_PROVIDER_READ,
                FS_CONSUMER_OK,
                FS_CONSUMER_CLOSE_OK,
                FS_CONSUMER_UNMOUNT_OK,
            ],
            FATAL_MARKERS,
        )
        output += "\n" + output3
        if not ok:
            raise RunFailure(
                "block chain did not complete: missing load OK / block gate "
                "probe / provider read / HELLO.TXT content / filesystem gate "
                "probe / FatFs read / fs_consumer read/close/unmount"
            )
        # 内容断言：consumer 读到的字节必须**恰好**是合成卷里 HELLO.TXT 的内容
        # （行尾逐字节比对，不是"日志里出现过这个词"）。
        if not any(line.endswith(CHAIN_CONTENT) for line in output3.splitlines()):
            raise RunFailure(
                f"no line ends with the exact HELLO.TXT content {CHAIN_CONTENT!r}"
            )
        # 差分证据（block）：唯一一条 gate dispatch 必须是 composer 的 capacity
        # 探针（method=0）。任何 method=1 的 gate dispatch 都意味着业务 read 走了
        # Core call gate —— 那就不再是 Direct，也意味着每次 read 都会分配
        # per-call service stack。
        gate_lines = [
            line for line in output3.splitlines() if CHAIN_GATE_DISPATCH in line
        ]
        if len(gate_lines) != 1 or "method=0" not in gate_lines[0]:
            raise RunFailure(
                "expected exactly one block gate dispatch (composer capacity "
                f"probe, method=0); got {gate_lines!r}"
            )
        if f"{CHAIN_GATE_DISPATCH} method=1" in output3:
            raise RunFailure(
                "a block business read went through the Core call gate (not Direct)"
            )
        # 差分证据（filesystem）：唯一一条 `[fatfs] gate dispatch` 必须是 composer
        # 的 mount 探针（method=0）。任何 method=4 的行都意味着消费者的 read 走了
        # Core call gate（不再是 Direct）。
        fs_gate_lines = [
            line for line in output3.splitlines() if FS_GATE_DISPATCH in line
        ]
        if len(fs_gate_lines) != 1 or "method=0" not in fs_gate_lines[0]:
            raise RunFailure(
                "expected exactly one filesystem gate dispatch (composer mount "
                f"probe, method=0); got {fs_gate_lines!r}"
            )
        if f"{FS_GATE_DISPATCH} method=4" in output3:
            raise RunFailure(
                "a filesystem business read went through the Core call gate "
                "(not Direct)"
            )
        summary.append(
            "block chain: PASS (Rust provider -> C FatFs -> FS consumer, "
            "Direct reads in both chains, 2 gate probes, 0 gate reads)"
        )

        # -- 2c) acyclic prober -> driver: create-with-config, result by pull --
        send(proc, PROBER_CMD)
        output4, ok = collect(
            proc,
            CHAIN_TIMEOUT_S,
            [
                PROBER_LOAD_OK,
                PROBER_QUEUED,
                PROBER_CREATE,
                PROBER_CREATED,
                PROBER_PULL,
                PROBER_ASSIGNMENT,
                PROBER_RESULT_MATCH,
                PROBER_ATTACH,
                PROBER_MATCH,
                PROBER_STOPPED,
            ],
            FATAL_MARKERS + REENTRANCY_MARKERS,
        )
        output += "\n" + output4
        if not ok:
            raise RunFailure(
                "prober -> driver flow did not complete: missing prober create / "
                "pull-after-create / driver attach / Match / stop markers"
            )
        # 无环差分证据：整段输出不得出现重入 / EBUSY 拒绝。
        for marker in REENTRANCY_MARKERS:
            if marker in output4:
                raise RunFailure(
                    f"re-entrancy rejection marker {marker!r} present in the "
                    "prober -> driver flow (the cycle is not gone)"
                )
        # prober 在首个 Match 之后停止：只创建过一个 driver 实例。
        creates = output4.count("driver_prober: create virtio_blk ")
        if creates != 1:
            raise RunFailure(
                f"expected exactly 1 driver create (stop after first attachment), "
                f"saw {creates}"
            )
        summary.append(
            "prober -> driver: PASS (assignment via create config, result pulled "
            "from probe.result, no re-entrancy/EBUSY, 1 driver create)"
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
            log.write(ANSI_RE.sub("", output))
            log.write("\n==== RUN FAILED ====\n" + str(error) + "\n")
        print(f"FAIL ({arch}): {error}")
        print(f"log: {log_path}")
        proc.kill()
        return 1

    with open(log_path, "w") as log:
        log.write(ANSI_RE.sub("", output))
        log.write("\n==== RUN PASSED ====\n")
    for line in summary:
        print(f"[qemu-{arch}] {line}")
    print(f"[qemu-{arch}] ALL PASS  (log: {log_path})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
