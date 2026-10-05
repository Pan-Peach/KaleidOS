"""Build ownership contracts, exercised through the public Make interface."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class Outputs(unittest.TestCase):
    def make(self, directory, *goals):
        return subprocess.run(["make", f"O={directory}", *goals], cwd=ROOT,
                              capture_output=True, text=True, check=True).stdout

    def test_clean_preserves_config_and_other_profile(self):
        with tempfile.TemporaryDirectory() as temporary:
            first, second = Path(temporary) / "first", Path(temporary) / "second"
            self.make(first, "defconfig")
            self.make(second, "qemu_rv32_defconfig")
            config = (first / ".config").read_bytes()
            (first / "cargo").mkdir()
            (first / "cargo" / "artifact").write_text("obsolete")
            (first / "kaleidos.elf").write_text("obsolete")
            other = (second / ".config").read_bytes()
            self.make(first, "clean")
            self.assertEqual((first / ".config").read_bytes(), config)
            self.assertFalse((first / "cargo").exists())
            self.assertFalse((first / "kaleidos.elf").exists())
            self.assertEqual((second / ".config").read_bytes(), other)

    def test_production_and_coretest_packages_select_different_inventory(self):
        with tempfile.TemporaryDirectory() as temporary:
            self.make(temporary, "defconfig")
            production = self.make(temporary, "-n", "init.kpkg")
            self.assertNotIn("tests/core_test", production)
            self.assertNotIn("tests/kcomp_", production)
            self.assertNotIn("exec_fixtures.py", production)
            self.make(temporary, "coretest_defconfig")
            testing = self.make(temporary, "-n", "init.kpkg")
            self.assertIn("tests/core_test", testing)
            self.assertIn("tests/kcomp_heap", testing)
            self.assertIn("exec_fixtures.py", testing)

    def test_core_build_cannot_overwrite_full_package(self):
        with tempfile.TemporaryDirectory() as temporary:
            self.make(temporary, "defconfig")
            commands = self.make(temporary, "-n", "core")
            self.assertIn(f"--output {temporary}/core/init.kpkg", commands)
            self.assertNotIn(f"--output {temporary}/init.kpkg", commands)

    def test_selftest_fragment_enables_fixtures_on_resolved_production_config(self):
        with tempfile.TemporaryDirectory() as temporary:
            self.make(temporary, "defconfig")
            self.make(temporary, "selftest_defconfig")
            config = (Path(temporary) / ".config").read_text()
            self.assertIn("CONFIG_SELFTEST=y", config)
            self.assertIn("CONFIG_TEST_COMPONENTS=y", config)
            self.assertIn("tests/kcomp_isolated", self.make(temporary, "-n", "init.kpkg"))


if __name__ == "__main__":
    unittest.main()
