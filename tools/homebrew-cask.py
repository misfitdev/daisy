#!/usr/bin/env python3
"""Generate a pinned Homebrew cask from the packaged release DMG."""

import hashlib
from pathlib import Path
import re
import sys


def generate(dmg: Path) -> str:
    match = re.fullmatch(r"Daisy-(\d+\.\d+\.\d+)-macos-arm64\.dmg", dmg.name)
    if match is None:
        raise ValueError("expected Daisy-X.Y.Z-macos-arm64.dmg")
    version = match[1]
    checksum = hashlib.sha256(dmg.read_bytes()).hexdigest()
    return f'''cask "daisy" do
  version "{version}"
  sha256 "{checksum}"

  url "https://github.com/misfitdev/daisy/releases/download/v#{{version}}/Daisy-#{{version}}-macos-arm64.dmg"
  name "Daisy"
  desc "Share keyboard, pointer, trackpad swipes, and clipboard across systems"
  homepage "https://misfitdev.github.io/daisy/"

  livecheck do
    url "https://github.com/misfitdev/daisy/releases/latest"
    strategy :github_latest
  end

  depends_on arch: :arm64
  depends_on macos: :tahoe

  app "Daisy.app"
end
'''


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: homebrew-cask.py DMG OUTPUT")
    try:
        cask = generate(Path(sys.argv[1]))
        Path(sys.argv[2]).write_text(cask)
    except (OSError, ValueError) as error:
        sys.exit(str(error))
