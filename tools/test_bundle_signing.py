"""Exercise the real recipe's signing block on the system Bash with fake signers."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class BundleSigning(unittest.TestCase):
    def run_signing(self, identity, profile=None):
        root = Path(__file__).resolve().parents[1]
        recipe = (root / "justfile").read_text()
        start = recipe.index('    timestamp=""', recipe.index("bundle:"))
        end = recipe.index('\n    echo "signed', start)
        block = "\n".join(line.removeprefix("    ") for line in recipe[start:end].splitlines())
        with tempfile.TemporaryDirectory(prefix="daisy-sign-test-") as directory:
            work = Path(directory)
            app = work / "app with spaces.app"
            log = work / "signing.log"
            block = block.replace("{{bundle_id}}", "dev.misfit.daisy").replace("{{app}}", str(app))
            (work / "codesign").write_text('#!/bin/sh\nprintf "%s\\n" "$@" >> "$SIGN_LOG"\nprintf "END\\n" >> "$SIGN_LOG"\n')
            (work / "python3").write_text('#!/bin/sh\ntouch "$6"\n')
            for name in ["codesign", "python3"]:
                (work / name).chmod(0o700)
            environment = dict(os.environ, PATH=str(work) + ":/usr/bin:/bin", SIGN_LOG=str(log))
            environment.pop("DAISY_PROVISIONING_PROFILE", None)
            if profile is not None:
                environment["DAISY_PROVISIONING_PROFILE"] = profile
            # Environment inputs are assigned by Bash itself, avoiding shell interpolation.
            environment["TEST_SIGN_IDENTITY"] = identity
            environment["TEST_ICON_WORK"] = str(work)
            result = subprocess.run(["/bin/bash", "-c", 'set -euo pipefail\nidentity="$TEST_SIGN_IDENTITY"\nicon_work="$TEST_ICON_WORK"\n' + block],
                                    env=environment, capture_output=True, text=True)
            calls = [part.splitlines() for part in log.read_text().split("END\n") if part] if log.exists() else []
            return result, calls, str(app), str(work / "entitlements.plist")

    def test_ad_hoc_without_profile_signs_and_verifies(self):
        result, calls, app, _ = self.run_signing("-")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [["--force", "--options", "runtime", "--sign", "-", "--identifier", "dev.misfit.daisy", app],
                                 ["--verify", "--strict", app]])

    def test_publisher_profile_preserves_entitlements_and_space_containing_arguments(self):
        identity = "Developer ID Application: Test"
        result, calls, app, entitlements = self.run_signing(identity, "/tmp/profile with spaces.provisionprofile")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [["--force", "--options", "runtime", "--entitlements", entitlements, "--timestamp",
                                 "--sign", identity, "--identifier", "dev.misfit.daisy", app], ["--verify", "--strict", app]])

    def test_publisher_without_profile_refuses_before_signing(self):
        result, calls, _, _ = self.run_signing("Developer ID Application: Test")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("DAISY_PROVISIONING_PROFILE", result.stderr)
        self.assertEqual(calls, [])


if __name__ == "__main__":
    unittest.main()
