#!/usr/bin/env python3
"""Build real component artifacts and an isolated newc package."""
import argparse
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def newc(files):
    archive = bytearray()
    for name, data in [*files, ("TRAILER!!!", b"")]:
        fields = [0, 0o100644, 0, 0, 1, 0, len(data), 0, 0, 0, 0,
                  len(name.encode()) + 1, 0]
        archive.extend(("070701" + "".join(f"{n:08x}" for n in fields)).encode())
        archive.extend(name.encode() + b"\0")
        archive.extend(b"\0" * (-len(archive) % 4))
        archive.extend(data)
        archive.extend(b"\0" * (-len(archive) % 4))
    return archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rust", nargs="*", default=[])
    parser.add_argument("--c", nargs="*", default=[])
    parser.add_argument("--catalog", nargs="*")
    args = parser.parse_args()
    directory = args.output.resolve().parent
    artifacts = directory / "components"
    artifacts.mkdir(parents=True, exist_ok=True)
    names = []
    for language, sources in [("rust", args.rust), ("c", args.c)]:
        for source in sources:
            name = Path(source).name
            if name in names:
                parser.error(f"duplicate component basename: {name}")
            env = dict(os.environ)
            env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") +
                                f" --remap-path-prefix={ROOT}=.").strip()
            env["KALEIDOS_EXEC_FIXTURES"] = str(directory / "exec-fixtures")
            if name == "fatfs":
                env["CFLAGS"] = (env.get("CFLAGS", "") +
                    f" -I{ROOT}/third_party/fatfs/source -include "
                    f"{ROOT}/os/components/filesystems/fatfs/ffconf.h")
            elif name == "littlefs":
                env["CFLAGS"] = (env.get("CFLAGS", "") +
                    f" -I{ROOT}/third_party/littlefs -DLFS_NO_MALLOC -DLFS_NO_ASSERT"
                    " -DLFS_NO_DEBUG -DLFS_NO_WARN -DLFS_NO_ERROR")
            script = "build-kcomp.sh" if language == "rust" else "build-kcomp-c.sh"
            subprocess.run([str(ROOT / "tools" / script),
                            str(ROOT / "os/components" / source), args.target,
                            str(artifacts / f"{name}.kcomp"),
                            str(directory / f"component-{language}")], check=True, env=env)
            names.append(name)
    catalog = names if args.catalog is None else args.catalog
    if any(name not in names for name in catalog):
        parser.error("catalog references a component that was not built")
    # Only selected artifacts enter the package; stale files cannot leak in.
    files = [("manifest", "".join(f"{name}.kcomp\n" for name in catalog).encode())]
    files += [(f"{name}.kcomp", (artifacts / f"{name}.kcomp").read_bytes())
              for name in catalog]
    staged = args.output.with_name(args.output.name + ".tmp")
    staged.write_bytes(newc(files))
    staged.replace(args.output)
    print(f"packed: {args.output} ({len(catalog)} components)")


if __name__ == "__main__":
    main()
