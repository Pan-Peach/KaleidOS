"""Reject incomplete reports and verify failure logs without booting a kernel."""
from argparse import Namespace
from pathlib import Path
import sys
import tempfile
import unittest
from common import RunFailure, Session
from protocol import core_test_report
from arch_runner import run_case, select_cases

REPORT = """[core-test] KTAP version 1
[core-test] # group
[core-test] ok 1 - first
[core-test] ok 2 - second
[core-test] 1..2
[core-test] all: PASS
"""


class Reports(unittest.TestCase):
    def test_complete_report(self):
        self.assertEqual(core_test_report(REPORT), ["first", "second"])

    def test_broken_reports_fail_closed(self):
        for report in [REPORT.replace("ok 2 - second\n", ""),
                       REPORT.replace("second", "first"),
                       REPORT.replace("ok 2", "ok 3"),
                       REPORT.replace("ok 2", "not ok 2"),
                       REPORT.replace("second", "second # SKIP unavailable"),
                       REPORT.replace("1..2", "1..3"),
                       REPORT.replace("KTAP version 1", "missing"),
                       REPORT.replace("all: PASS", ""),
                       REPORT + "[core-test] ok 3 - late\n",
                       REPORT + "[core-test] 1..2\n",
                       "[core-test] KTAP version 1\n[core-test] 1..0\n[core-test] all: PASS"]:
            with self.subTest(report=report), self.assertRaises(RunFailure):
                core_test_report(report)

    def test_case_selection_does_not_repeat_base_suite(self):
        self.assertEqual(len(select_cases("rv64", selected="timer")), 1)
        self.assertEqual([case[0] for case in select_cases("rv64", smp_only=True)],
                         ["smp-boot", "smp-ipi", "smp-percpu"])
        with self.assertRaises(RunFailure):
            select_cases("rv64", selected="does-not-exist")
        with self.assertRaises(RunFailure):
            select_cases("rv32", smp_only=True)


class Serial(unittest.TestCase):
    def session(self, temporary):
        return Session(Namespace(kernel=Path(temporary) / "kernel", work_dir=None,
                                 log_dir=None), "probe")

    def test_partial_fatal_output_is_kept_and_guest_reaped(self):
        with tempfile.TemporaryDirectory() as temporary:
            session = self.session(temporary)
            with self.assertRaises(RunFailure), session:
                proc = session.start([sys.executable, "-c",
                    "import sys,time; sys.stdout.write('PANIC: partial'); sys.stdout.flush(); time.sleep(5)"])
                session.collect(proc, 1, ["ready"], ["PANIC:"])
            self.assertIn("PANIC: partial", session.log_path.read_text())
            self.assertIsNotNone(proc.poll())
            self.assertFalse(session.directory.exists())

    def test_nonzero_exit_cannot_pass(self):
        with tempfile.TemporaryDirectory() as temporary:
            session = self.session(temporary)
            with self.assertRaises(RunFailure), session:
                session.start([sys.executable, "-c", "print('PASS'); raise SystemExit(7)"])
                session.shutdown(1)
            self.assertIn("PASS", session.log_path.read_text())

    def test_timeout_is_incomplete_and_disk_paths_are_unique(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.session(temporary) as first, self.session(temporary) as second:
                self.assertNotEqual(first.directory, second.directory)
                proc = first.start([sys.executable, "-c", "import time; time.sleep(5)"])
                _, complete = first.collect(proc, 0.03, ["missing"])
                self.assertFalse(complete)

    def test_arch_verdict_rejects_nonzero_exit_and_wrong_fault(self):
        with tempfile.TemporaryDirectory() as temporary:
            script = Path(temporary) / "guest.py"
            args = Namespace(kernel=Path(temporary) / "kernel", work_dir=None,
                             log_dir=None, qemu=sys.executable, qemu_flags=str(script),
                             memory="1G", arch="rv64")
            for verdict, status, cause in [("[selftest] probe: PASS", 7, None),
                                           ("PANIC: scause=0xd", 7, 13),
                                           ("PANIC: scause=0xf", 0, 13)]:
                script.write_text("import sys\nprint('[selftest] ready', flush=True)\n"
                                  "sys.stdin.readline()\n" + f"print({verdict!r}, flush=True)\n"
                                  f"raise SystemExit({status})\n")
                with self.subTest(verdict=verdict, status=status), self.assertRaises(RunFailure):
                    run_case(args, "probe", cause, None)


if __name__ == "__main__":
    unittest.main()
