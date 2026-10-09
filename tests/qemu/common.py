"""QEMU lifecycle and serial I/O shared by the integration harnesses."""
from contextlib import AbstractContextManager
from pathlib import Path
import os
import re
import select
import shlex
import subprocess
import tempfile
import time

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
FATAL_MARKERS = ("PANIC:", "FAIL", "trap fatal")
BOOT_MARKERS = {"rv64": "[core] BOOT CORE OK", "rv32": "[bootstrap] RV32 CORE OK"}
MONITOR_BANNER = "KaleidOS Core Monitor"


class RunFailure(Exception):
    pass


def add_arguments(parser):
    parser.add_argument("--kernel", required=True, type=Path)
    parser.add_argument("--qemu", required=True, help="resolved KCFG_QEMU")
    parser.add_argument("--memory", required=True, help="resolved KCFG_QEMU_MEM")
    parser.add_argument("--qemu-flags", required=True, help="resolved KCFG_QEMU_FLAGS")
    parser.add_argument("--work-dir", type=Path)
    parser.add_argument("--log-dir", type=Path)


def qemu_command(args, disks=(), memory=None, probe=True):
    command = [args.qemu, *shlex.split(args.qemu_flags), "-smp", "2",
               "-m", memory or args.memory, "-kernel", str(args.kernel.resolve()),
               "-nographic"]
    if probe:
        command += ["-device", "virtio-rng-device"]
    for index, disk in enumerate(disks):
        command += ["-drive", f"file={disk},if=none,format=raw,id=hd{index}",
                    "-device", f"virtio-blk-device,drive=hd{index}"]
    return command


def make_disk(path, marker=0, signature=True):
    image = bytearray(1024 * 1024)
    image[0] = marker
    if signature:
        image[510:512] = b"\x55\xaa"
    Path(path).write_bytes(image)


class Session(AbstractContextManager):
    """Each guest owns temporary disks and a complete persistent serial log."""
    def __init__(self, args, name):
        base = args.kernel.resolve().parent
        work = args.work_dir or base / "runs"
        logs = args.log_dir or base / "logs"
        work.mkdir(parents=True, exist_ok=True)
        logs.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix=name + "-", dir=work)
        self.directory = Path(self.temporary.name)
        self.log_path = logs / (self.directory.name + ".log")
        self.proc = None
        self.output = ""
        self.command = []

    def start(self, command):
        self.command = command
        self.proc = subprocess.Popen(command, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        return self.proc

    def read(self, timeout=0.2):
        ready, _, _ = select.select([self.proc.stdout], [], [], timeout)
        if ready:
            chunk = os.read(self.proc.stdout.fileno(), 4096)
            self.output += ANSI_RE.sub("", chunk.decode("utf-8", "replace"))
        return self.output

    def collect(self, proc, timeout, expected, forbid=()):
        # Keep all bytes even when a forbidden marker aborts a stage.
        start = len(self.output)
        deadline = time.monotonic() + timeout
        while True:
            stage = self.output[start:]
            for marker in forbid:
                if marker in stage:
                    raise RunFailure(f"forbidden output {marker!r}")
            if all(marker in stage for marker in expected):
                return stage, True
            if time.monotonic() >= deadline:
                return stage, False
            self.read(min(0.2, max(0, deadline - time.monotonic())))
            if proc.poll() is not None:
                # Consume bytes written just before exit, including partial lines.
                self.output += ANSI_RE.sub("", proc.stdout.read().decode("utf-8", "replace"))
                stage = self.output[start:]
                if any(marker in stage for marker in forbid):
                    raise RunFailure("forbidden output before QEMU exit")
                return stage, all(marker in stage for marker in expected)

    def send(self, proc, data):
        try:
            proc.stdin.write(data.encode())
            proc.stdin.flush()
        except (BrokenPipeError, OSError) as error:
            raise RunFailure("QEMU exited before serial command") from error

    def shutdown(self, timeout=10):
        try:
            self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired as error:
            raise RunFailure("QEMU did not exit after shutdown") from error
        if self.proc.returncode != 0:
            raise RunFailure(f"QEMU exited with {self.proc.returncode}")
        self.output += ANSI_RE.sub("", self.proc.stdout.read().decode("utf-8", "replace"))
        if any(marker in self.output for marker in FATAL_MARKERS):
            raise RunFailure("fatal output before shutdown")

    def __exit__(self, kind, error, traceback):
        if self.proc is not None:
            if self.proc.poll() is None:
                self.proc.kill()
            self.proc.wait()
            self.output += ANSI_RE.sub("", self.proc.stdout.read().decode("utf-8", "replace"))
            self.proc.stdin.close()
            self.proc.stdout.close()
        verdict = f"FAILED: {error}" if error else "PASSED"
        self.log_path.write_text(shlex.join(self.command) + "\n" + self.output +
                                 f"\n==== {verdict} ====\n")
        self.temporary.cleanup()
        print(f"log: {self.log_path}")
        return False
