#!/usr/bin/env python3
"""Build ordinary RV64 exec probes; optional QEMU Linux syscall reference.

This host tool never reports KaleidOS execution or isolation as passed.
"""

import argparse
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "os/components/tests/exec_probe"
CASES = (
    ("exit-zero", "EXIT_ZERO", "exit", 0, ()),
    ("exit-seven", "EXIT_SEVEN", "exit", 7, ()),
    ("write", "WRITE", "exit", 0, ()),
    ("stack-bss", "STACK_BSS", "exit", 0, ("probe",)),
    ("bad-pointer", "BAD_POINTER", "exit", 0, ()),
    ("privileged", "PRIVILEGED", "fault", 2, ()),
    ("text-write", "TEXT_WRITE", "fault", 15, ()),
    ("core-read", "CORE_READ", "fault", 13, ()),
    ("stack-execute", "STACK_EXECUTE", "fault", 12, ()),
    ("breakpoint", "BREAKPOINT", "fault", 3, ()),
    ("protect", "PROTECT", "fault", 15, ()),
    ("fork-exec", "FORK_EXEC", "exit", 0, ()),
    ("target", "TARGET", "exit", 7, ("fresh",)),
    ("timer", "TIMER", "exit", 0, ()),
)
# Excluded from the ordinary guest manifest and the Linux reference run: this
# probe depends on a fresh guest's limited physical RAM, not Linux brk policy.
PRESSURE_CASES = (("oom", "OOM", "exit", 0, ()),)


def inspect(binary, needs_bss):
    """Check the emitted artifact, not a reimplementation of guest loading."""
    image = binary.read_bytes()
    if image[:7] != b"\x7fELF\x02\x01\x01":
        raise ValueError(f"{binary}: expected ELF64 little-endian")
    kind, machine, version, entry, phoff = struct.unpack_from("<HHIQQ", image, 16)
    stride, count = struct.unpack_from("<HH", image, 54)
    if (kind, machine, version, stride) != (2, 243, 1, 56):
        raise ValueError(f"{binary}: expected RV64 ET_EXEC")
    if count != 2 or phoff + count * stride > len(image):
        raise ValueError(f"{binary}: expected two load segments")
    segments = [struct.unpack_from("<IIQQQQQQ", image, phoff + i * stride)
                for i in range(count)]
    if [segment[:2] for segment in segments] != [(1, 5), (1, 6)]:
        raise ValueError(f"{binary}: expected separate R-X / RW-NX PT_LOADs")
    for _, _, offset, address, _, filesz, memsz, align in segments:
        if (filesz > memsz or offset + filesz > len(image) or align != 4096
                or address % align != offset % align):
            raise ValueError(f"{binary}: invalid emitted segment")
    text, data = segments
    if not text[3] <= entry < text[3] + text[6]:
        raise ValueError(f"{binary}: entry outside executable segment")
    if (text[3] + text[6] + 4095) // 4096 > data[3] // 4096:
        raise ValueError(f"{binary}: load permissions overlap at page granularity")
    if needs_bss and data[6] - data[5] < 4097:
        raise ValueError(f"{binary}: missing cross-page BSS")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", required=True, choices=("rv64",))
    parser.add_argument("--cc", default="riscv64-unknown-elf-gcc")
    parser.add_argument("--out", type=Path, default=ROOT / "build/exec-fixtures")
    parser.add_argument("--linux-reference", action="store_true",
                        help="run supported exit probes under qemu-riscv64, not KaleidOS")
    args = parser.parse_args()
    compiler = shutil.which(args.cc)
    if not compiler:
        parser.error(f"compiler not found: {args.cc}")
    args.out.mkdir(parents=True, exist_ok=True)
    manifest = args.out / "manifest.txt"
    lines = ["# name completion expected binary argv1 (- = absent)",
             "# fault values are RISC-V synchronous scause; build metadata only; guest results are in CoreTest/QEMU logs"]
    # Concurrent kernel/lint builds must never observe missing or partial ELF.
    with tempfile.TemporaryDirectory(prefix=".exec-build-", dir=args.out) as staging:
        for name, define, completion, expected, argv in CASES + PRESSURE_CASES:
            binary = (args.out / name).resolve()
            staged = Path(staging) / name
            subprocess.run([
                compiler, "-march=rv64gc", "-mabi=lp64", "-mno-relax", "-nostdlib",
                "-static", "-Wl,--build-id=none", "-Wl,-T," + str(SOURCE / "linker.ld"),
                "-DPROBE_" + define, str(SOURCE / (name + ".S") if (SOURCE / (name + ".S")).is_file()
                                        else SOURCE / "probe.S"), "-o", str(staged),
            ], check=True)
            inspect(staged, name == "stack-bss")
            staged.replace(binary)
            if name != "oom":
                lines.append(f"{name} {completion} {expected} {name} {argv[0] if argv else '-'}")
            print(f"{name}: BUILT (no guest result asserted)")
        staged_manifest = Path(staging) / "manifest.txt"
        staged_manifest.write_text("\n".join(lines) + "\n", encoding="utf-8")
        staged_manifest.replace(manifest)
    if args.linux_reference:
        for name, _, completion, expected, argv in CASES:
            if completion != "exit" or name == "fork-exec":
                continue  # Hardware fault attribution requires Core/Arch tests.
            with tempfile.TemporaryDirectory(prefix="exec-linux-reference-") as cwd:
                result = subprocess.run([
                    "qemu-riscv64", str((args.out / name).resolve()), *argv,
                ], cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT, timeout=10)
            output = b"EXEC_WRITE_OK\n" if name == "write" else b""
            if result.returncode != expected or result.stdout != output:
                raise ValueError(f"{name}: reference exit={result.returncode}, output={result.stdout!r}")
            print(f"{name}: QEMU Linux syscall reference matched; no guest result asserted")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
