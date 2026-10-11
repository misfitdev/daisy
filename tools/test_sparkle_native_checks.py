"""Check native fixture assertions without starting an application."""
from pathlib import Path
import plistlib
import tempfile
import unittest

from sparkle_fixtures import INSTALLED_VERSION, UPDATE_VERSION
from test_sparkle_native import verify_result


class NativeChecks(unittest.TestCase):
    def test_manual_check_uses_the_final_installed_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            installed = Path(directory) / 'Daisy.app'
            info = installed / 'Contents/Info.plist'
            info.parent.mkdir(parents=True)
            info.write_bytes(plistlib.dumps({'CFBundleVersion': INSTALLED_VERSION}))
            log = 'sparkle://did-find-valid-update'
            verify_result('manual', installed, log)
            info.write_bytes(plistlib.dumps({'CFBundleVersion': UPDATE_VERSION}))
            with self.assertRaises(AssertionError):
                verify_result('manual', installed, log)


if __name__ == '__main__':
    unittest.main()
