"""Host concurrency/session regressions against actual C providers and upstream FS."""
from pathlib import Path
import subprocess
import re
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SDK = ROOT / "os/components/kcomp-sdk/include"


class Providers(unittest.TestCase):
    def test_c_sdk_reads_into_plain_business_buffers(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary) / "client"
            subprocess.run(["cc", "-std=c11", "-O2", "-Werror", "-I" + str(SDK),
                            str(ROOT / "tests/components/filesystem_client.c"),
                            str(ROOT / "os/components/kcomp-sdk/c/kcomp_filesystem.c"),
                            "-o", str(binary)], check=True)
            subprocess.run([str(binary)], check=True, timeout=10)

    def test_c_block_ipc_splitting_overflow_and_partial_completion(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary) / "block"
            subprocess.run(["cc", "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror",
                            "-fsanitize=undefined", "-fno-sanitize-recover=undefined",
                            "-iquote", str(SDK), str(ROOT / "tests/components/block_client.c"),
                            str(ROOT / "os/components/kcomp-sdk/c/kcomp_block.c"),
                            "-o", str(binary)], check=True)
            subprocess.run([str(binary)], check=True, timeout=10)

    def test_production_providers_serialize_io_and_reject_stale_handles(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            disk = directory / "fat.img"
            disk.write_bytes(bytes(1024 * 1024))
            subprocess.run(["mkfs.fat", "-F", "12", str(disk)], check=True, capture_output=True)
            subprocess.run(["mcopy", "-i", str(disk),
                            str(ROOT / "tests/fixtures/rootfs/HELLO.TXT"), "::/HELLO.TXT"], check=True)
            for name in ("DIR1", "DIR2"):
                subprocess.run(["mmd", "-i", str(disk), "::/" + name], check=True)
                subprocess.run(["mcopy", "-i", str(disk),
                                str(ROOT / "tests/fixtures/rootfs/HELLO.TXT"),
                                "::/" + name + "/HELLO.TXT"], check=True)
            header = (ROOT / "os/components/filesystems/fatfs/fatfs_internal.h").read_text()
            capacity = int(re.search(r"#define FATFS_MAX_NODES (\d+)", header).group(1))
            for name in ("N" + str(index) for index in range(1, capacity + 1)):
                subprocess.run(["mcopy", "-i", str(disk),
                                str(ROOT / "tests/fixtures/rootfs/HELLO.TXT"), "::/" + name], check=True)
            for fs in ("fatfs", "littlefs"):
                with self.subTest(provider=fs):
                    component = ROOT / "os/components/filesystems" / fs
                    binary = directory / fs
                    command = ["cc", "-std=c11", "-O2", "-pthread", "-Werror",
                               "-I" + str(SDK), "-I" + str(component),
                               str(ROOT / "tests/components/filesystem_provider.c"),
                               str(component / (fs + "_backend.c"))]
                    if fs == "fatfs":
                        command += ["-DTEST_FATFS", "-include", str(component / "ffconf.h"),
                                    "-I" + str(ROOT / "third_party/fatfs/source"),
                                    str(component / "diskio_kaleidos.c"),
                                    str(component / "fatfs_service.c"),
                                    str(ROOT / "os/components/kcomp-sdk/c/kcomp_filesystem.c"),
                                    str(ROOT / "third_party/fatfs/source/ff.c")]
                    else:
                        command += ["-DLFS_NO_MALLOC", "-DLFS_NO_ASSERT", "-DLFS_NO_DEBUG",
                                    "-DLFS_NO_WARN", "-DLFS_NO_ERROR",
                                    "-I" + str(ROOT / "third_party/littlefs"),
                                    str(component / "lfs_adapter.c"),
                                    str(ROOT / "third_party/littlefs/lfs.c"),
                                    str(ROOT / "third_party/littlefs/lfs_util.c")]
                    subprocess.run(command + ["-o", str(binary)], check=True)
                    result = subprocess.run([str(binary), str(disk)], check=True,
                                            capture_output=True, text=True, timeout=10)
                    self.assertIn("interleaving PASS", result.stdout)
                    if fs == "fatfs":
                        self.assertIn("lookup/ipc-codec/stale/capacity PASS", result.stdout)
