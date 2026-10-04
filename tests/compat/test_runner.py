#!/usr/bin/env python3
"""Host orchestration contracts; fixture executables do not test any libc."""

import contextlib
import io
import json
from pathlib import Path
from types import SimpleNamespace
import subprocess
import sys
import tempfile
import unittest

import runner


@unittest.skipUnless(sys.platform.startswith("linux"), "POSIX host process fixtures")
class ReferenceRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.directory = self.root / "linux"
        (self.directory / "bin").mkdir(parents=True)
        (self.directory / "logs").mkdir()
        self.binary = self.directory / "bin" / "fixture"
        self.args = SimpleNamespace(out=self.root, target="linux",
                                    filter="libc/memcpy", timeout=0.1)
        self.data = dict(machine="host-fixture", upstream_revision="fixture", tests=[])

    def fixture(self, body):
        self.binary.write_text("#!/bin/sh\n" + body + "\n", encoding="utf-8")
        self.binary.chmod(0o755)
        self.data["tests"] = [dict(name="libc/memcpy", status="BUILT",
                                   binary="bin/fixture", sha256=runner.digest(self.binary))]
        runner.write_json(self.directory / "build.json", self.data)

    def execute(self):
        with contextlib.redirect_stdout(io.StringIO()):
            status = runner.run(self.args)
        results = json.loads((self.directory / "results.json").read_text(encoding="utf-8"))
        return status, results["results"][0]

    def test_pass_runs_with_private_cwd_and_closed_stdin(self):
        self.fixture('test ! -f build.json || exit 7\nread value && exit 8\nexit 0')
        status, row = self.execute()
        self.assertEqual((status, row["status"], row["exit_status"]), (0, "PASS", 0))

    def test_failed_assertion_retains_exit_and_diagnostic(self):
        self.fixture('echo "upstream.c:37: failed" >&2\nexit 7')
        status, row = self.execute()
        self.assertEqual((status, row["status"], row["exit_status"]), (1, "FAIL", 7))
        self.assertIn("upstream.c:37", (self.directory / "logs" / "fixture.run.log").read_text())

    def test_signal_is_a_crash(self):
        self.fixture("ulimit -c 0\nkill -SEGV $$")
        status, row = self.execute()
        self.assertEqual((status, row["status"]), (1, "CRASH"))
        self.assertLess(row["exit_status"], 0)

    def test_timeout_is_a_failure(self):
        self.fixture("exec sleep 5")
        status, row = self.execute()
        self.assertEqual((status, row["status"]), (1, "TIMEOUT"))

    def test_missing_or_changed_binary_cannot_pass(self):
        for missing in (False, True):
            with self.subTest(missing=missing):
                self.fixture("exit 0")
                if missing:
                    self.binary.unlink()
                else:
                    self.binary.write_text("#!/bin/sh\nexit 7\n")
                status, row = self.execute()
                self.assertEqual((status, row["status"]), (1, "LOAD_FAIL"))

    def test_build_failure_preserves_compiler_exit(self):
        self.data["tests"] = [dict(name="libc/memcpy", status="BUILD_FAIL", exit_status=9)]
        runner.write_json(self.directory / "build.json", self.data)
        status, row = self.execute()
        self.assertEqual((status, row["status"], row["exit_status"]), (1, "BUILD_FAIL", 9))

    def test_unsupported_remains_distinct_from_pass(self):
        self.data["tests"] = [dict(name="libc/memcpy", status="UNSUPPORTED", reason="fixture")]
        runner.write_json(self.directory / "build.json", self.data)
        status, row = self.execute()
        self.assertEqual((status, row["status"], row["exit_status"]), (0, "UNSUPPORTED", None))

    def test_unbuilt_filter_rejects_and_clears_stale_results(self):
        self.fixture("exit 0")
        self.execute()
        self.args.filter = "libc/memset"
        with self.assertRaisesRegex(ValueError, "tests were not built"):
            runner.run(self.args)
        self.assertFalse((self.directory / "results.json").exists())

    def test_linux_cannot_report_a_windows_reference(self):
        self.args.target = "windows"
        with self.assertRaisesRegex(ValueError, "requires native windows"):
            runner.run(self.args)

    def test_missing_toolchain_invalidates_previous_pass(self):
        self.fixture("exit 0")
        self.execute()
        self.args.cc = str(self.root / "missing-compiler")
        with self.assertRaises(FileNotFoundError):
            runner.build(self.args)
        self.assertFalse((self.directory / "build.json").exists())
        self.assertFalse((self.directory / "results.json").exists())

    def test_compat_make_goals_do_not_create_kernel_config(self):
        config = self.root / ".config"
        result = subprocess.run(
            ["make", "-n", f"KCONFIG_CONFIG={config}", "compat-linux", "compat-windows",
             "test-compat-linux", "test-compat-windows", "compat-package"],
            cwd=runner.ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("scripts/kconfig/", result.stdout)
        self.assertFalse(config.exists())


if __name__ == "__main__":
    unittest.main()
