"""Schema validation, generated C/Rust wire goldens and real handler dispatch.

This is a host codec test. It does not prove Core IPC or execution-domain isolation.
"""
import importlib.util
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("kabi_methods_generator", ROOT / "tools/kabi/kabi_gen.py")
kabi = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = kabi
spec.loader.exec_module(kabi)


def numbers_schema():
    text = '[[method]]\nname = "numbers"\nid = 9\nsymbol = "NUMBERS"\n'
    for section, prefix in (("args", "a"), ("reply", "b")):
        for type_ in ("u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64"):
            text += f'[[method.{section}]]\nname = "{prefix}_{type_}"\ntype = "{type_}"\n'
    return text


FLUSH = '[[method]]\nname = "flush"\nid = 3\nsymbol = "FLUSH"\n'


class KabiMethods(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.directory = Path(cls.temporary.name)
        abi = cls.directory / "abi"
        abi.mkdir()
        cls.schema_path = abi / "methods.toml"
        # Adding flush edits only the schema and semantic handler/test. Both
        # languages' clients/codec/dispatch emerge from the same generator.
        cls.schema_path.write_text(numbers_schema() + FLUSH)
        schema = kabi.load_schema(str(cls.schema_path))
        for target, name, emitter in (("wire-rust", "methods_wire.rs", kabi.WireRustEmitter),
                                      ("wire-c", "methods_wire.h", kabi.WireCEmitter)):
            output = kabi.Output(target, name, ("methods.toml",), guard="METHODS_WIRE_H")
            (cls.directory / name).write_text(emitter().render([schema], output))
        sdk = ROOT / "os/components/kcomp-sdk"
        cls.binaries = [cls.directory / "c", cls.directory / "rust"]
        subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-fsanitize=undefined",
                        "-iquote", str(sdk / "include"), "-iquote", str(cls.directory),
                        str(ROOT / "tests/components/generated_wire.c"), str(sdk / "c/kcomp_ipc.c"),
                        "-o", str(cls.binaries[0])], check=True)
        subprocess.run(["cargo", "build", "--quiet", "--manifest-path", str(sdk / "Cargo.toml"),
                        "--target-dir", str(cls.directory / "cargo")], check=True)
        harness = cls.directory / "main.rs"
        harness.write_bytes((ROOT / "tests/components/generated_wire.rs").read_bytes())
        subprocess.run(["rustc", "--edition=2024", str(harness), "--extern",
                        f'kcomp_sdk={cls.directory / "cargo/debug/libkcomp_sdk.rlib"}',
                        "-o", str(cls.binaries[1])], check=True)

    def run_both(self, *args):
        return [subprocess.run([str(binary), *map(str, args)], capture_output=True, text=True,
                               check=True, timeout=5).stdout.strip() for binary in self.binaries]

    def test_clients_and_dispatch_match_independent_golden(self):
        integers = struct.pack("<BHIQbhiq", 0xff, 0xabcd, 0x89abcdef, 0xfedcba9876543210,
                               -128, -32768, -(2**31), -(2**63))
        cases = [("echo", "6563686f00", 5, 247, b"", b"echo\x00", b"echo\x00"),
                 ("echo", "", 0, 247, b"", b"", b""),
                 ("block", "capacity", 8, 0, b"", b"", struct.pack("<Q", 0x0102030405060708)),
                 ("block", "7", 512, 1, struct.pack("<Q", 7), b"", bytes([7])*512),
                 ("block", "write", 512, 2, struct.pack("<Q", 7), bytes([0x81])*512, b""),
                 ("numbers", "", 30, 9, integers, b"", integers),
                 ("flush", "", 0, 3, b"", b"", b"")]
        cases += [("filesystem", "root", 8, 5, b"", b"", struct.pack("<Q",42)),
                  ("filesystem", "lookup", 8, 6, struct.pack("<QI",42,1), b"HELLO.TXT", struct.pack("<Q",42)),
                  ("filesystem", "details", 12, 8, struct.pack("<Q",42), b"", struct.pack("<IIQ",1,9,23)+b"HELLO.TXT"+bytes(3)),
                  ("filesystem", "open", 8, 9, struct.pack("<Q",42), b"", struct.pack("<Q",42)),
                  ("filesystem", "read_at", 7, 10, struct.pack("<QQ",42,7), b"", struct.pack("<Q",3)+b"abc"+bytes(4)),
                  ("filesystem", "read_at", 0, 10, struct.pack("<QQ",42,7), b"", bytes(8)),
                  ("filesystem", "close", 0, 3, struct.pack("<Q",42), b"", b"")]
        for contract, arg, capacity, method, args, input_, output in cases:
            with self.subTest(contract=contract, arg=arg):
                request = struct.pack("<IIII", method, len(output), len(args), len(input_))+args+input_
                expected = f'request={request.hex()}\nreply={(bytes(4)+output).hex()}\ntransport=0 method=0 calls=1'
                self.assertEqual(self.run_both("client", contract, arg, capacity), [expected]*2)
                expected_dispatch = f'status=0 calls=1 output={output.hex()}'
                self.assertEqual(self.run_both("dispatch", contract, request.hex(), len(output)), [expected_dispatch]*2)

    def test_shape_errors_do_not_execute_handler(self):
        def frame(method, capacity, args=b"", input_=b""):
            return struct.pack("<IIII", method, capacity, len(args), len(input_))+args+input_
        frames = [("block", frame(1, 512, b"1234567"), 512, -22),
                  ("block", frame(1, 511, bytes(8)), 511, -22),
                  ("block", frame(0, 7), 7, -22),
                  ("block", frame(0, 8), 7, -22),
                  ("block", frame(0, 8, input_=b"x"), 8, -22),
                  ("echo", frame(247, 2, input_=b"x"), 2, -22),
                  ("echo", frame(247, 1009, input_=bytes(1009)), 1009, -22),
                  ("echo", frame(247, 1, input_=b"x")+b"x", 1, -22),
                  ("flush", frame(10, 0), 0, -38)]
        frames += [("filesystem", frame(8, 27, bytes(8)), 27, -22),
                   ("filesystem", frame(10, 7, bytes(16)), 7, -22),
                   ("filesystem", frame(10, 521, bytes(16)), 521, -22),
                   ("filesystem", frame(6, 8, bytes(12)), 8, -22)]
        for contract, request, capacity, status in frames:
            with self.subTest(contract=contract, request=request[:16]):
                expected = f'status={status} calls=0 output={bytes(capacity).hex()}'
                self.assertEqual(self.run_both("dispatch", contract, request.hex(), capacity), [expected]*2)

    def test_transport_and_business_errors_remain_distinct(self):
        self.assertEqual(self.run_both("client", "block", "7", 512, "transport"),
                         ["request="+(struct.pack("<IIIIQ", 1, 512, 8, 0, 7)).hex()+"\ntransport=-13 calls=0"]*2)
        result = self.run_both("client", "block", "13", 512)
        self.assertEqual(result[0], result[1])
        self.assertTrue(result[0].endswith("transport=0 method=-5 calls=1"))
        self.assertEqual(self.run_both("client", "block", "7", 511), ["transport=0 method=-22 calls=0"]*2)

    def test_schema_rejects_unsafe_types_and_ambiguous_methods(self):
        base = '[[method]]\nname = "read"\nid = 1\nsymbol = "READ"\n'
        invalid = [base + base, base + FLUSH.replace("id = 3", "id = 1"),
                   base + FLUSH.replace('symbol = "FLUSH"', 'symbol = "READ"'),
                   base.replace("id = 1", "id = -1"), base.replace("id = 1", "id = 4294967296"),
                   base + 'extra = 1\n', base + '[method.output]\nmin = 8\nmax = 7\n',
                   base + '[method.input]\nmin = -1\nmax = 8\n',
                   base + '[method.output]\nmin = 0\nmax = 1021\n',
                   base + '[method.input]\nmin = 0\nmax = 1009\n',
                   base + '[method.output]\nmin = 0\nmax = 8\nmatches = "input"\n']
        for type_ in ("usize", "*mut u8", "fn() -> u64", "bool", "Unknown"):
            invalid.append(base + f'[[method.args]]\nname = "lba"\ntype = "{type_}"\n')
        for name in ("for", "impl", "input", "wire_args"):
            invalid.append(base + f'[[method.args]]\nname = "{name}"\ntype = "u64"\n')
        for text in invalid:
            with self.subTest(text=text):
                self.schema_path.write_text(text)
                with self.assertRaises(kabi.KabiError):
                    kabi.load_schema(str(self.schema_path))

    def test_fallback_toml_preserves_method_structure(self):
        text = numbers_schema() + FLUSH
        import tomllib
        self.assertEqual(kabi._parse_toml_subset(text), tomllib.loads(text))


if __name__ == "__main__":
    unittest.main()
