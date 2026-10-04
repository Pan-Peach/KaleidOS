#!/usr/bin/env python3
"""Build upstream C tests; run only on native references; package for future exec.

Python and JSON are host-only. The guest artifact is a plain text manifest plus
ordinary ELF/PE programs, never .kcomp components.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
HERE = ROOT / "tests" / "compat"
UPSTREAM = ROOT / "third_party" / "libc-test"
TARGETS = ("linux", "windows")


def cases(pattern=""):
    result = []
    names = set()
    for line in (HERE / "cases.txt").read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        name, targets, contract, source, support = line.split()
        if not re.fullmatch(r"[a-z0-9_-]+/[a-z0-9_-]+", name) or name in names:
            raise ValueError(f"invalid or duplicate test name: {name}")
        names.add(name)
        platforms = targets.split(",")
        if not set(platforms) <= set(TARGETS) or support not in ("-", "rand"):
            raise ValueError(f"invalid targets/support for {name}")
        if contract not in ("iso-c", "posix", "computation"):
            raise ValueError(f"invalid contract for {name}")
        path = Path(source)
        if path.is_absolute() or ".." in path.parts or path.suffix != ".c":
            raise ValueError(f"invalid source for {name}")
        if not pattern or name == pattern or name.startswith(pattern.rstrip("/") + "/"):
            result.append(dict(name=name, targets=platforms, contract=contract,
                               source=source, support=support))
    if not result:
        raise ValueError(f"no tests match {pattern!r}")
    return result


def output(args):
    return args.out.resolve() / args.target


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def build(args):
    selected = cases(args.filter)
    directory = output(args)
    (directory / "bin").mkdir(parents=True, exist_ok=True)
    (directory / "logs").mkdir(exist_ok=True)
    # Invalidate earlier results even if toolchain probing or the build fails.
    for filename in ("build.json", "results.json", "results.tsv"):
        (directory / filename).unlink(missing_ok=True)
    if not (UPSTREAM / "COPYRIGHT").is_file():
        raise ValueError("run git submodule update --init --recursive first")
    compiler = args.cc or ("gcc" if sys.platform == "win32" else
                          "x86_64-w64-mingw32-gcc" if args.target == "windows" else "cc")
    machine = subprocess.check_output([compiler, "-dumpmachine"], text=True).strip()
    if (args.target == "windows" and "mingw" not in machine) or (
            args.target == "linux" and "linux" not in machine):
        raise ValueError(f"compiler target {machine!r} does not match {args.target}")
    revision = subprocess.check_output(
        ["git", "-C", str(UPSTREAM), "rev-parse", "HEAD"], text=True).strip()
    if subprocess.check_output(["git", "-C", str(UPSTREAM), "status", "--porcelain"]):
        raise ValueError("libc-test has local modifications; use the pinned upstream sources")
    flags = ["-std=c99", "-O0", "-fno-builtin", "-I", str(HERE / "include")]
    if args.target == "linux":
        flags += ["-D_POSIX_C_SOURCE=200809L"]
    else:
        flags += ["-D__USE_MINGW_ANSI_STDIO=1"]
    flags += args.cflag
    link_flags = (["-static"] if args.linkage == "static" else []) + args.ldflag
    rows = []
    for case in selected:
        row = dict(case, target=args.target)
        rows.append(row)
        if args.target not in case["targets"]:
            row.update(status="UNSUPPORTED", reason="outside this target's selected contract")
            continue
        filename = case["name"].replace("/", "_") + (
            ".exe" if args.target == "windows" else "")
        binary = directory / "bin" / filename
        binary.unlink(missing_ok=True)
        sources = [UPSTREAM / "src" / case["source"], HERE / "support.c"]
        if case["support"] == "rand":
            sources.append(UPSTREAM / "src" / "common" / "rand.c")
        command = [compiler] + flags + [str(s) for s in sources] + link_flags + [
            "-lm", "-o", str(binary)]
        row["command"] = command
        with (directory / "logs" / (filename + ".build.log")).open("wb") as log:
            completed = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT)
        row.update(status="BUILT" if completed.returncode == 0 else "BUILD_FAIL",
                   exit_status=completed.returncode, binary="bin/" + filename)
        if row["status"] == "BUILT":
            row["sha256"] = digest(binary)
        else:
            binary.unlink(missing_ok=True)
        print(f"{case['name']}: {row['status']}")
    write_json(directory / "build.json", dict(
        target=args.target, machine=machine, compiler=compiler,
        compiler_version=subprocess.check_output([compiler, "--version"], text=True).splitlines()[0],
        linkage=args.linkage, flags=flags, link_flags=link_flags,
        upstream_revision=revision, tests=rows))
    return int(any(row["status"] == "BUILD_FAIL" for row in rows))


def run(args):
    native = "windows" if sys.platform == "win32" else "linux" if sys.platform.startswith("linux") else None
    if args.target != native:
        raise ValueError(f"{args.target} reference execution requires native {args.target}; cross-build only here")
    directory = output(args)
    for filename in ("results.json", "results.tsv"):
        (directory / filename).unlink(missing_ok=True)
    data = json.loads((directory / "build.json").read_text(encoding="utf-8"))
    wanted = {case["name"] for case in cases(args.filter)}
    rows = []
    for case in data["tests"]:
        if case["name"] not in wanted:
            continue
        row = dict(name=case["name"], target=args.target, status=case["status"],
                   exit_status=case.get("exit_status") if case["status"] == "BUILD_FAIL" else None,
                   reason=case.get("reason", ""))
        if case["status"] == "BUILT":
            binary = directory / case["binary"]
            if not binary.is_file() or digest(binary) != case["sha256"]:
                row.update(status="LOAD_FAIL", reason="binary missing or changed; rebuild")
            else:
                # Separate working directory and closed stdin for each executable.
                with tempfile.TemporaryDirectory(prefix="kaleidos-compat-") as work:
                    with (directory / "logs" / (binary.name + ".run.log")).open("wb") as log:
                        try:
                            process = subprocess.run([str(binary)], cwd=work, stdin=subprocess.DEVNULL,
                                                     stdout=log, stderr=subprocess.STDOUT,
                                                     timeout=args.timeout)
                            code = process.returncode
                            crash = code < 0 or (args.target == "windows" and (code & 0xC0000000) == 0xC0000000)
                            row.update(status="PASS" if code == 0 else "CRASH" if crash else "FAIL",
                                       exit_status=code)
                        except subprocess.TimeoutExpired:
                            row.update(status="TIMEOUT", reason=f"exceeded {args.timeout}s")
                        except OSError as error:
                            row.update(status="LOAD_FAIL", reason=str(error))
        rows.append(row)
        print(f"{row['name']} [{args.target}]: {row['status']}" + (
            f"({row['exit_status']})" if row["exit_status"] not in (None, 0) else ""))
    missing = wanted - {row["name"] for row in rows}
    if missing:
        raise ValueError(f"tests were not built: {', '.join(sorted(missing))}; rebuild without a filter")
    write_json(directory / "results.json", dict(
        target=args.target, reference="native", host=sys.platform,
        machine=data["machine"], upstream_revision=data["upstream_revision"], results=rows))
    with (directory / "results.tsv").open("w", encoding="utf-8", newline="\n") as report:
        report.write("test\ttarget\tstatus\texit_status\treason\n")
        for row in rows:
            report.write("\t".join(str(row[key]) if row[key] is not None else "-" for key in
                                   ("name", "target", "status", "exit_status", "reason")) + "\n")
    passed = sum(row["status"] == "PASS" for row in rows)
    unsupported = sum(row["status"] == "UNSUPPORTED" for row in rows)
    print(f"{args.target}: {passed}/{len(rows) - unsupported} PASS, {unsupported} UNSUPPORTED")
    return int(any(row["status"] not in ("PASS", "UNSUPPORTED") for row in rows))


def package(args):
    root = args.out.resolve()
    data = [json.loads((root / target / "build.json").read_text(encoding="utf-8")) for target in TARGETS]
    if len({item["upstream_revision"] for item in data}) != 1:
        raise ValueError("Linux and Windows builds use different upstream revisions; rebuild")
    revision = subprocess.check_output(
        ["git", "-C", str(UPSTREAM), "rev-parse", "HEAD"], text=True).strip()
    if revision != data[0]["upstream_revision"]:
        raise ValueError("upstream checkout changed since the builds; rebuild before packaging")
    if any({case["name"] for case in item["tests"]} != {case["name"] for case in cases()} for item in data):
        raise ValueError("packaging requires full builds of both targets")
    lines = ["# test contract target machine executable (relative to package root)"]
    # Validate all inputs before replacing the previous package.
    for item in data:
        for case in item["tests"]:
            if case["status"] == "UNSUPPORTED":
                path = "-"
            elif case["status"] == "BUILT":
                binary = root / item["target"] / case["binary"]
                if not binary.is_file() or digest(binary) != case["sha256"]:
                    raise ValueError(f"missing or changed binary: {binary}")
                path = item["target"] + "/" + case["binary"]
            else:
                raise ValueError(f"cannot package {case['name']}: {case['status']}")
            lines.append(" ".join((case["name"], case["contract"], item["target"], item["machine"], path)))
    destination = root / "package"
    if destination.exists():
        shutil.rmtree(destination)
    destination.mkdir()
    for item in data:
        for case in item["tests"]:
            if case["status"] == "BUILT":
                path = Path(item["target"]) / case["binary"]
                (destination / path).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(root / path, destination / path)
                # CI artifact download does not preserve executable mode.
                (destination / path).chmod(0o755)
        write_json(destination / (item["target"] + "-build.json"), item)
    for filename in ("COPYRIGHT", "AUTHORS"):
        shutil.copy2(UPSTREAM / filename, destination / filename)
    (destination / "manifest.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
    shutil.make_archive(str(root / "compat-package"), "gztar", root_dir=destination)
    print(f"package: {destination} (artifacts only; KaleidOS application execution is not implemented)")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("list", "build", "run", "test", "package"))
    parser.add_argument("--target", choices=TARGETS, default="linux")
    parser.add_argument("--out", type=Path, default=ROOT / "build" / "compat")
    parser.add_argument("--filter", default="", help="exact test name or category prefix, e.g. libc")
    parser.add_argument("--cc", help="GCC-compatible C compiler executable")
    parser.add_argument("--linkage", choices=("static", "dynamic"), default="static")
    parser.add_argument("--cflag", action="append", default=[], help="extra flag, use --cflag=-option")
    parser.add_argument("--ldflag", action="append", default=[], help="extra flag, use --ldflag=-option")
    parser.add_argument("--timeout", type=float, default=30)
    args = parser.parse_args()
    try:
        if args.timeout <= 0:
            raise ValueError("timeout must be positive")
        if args.action == "list":
            for case in cases(args.filter):
                print(f"{case['name']}\t{','.join(case['targets'])}\t{case['contract']}\t{case['source']}")
            return 0
        if args.action == "package":
            return package(args)
        if args.action in ("build", "test"):
            failed = build(args)
            if args.action == "build":
                return failed
        return run(args)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"compat: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
