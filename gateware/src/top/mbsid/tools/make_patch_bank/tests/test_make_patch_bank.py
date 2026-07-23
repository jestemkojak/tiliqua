import contextlib
import io
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tools.make_patch_bank import make_patch_bank as bank


def patch_bytes(seed: int) -> bytes:
    return bytes((index * 31 + seed) & 0xFF for index in range(512))


class CodecTests(unittest.TestCase):
    def test_encode_matches_firmware_layout_and_round_trips(self):
        patch = bytes([0xAB]) + patch_bytes(9)[1:]
        encoded = bank.encode_bank_patch(patch, 5)

        self.assertEqual(len(encoded), 1036)
        self.assertEqual(encoded[:10], bytes.fromhex("f0 00 00 7e 4b 00 02 00 01 05"))
        self.assertEqual(encoded[10:12], bytes.fromhex("0b 0a"))
        self.assertEqual(encoded[-1], 0xF7)
        self.assertEqual(encoded[-2], (-sum(encoded[10:1034])) & 0x7F)
        self.assertEqual(bank.decode_patch_message(encoded), (patch, 5))

    def test_decode_accepts_source_metadata_but_returns_embedded_slot(self):
        encoded = bytearray(bank.encode_bank_patch(patch_bytes(4), 73))
        encoded[7] = 0x7F
        encoded[8] = 0x00
        self.assertEqual(bank.decode_patch_message(bytes(encoded))[1], 73)

    def test_decode_rejects_each_malformed_field(self):
        valid = bytearray(bank.encode_bank_patch(patch_bytes(1), 0))
        mutations = {
            "length": bytes(valid[:-1]),
            "header": bytes([0xF0, 1]) + bytes(valid[2:]),
            "command": bytes(valid[:6]) + bytes([3]) + bytes(valid[7:]),
            "metadata": bytes(valid[:7]) + bytes([0x80]) + bytes(valid[8:]),
            "nibble": bytes(valid[:10]) + bytes([0x10]) + bytes(valid[11:]),
            "checksum": bytes(valid[:-2]) + bytes([(valid[-2] + 1) & 0x7F, 0xF7]),
            "terminator": bytes(valid[:-1]) + bytes([0x00]),
        }
        for label, message in mutations.items():
            with self.subTest(label=label):
                with self.assertRaises(bank.PatchFormatError):
                    bank.decode_patch_message(message)

    def test_encode_rejects_bad_patch_length_and_slot(self):
        for patch, slot in ((bytes(511), 0), (bytes(512), -1), (bytes(512), 128)):
            with self.subTest(length=len(patch), slot=slot):
                with self.assertRaises(ValueError):
                    bank.encode_bank_patch(patch, slot)


