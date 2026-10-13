"""Generate and sign the release feed, retaining authenticated older entries."""
import copy
from datetime import datetime, timezone
from email.utils import format_datetime
import hashlib
import html
from itertools import combinations
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import tomllib
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
SIGNER = ROOT / "target/sparkle/bin/sign_update"
NS = "http://www.andymatuschak.org/xml-namespaces/sparkle"
ET.register_namespace("sparkle", NS)
FEED = "https://misfitdev.github.io/daisy/updates/appcast.xml"


def signing_args():
    key = os.environ.get("DAISY_SPARKLE_KEY_FILE")
    return ["--ed-key-file", key] if key else ["--account", "dev.misfit.daisy.sparkle"]


# Sparkle shows the description in a web view; without a style it uses a
# large serif default. Matches the system's dialog text in light and dark.
NOTES_STYLE = (
    "<style>:root{color-scheme:light dark}"
    "body{font:13px -apple-system,system-ui,sans-serif;margin:4px 2px;line-height:1.4}"
    "h3{font-size:13px;font-weight:600;margin:10px 0 2px}h3:first-child{margin-top:0}"
    "ul{margin:0;padding-left:20px}li{margin:1px 0}</style>"
)


def release_notes_html(notes, protocols):
    """The update dialog's text: the changelog git-cliff renders for this
    release, then the protocol versions it can talk to."""
    parts, items = [NOTES_STYLE], []

    def close_list():
        if items:
            parts.append("<ul>" + "".join(f"<li>{item}</li>" for item in items) + "</ul>")
            items.clear()

    for line in notes.splitlines():
        line = line.strip()
        if line.startswith("### "):
            close_list()
            parts.append(f"<h3>{html.escape(line[4:])}</h3>")
        elif line.startswith("- "):
            items.append(html.escape(line[2:]))
    close_list()
    if len(parts) == 1:
        parts.append("<p>Maintenance changes only.</p>")
    parts.append("<h3>Supported protocol versions</h3>")
    parts.append("<ul>" + "".join(f"<li>{protocol}</li>" for protocol in protocols) + "</ul>")
    return "".join(parts)


def add_release(root, manifest, size, signature, notes=""):
    version = manifest["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError("feed requires a stable release version")
    protocols = sorted(set(manifest["supported_protocols"]))
    if not protocols or len(protocols) != len(manifest["supported_protocols"]) or manifest["protocol"] not in protocols:
        raise ValueError("invalid release protocol metadata")
    channel = root.find("channel")
    if channel is None:
        raise ValueError("feed has no channel")
    # Rebuilding the same release is idempotent, without altering previous versions.
    for item in list(channel.findall("item")):
        if item.findtext(f"{{{NS}}}version") == version:
            channel.remove(item)
    channels = ["stable"]
    for count in range(1, len(protocols) + 1):
        for subset in combinations(protocols, count):
            channels.append(f"major-{version.split('.')[0]}-protocols-" + "-".join(map(str, subset)))
    for name in channels:
        item = ET.SubElement(channel, "item")
        ET.SubElement(item, "title").text = f"Daisy {version}"
        ET.SubElement(item, "pubDate").text = format_datetime(datetime.now(timezone.utc))
        ET.SubElement(item, f"{{{NS}}}version").text = version
        ET.SubElement(item, f"{{{NS}}}shortVersionString").text = version
        ET.SubElement(item, f"{{{NS}}}channel").text = name
        ET.SubElement(item, f"{{{NS}}}minimumSystemVersion").text = "26.0"
        ET.SubElement(item, "description").text = release_notes_html(notes, protocols)
        ET.SubElement(item, "enclosure", {
            "url": f"https://github.com/misfitdev/daisy/releases/download/v{version}/Daisy-{version}-macos-arm64.zip",
            "length": str(size), "type": "application/octet-stream",
            f"{{{NS}}}edSignature": signature,
        })
    return root


def previous_feed():
    try:
        with urllib.request.urlopen(FEED, timeout=30) as response:
            data = response.read(4 * 1024 * 1024 + 1)
        if len(data) > 4 * 1024 * 1024:
            raise ValueError("feed exceeds size limit")
    except urllib.error.HTTPError as error:
        if error.code != 404:
            raise
        root = ET.Element("rss", version="2.0")
        channel = ET.SubElement(root, "channel")
        ET.SubElement(channel, "title").text = "Daisy Updates"
        ET.SubElement(channel, "link").text = "https://misfitdev.github.io/daisy/"
        return root
    with tempfile.TemporaryDirectory(prefix="daisy-feed-") as directory:
        path = Path(directory) / "appcast.xml"
        path.write_bytes(data)
        subprocess.run([str(SIGNER), *signing_args(), "--verify", str(path)], check=True)
    return ET.fromstring(data)


def generate(archive, manifest_path, output, notes_path=None):
    manifest = tomllib.loads(manifest_path.read_text())
    if manifest["format"] != 2 or archive.name != f"Daisy-{manifest['version']}-macos-arm64.zip":
        raise ValueError("release archive does not match metadata")
    data = archive.read_bytes()
    if hashlib.sha256(data).hexdigest() != manifest["archive_sha256"]:
        raise ValueError("release archive checksum does not match metadata")
    signature = subprocess.check_output([str(SIGNER), *signing_args(), "-p", str(archive)], text=True).strip()
    notes = notes_path.read_text() if notes_path else ""
    root = add_release(previous_feed(), manifest, len(data), signature, notes)
    ET.indent(root)
    ET.ElementTree(root).write(output, encoding="utf-8", xml_declaration=True)
    subprocess.run([str(SIGNER), *signing_args(), str(output)], check=True)


if __name__ == "__main__":
    if len(sys.argv) != 5:
        sys.exit("usage: sparkle_appcast.py ARCHIVE MANIFEST NOTES OUTPUT")
    archive, manifest, notes, output = (Path(arg) for arg in sys.argv[1:])
    generate(archive, manifest, output, notes)
