"""Byte compatibility of the actual SDK envelopes, not an IPC isolation test."""
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class IpcCodec(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.binaries = [Path(cls.temporary.name) / name for name in ("c", "rust")]
        sdk = ROOT / "os/components/kcomp-sdk"
        subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror",
                        "-iquote", str(sdk / "include"),
                        str(ROOT / "tests/components/ipc_codec.c"),
                        str(sdk / "c/kcomp_ipc.c"), "-o", str(cls.binaries[0])], check=True)
        subprocess.run(["rustc", "--edition=2024",
                        str(ROOT / "tests/components/ipc_codec.rs"),
                        "-o", str(cls.binaries[1])], check=True)

    def run_both(self, *args):
        return [subprocess.run([str(binary), *map(str, args)], check=True,
                               capture_output=True, text=True, timeout=5).stdout.strip()
                for binary in self.binaries]

    def test_echo_and_block_read_match_independent_golden_bytes(self):
        cases = [(0xf7, b"", b"echo\x00", 5),
                 (1, struct.pack("<Q", 0x0102030405060708), b"", 512)]
        for method, args, data, capacity in cases:
            with self.subTest(method=method):
                request = struct.pack("<IIII", method, capacity, len(args), len(data)) + args + data
                expected = ("request=" + request.hex() + "\ntransport=0 method=0 output="
                            + (b"\xa5" * capacity).hex())
                self.assertEqual(self.run_both("invoke", method, args.hex(), data.hex(), capacity, 0),
                                 [expected, expected])

    def test_message_boundaries_and_transport_method_error_separation(self):
        for status in (0, -5, "transport", 1):
            with self.subTest(status=status):
                result = self.run_both("invoke", 0xf7, "", "", 0, status)
                self.assertEqual(result[0], result[1])
                suffix = ("transport=-5" if status == "transport" else "transport=-71"
                          if status == 1 else f"transport=0 method={status} output=")
                self.assertTrue(result[0].endswith(suffix))
        for input_len, output_len in ((1008, 1020), (1009, 0), (0, 1021)):
            result = self.run_both("invoke", 9, "", bytes(input_len).hex(), output_len, 0)
            self.assertEqual(result[0], result[1])
            if input_len > 1008 or output_len > 1020:
                self.assertEqual(result[0], "transport=-90")

    def test_decoders_agree_on_valid_and_malformed_frames(self):
        valid = struct.pack("<IIII", 1, 512, 8, 0) + struct.pack("<Q", 7)
        frames = [valid, b"", valid[:15], valid[:-1], valid + b"x",
                  struct.pack("<IIII", 1, 0, 0xffffffff, 0),
                  struct.pack("<IIII", 1, 1021, 0, 0)]
        for index, frame in enumerate(frames):
            with self.subTest(index=index):
                result = self.run_both("decode", frame.hex())
                self.assertEqual(result[0], result[1])
                if index:
                    self.assertEqual(result[0], "error=-22")

    @unittest.expectedFailure
    def test_known_rust_decoder_gap_rejects_message_larger_than_core_limit(self):
        # HEAD 2e10304: C rejects >1024; Rust Request::decode accepts this.
        # Core submit already enforces the bound. Remove xfail with decoder fix.
        frame = struct.pack("<IIII", 1, 0, 0, 1009) + bytes(1009)
        self.assertEqual(self.run_both("decode", frame.hex()), ["error=-22", "error=-22"])


if __name__ == "__main__":
    unittest.main()