class DiscoveryTests(unittest.TestCase):
    def test_discovers_immediate_syx_files_in_natural_casefolded_order(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for name in ("Patch10.SYX", "patch2.syx", "PATCH1.sYx", "notes.txt"):
                (root / name).write_bytes(b"x")
            (root / "nested").mkdir()
            (root / "nested" / "patch0.syx").write_bytes(b"x")

            found = bank.discover_syx_files(root)

            self.assertEqual([path.name for path in found],
                             ["PATCH1.sYx", "patch2.syx", "Patch10.SYX"])

    def test_discovery_excludes_resolved_output(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            output = root / "BANK.SYX"
            output.write_bytes(b"old bank")
            (root / "patch1.syx").write_bytes(b"patch")
            self.assertEqual(bank.discover_syx_files(root, output),
                             [root / "patch1.syx"])


class BankAssemblyTests(unittest.TestCase):
    def write_patch(self, root: Path, name: str, seed: int, slot: int) -> Path:
        path = root / name
        path.write_bytes(bank.encode_bank_patch(patch_bytes(seed), slot))
        return path

    def test_default_mode_skips_bad_file_without_consuming_slot(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            first = self.write_patch(root, "p1.syx", 1, 90)
            bad = root / "p2.syx"
            bad.write_bytes(b"bad")
            third = self.write_patch(root, "p3.syx", 3, 91)

            result = bank.build_bank([first, bad, third])

            self.assertEqual((result.included, result.skipped), (2, 1))
            self.assertIn("p2.syx", result.warnings[0])
            self.assertEqual(bank.decode_patch_message(result.data[:1036]),
                             (patch_bytes(1), 0))
            self.assertEqual(bank.decode_patch_message(result.data[1036:]),
                             (patch_bytes(3), 1))

    def test_preserve_mode_keeps_first_duplicate_and_retains_gaps(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            first = self.write_patch(root, "p1.syx", 1, 7)
            duplicate = self.write_patch(root, "p2.syx", 2, 7)
            later = self.write_patch(root, "p3.syx", 3, 42)

            result = bank.build_bank([first, duplicate, later], True)

            self.assertEqual((result.included, result.skipped), (2, 1))
            self.assertIn("duplicate slot 7", result.warnings[0])
            self.assertEqual(bank.decode_patch_message(result.data[:1036]),
                             (patch_bytes(1), 7))
            self.assertEqual(bank.decode_patch_message(result.data[1036:]),
                             (patch_bytes(3), 42))

    def test_default_mode_warns_for_each_valid_patch_after_128(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            files = [
                self.write_patch(root, f"p{index:03}.syx", index, index & 0x7F)
                for index in range(130)
            ]

            result = bank.build_bank(files)

            self.assertEqual((result.included, result.skipped), (128, 2))
            self.assertEqual(len(result.data), 128 * 1036)
            self.assertTrue(all("bank already has 128 patches" in warning
                                for warning in result.warnings))

    def test_unreadable_file_is_skipped(self):
        with tempfile.TemporaryDirectory() as raw:
            missing = Path(raw) / "missing.syx"
            result = bank.build_bank([missing])
            self.assertEqual((result.included, result.skipped), (0, 1))
            self.assertIn("cannot read", result.warnings[0])


class CliTests(unittest.TestCase):
    def run_main(self, argv):
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = bank.main(argv)
        return status, stdout.getvalue(), stderr.getvalue()

    def test_default_output_and_summary(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "patch1.syx"
            source.write_bytes(bank.encode_bank_patch(patch_bytes(1), 88))
            previous = Path.cwd()
            os.chdir(root)
            try:
                status, stdout, stderr = self.run_main([str(root)])
            finally:
                os.chdir(previous)

            self.assertEqual(status, 0)
            self.assertEqual(stderr, "")
            self.assertTrue((root / "BANK.SYX").is_file())
            self.assertIn("1 patch", stdout)
            self.assertIn("0 skipped", stdout)

    def test_custom_output_preserves_embedded_slots_and_warns(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "source"
            source.mkdir()
            (source / "p1.syx").write_bytes(bank.encode_bank_patch(patch_bytes(1), 12))
            (source / "p2.syx").write_bytes(bank.encode_bank_patch(patch_bytes(2), 12))
            output = root / "custom.syx"

            status, stdout, stderr = self.run_main([
                str(source), "-o", str(output), "--preserve-patch-numbers"
            ])

            self.assertEqual(status, 0)
            self.assertIn("duplicate slot 12", stderr)
            self.assertIn("1 skipped", stdout)
            self.assertEqual(bank.decode_patch_message(output.read_bytes())[1], 12)

    def test_missing_directory_is_fatal(self):
        status, _, stderr = self.run_main(["does-not-exist"])
        self.assertEqual(status, 2)
        self.assertIn("not a directory", stderr)

    def test_zero_valid_patches_does_not_replace_output(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "bad.syx").write_bytes(b"bad")
            output = root / "existing.syx"
            output.write_bytes(b"keep me")

            status, _, stderr = self.run_main([str(root), "-o", str(output)])

            self.assertEqual(status, 1)
            self.assertIn("no valid patches", stderr)
            self.assertEqual(output.read_bytes(), b"keep me")

    def test_atomic_write_failure_is_fatal_and_leaves_destination(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            source = root / "patch.syx"
            source.write_bytes(bank.encode_bank_patch(patch_bytes(1), 0))
            output = root / "bank.syx"
            output.write_bytes(b"old")
            with mock.patch.object(bank.os, "replace", side_effect=OSError("replace failed")):
                status, _, stderr = self.run_main([str(root), "-o", str(output)])
            self.assertEqual(status, 1)
            self.assertIn("cannot write", stderr)
            self.assertEqual(output.read_bytes(), b"old")
            self.assertEqual(list(root.glob(".bank.syx.*")), [])


if __name__ == "__main__":
    unittest.main()
