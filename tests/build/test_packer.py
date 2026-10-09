"""Packer must consume readelf output fully when pipefail is enabled."""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class Packer(unittest.TestCase):
    def test_large_readelf_output_cannot_hide_a_match_or_rejection(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            source = directory / "fixture.c"
            source.write_text("const unsigned long long kcomp_abi = 1;\n"
                              "int kcomp_instance_create(void *a, void *s) { return 0; }\n"
                              "int kcomp_instance_destroy(void *s) { return 0; }\n")
            obj = directory / "fixture.o"
            subprocess.run(["clang", "--target=riscv64-unknown-elf", "-ffunction-sections",
                            "-fdata-sections", "-mno-relax", "-c", str(source), "-o", str(obj)],
                           check=True)
            readelf = directory / "readelf"
            readelf.write_text("#!/usr/bin/env python3\n"
                               "import os, subprocess, sys\n"
                               "data = subprocess.check_output([os.environ['REAL_READELF'], *sys.argv[1:]])\n"
                               "if sys.argv[1] == '-r' and os.environ.get('INJECT_ALIGN'):\n"
                               "    data = b'R_RISCV_ALIGN\\n' + data\n"
                               "sys.stdout.buffer.write(data)\n"
                               "if sys.argv[1] in ('-h', '-r'):\n"
                               "    sys.stdout.buffer.write(b'padding\\n' * 100000)\n")
            readelf.chmod(0o755)
            env = dict(os.environ, READELF=str(readelf), REAL_READELF=shutil.which("llvm-readelf"))
            command = [str(ROOT / "tools/kcomp-link.sh"), str(directory / "fixture.kcomp"), str(obj)]
            accepted = subprocess.run(command, env=env, capture_output=True, text=True)
            self.assertEqual(accepted.returncode, 0, accepted.stderr)
            rejected = subprocess.run(command, env=dict(env, INJECT_ALIGN="1"),
                                      capture_output=True, text=True)
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn("R_RISCV_ALIGN present", rejected.stderr)
