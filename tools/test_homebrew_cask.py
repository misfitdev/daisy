"""Tests for release cask generation; no Homebrew installation required."""

import hashlib
import importlib.util
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest

spec = importlib.util.spec_from_file_location(
    "homebrew_cask", Path(__file__).with_name("homebrew-cask.py")
)
cask = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cask)


class CaskTests(unittest.TestCase):
    def test_release_version_and_actual_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            dmg = Path(directory) / "Daisy-0.6.0-macos-arm64.dmg"
            dmg.write_bytes(b"packaged release")
            recipe = cask.generate(dmg)
            self.assertIn('version "0.6.0"', recipe)
            self.assertIn(hashlib.sha256(dmg.read_bytes()).hexdigest(), recipe)
            dmg.write_bytes(b"changed release")
            self.assertNotEqual(recipe, cask.generate(dmg))

    def test_install_and_platform_constraints(self):
        with tempfile.TemporaryDirectory() as directory:
            dmg = Path(directory) / "Daisy-0.6.0-macos-arm64.dmg"
            dmg.write_bytes(b"release")
            recipe = cask.generate(dmg)
            self.assertIn('app "Daisy.app"', recipe)
            self.assertIn("depends_on arch: :arm64", recipe)
            self.assertIn("depends_on macos: :tahoe", recipe)
            self.assertIn("strategy :github_latest", recipe)
            self.assertIn("/v#{version}/Daisy-#{version}-macos-arm64.dmg", recipe)
            self.assertNotIn("auto_updates", recipe)
            self.assertNotIn("zap", recipe)

    def test_reject_wrong_architecture_and_prerelease(self):
        for name in ("Daisy-0.6.0-macos-x86_64.dmg", "Daisy-0.6.0-beta-macos-arm64.dmg"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                cask.generate(Path(name))

    def test_missing_package_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(FileNotFoundError):
                cask.generate(Path(directory) / "Daisy-0.6.0-macos-arm64.dmg")

    def test_cli_writes_release_recipe(self):
        with tempfile.TemporaryDirectory() as directory:
            dmg = Path(directory) / "Daisy-0.6.0-macos-arm64.dmg"
            recipe = Path(directory) / "daisy.rb"
            dmg.write_bytes(b"packaged release")
            result = subprocess.run(
                [sys.executable, str(spec.origin), str(dmg), str(recipe)],
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(recipe.read_text(), cask.generate(dmg))

    def test_cli_rejects_missing_package_without_writing_recipe(self):
        with tempfile.TemporaryDirectory() as directory:
            dmg = Path(directory) / "Daisy-0.6.0-macos-arm64.dmg"
            recipe = Path(directory) / "daisy.rb"
            result = subprocess.run(
                [sys.executable, str(spec.origin), str(dmg), str(recipe)],
                capture_output=True, text=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(recipe.exists())


if __name__ == "__main__":
    unittest.main()
