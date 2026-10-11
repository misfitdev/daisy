"""Exercise framework bootstrap with fresh and incomplete caches."""

import hashlib
from pathlib import Path
import plistlib
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import sparkle


class FetchTests(unittest.TestCase):
    def test_bootstrap_repairs_partial_cache(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            framework = source / "Sparkle.framework"
            resources = framework / "Versions/B/Resources"
            resources.mkdir(parents=True)
            (resources / "Info.plist").write_bytes(plistlib.dumps({
                "CFBundleShortVersionString": sparkle.VERSION,
            }))
            (framework / "Versions/Current").symlink_to("B")
            (framework / "Resources").symlink_to("Versions/Current/Resources")
            (source / "bin").mkdir()
            (source / "bin/sign_update").write_text("fixture")
            destination = root / "cache"
            destination.mkdir()
            archive = destination / f"Sparkle-{sparkle.VERSION}.tar.xz"
            with tarfile.open(archive, "w:xz") as tar:
                tar.add(framework, arcname="Sparkle.framework")
                tar.add(source / "bin", arcname="bin")
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            with patch.object(sparkle, "DEST", destination), \
                    patch.object(sparkle, "SHA256", checksum), \
                    patch.object(sparkle.subprocess, "run") as codesign:
                sparkle.fetch()
                plist = destination / "Sparkle.framework/Resources/Info.plist"
                self.assertTrue(plist.is_file())
                plist.unlink()
                (destination / "bin/sign_update").unlink()
                sparkle.fetch()
                self.assertTrue(plist.is_file())
                self.assertEqual((destination / "bin/sign_update").read_text(), "fixture")
                self.assertTrue((destination / "Sparkle.framework/Resources").is_symlink())
                self.assertEqual(codesign.call_count, 2)


if __name__ == "__main__":
    unittest.main()
