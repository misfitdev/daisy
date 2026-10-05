"""Validate a supplied profile and prepare Daisy's Keychain signing entitlement."""

import datetime
import fnmatch
import hashlib
import pathlib
import plistlib
import re
import shutil
import subprocess
import sys


def prepare(profile_path, bundle_id, identity, app_path, entitlements_path):
    profile_path = pathlib.Path(profile_path)
    decoded = subprocess.run(
        ["security", "cms", "-D", "-i", str(profile_path)], check=True, capture_output=True
    ).stdout
    profile = plistlib.loads(decoded)
    if profile["ExpirationDate"] <= datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None):
        raise ValueError("the provisioning profile has expired")
    if "OSX" not in profile.get("Platform", []):
        raise ValueError("the provisioning profile must target macOS")
    allowed = profile["Entitlements"]
    prefix = profile["ApplicationIdentifierPrefix"][0]
    app_id = f"{prefix}.{bundle_id}"
    if not fnmatch.fnmatchcase(app_id, allowed.get("com.apple.application-identifier", "")):
        raise ValueError("the provisioning profile does not authorize Daisy's application identifier")
    available = subprocess.run(
        ["security", "find-identity", "-v", "-p", "codesigning"], check=True, capture_output=True, text=True
    ).stdout
    matches = {digest.lower() for digest, name in re.findall(r'([A-Fa-f0-9]{40}) "([^"\n]+)"', available)
               if identity.lower() == digest.lower() or identity in name}
    if len(matches) != 1:
        raise ValueError("the signing identity must select exactly one installed certificate")
    certificate_ids = {hashlib.sha1(cert).hexdigest() for cert in profile["DeveloperCertificates"]}
    if next(iter(matches)) not in certificate_ids:
        raise ValueError("the provisioning profile does not authorize the selected signing certificate")
    entitlements = {
        "com.apple.application-identifier": app_id,
        "com.apple.developer.team-identifier": profile["TeamIdentifier"][0],
    }
    destination = pathlib.Path(app_path) / "Contents" / "embedded.provisionprofile"
    shutil.copyfile(profile_path, destination)
    with pathlib.Path(entitlements_path).open("wb") as output:
        plistlib.dump(entitlements, output)


if __name__ == "__main__":
    try:
        prepare(*sys.argv[1:])
    except (ValueError, KeyError, subprocess.CalledProcessError) as error:
        sys.exit(f"Daisy signing profile: {error}")
