#!/usr/bin/env python3
"""Run one Cargo check across the Make-owned crate inventory."""
import argparse
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["fmt", "fmt-check", "clippy", "test"])
    parser.add_argument("--target")
    parser.add_argument("crates", nargs="+")
    args = parser.parse_args()
    for crate in args.crates:
        command = ["cargo", "fmt" if args.mode.startswith("fmt") else args.mode,
                   "--manifest-path", str(Path(crate) / "Cargo.toml")]
        if args.mode == "fmt-check":
            command += ["--", "--check"]
        elif args.mode == "clippy":
            if args.target:
                command += ["--target", args.target]
            else:
                command += ["--all-targets"]
                if crate == "os/core":
                    command += ["--features", "test-fixtures"]
            command += ["--", "-D", "warnings"]
        subprocess.run(command, check=True)


if __name__ == "__main__":
    main()
