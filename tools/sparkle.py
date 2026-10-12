"""Fetch the pinned native framework and prepare its signed application bundle."""
import argparse
import fcntl
import hashlib
from pathlib import Path
import plistlib
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

VERSION = "2.10.0"
SHA256 = "c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c"
ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / "target/sparkle"


def fetch():
    DEST.mkdir(parents=True, exist_ok=True)
    with (DEST / ".fetch.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        _fetch_locked()


def _fetch_locked():
    archive = DEST / f"Sparkle-{VERSION}.tar.xz"
    if not archive.exists():
        url = f"https://github.com/sparkle-project/Sparkle/releases/download/{VERSION}/{archive.name}"
        with urllib.request.urlopen(url, timeout=120) as response:
            data = response.read(64 * 1024 * 1024 + 1)
        if len(data) > 64 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != SHA256:
            raise ValueError("native framework download checksum mismatch")
        archive.write_bytes(data)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
        raise ValueError("cached native framework checksum mismatch")
    # Build from the verified archive: CI caches may retain only part of a tree.
    with tempfile.TemporaryDirectory(dir=DEST) as staging:
        unpacked = Path(staging)
        with tarfile.open(archive) as tar:
            tar.extractall(unpacked, filter="data")
        info = plistlib.loads((unpacked / "Sparkle.framework/Resources/Info.plist").read_bytes())
        if info["CFBundleShortVersionString"] != VERSION:
            raise ValueError("native framework version mismatch")
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(unpacked / "Sparkle.framework")], check=True)
        for source in unpacked.iterdir():
            destination = DEST / source.name
            if destination.is_symlink() or destination.is_file():
                destination.unlink()
            elif destination.exists():
                shutil.rmtree(destination)
            source.rename(destination)


def bundle(app):
    framework = app / "Contents/Frameworks/Sparkle.framework"
    framework.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(DEST / "Sparkle.framework", framework, symlinks=True, dirs_exist_ok=True)
    shutil.copy2(DEST / "LICENSE", app / "Contents/Resources/Sparkle-LICENSE.txt")
    path = app / "Contents/Info.plist"
    info = plistlib.loads(path.read_bytes())
    info.update(SUFeedURL="https://misfitdev.github.io/daisy/updates/appcast.xml",
                SUPublicEDKey=(ROOT / "macos/sparkle-public-key.txt").read_text().strip(),
                SUEnableAutomaticChecks=False, SURequireSignedFeed=True,
                SUVerifyUpdateBeforeExtraction=True)
    path.write_bytes(plistlib.dumps(info))


def sign(app, identity):
    framework = app / "Contents/Frameworks/Sparkle.framework"
    version = framework / "Versions/B"
    targets = sorted((version / "XPCServices").glob("*.xpc"))
    targets += [version / "Autoupdate", version / "Updater.app", framework]
    for target in targets:
        command = ["codesign", "--force", "--options", "runtime", "--preserve-metadata=entitlements", "--sign", identity]
        if identity != "-":
            command += ["--timestamp"]
        subprocess.run(command + [str(target)], check=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["fetch", "bundle", "sign"])
    parser.add_argument("--app", type=Path)
    parser.add_argument("--identity")
    args = parser.parse_args()
    if args.action == "fetch":
        fetch()
    elif args.action == "bundle":
        bundle(args.app)
    else:
        sign(args.app, args.identity)
