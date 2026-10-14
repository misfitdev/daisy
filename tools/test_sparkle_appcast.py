"""Exercise protocol compatibility across releases in the signed feed."""
import hashlib
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import sparkle_appcast as feed
from sparkle_fixtures import INSTALLED_VERSION, UPDATE_VERSION, MAJOR_UPDATE_VERSION, ROUTINE_CHANNEL


class Appcast(unittest.TestCase):
    def test_a_protocol_retirement_never_enters_an_older_routine_channel(self):
        root = ET.fromstring('<rss version="2.0"><channel><title>Daisy Updates</title></channel></rss>')
        def release(version, protocols):
            feed.add_release(root, dict(version=version, protocol=max(protocols), supported_protocols=protocols), 42, 'signature')
        release(INSTALLED_VERSION, [6, 7])
        release(UPDATE_VERSION, [7, 8])
        release(MAJOR_UPDATE_VERSION, [6, 7, 8])
        def versions(channel):
            return {item.findtext(f'{{{feed.NS}}}version') for item in root.findall('channel/item')
                    if item.findtext(f'{{{feed.NS}}}channel') == channel}
        self.assertEqual(versions(ROUTINE_CHANNEL), {INSTALLED_VERSION})
        self.assertEqual(versions(f"major-{INSTALLED_VERSION.split('.')[0]}-protocols-7"), {INSTALLED_VERSION, UPDATE_VERSION})
        self.assertEqual(versions('stable'), {INSTALLED_VERSION, UPDATE_VERSION, MAJOR_UPDATE_VERSION})
        release(INSTALLED_VERSION, [6, 7])
        self.assertEqual(len([item for item in root.findall('channel/item')
                              if item.findtext(f'{{{feed.NS}}}version') == INSTALLED_VERSION]), 4)

    def test_release_notes_list_changes_then_protocol_versions(self):
        notes = "## [1.2.3] - 2026-10-13\n\n### Bug Fixes\n\n- Stop <b>storms</b> & jumps\n- Keep peers busy\n"
        text = feed.release_notes_html(notes, [6, 7])
        self.assertIn("<h3>Bug Fixes</h3><ul><li>Stop &lt;b&gt;storms&lt;/b&gt; &amp; jumps</li><li>Keep peers busy</li></ul>", text)
        self.assertTrue(text.endswith("<h3>Supported protocol versions</h3><ul><li>6</li><li>7</li></ul>"))
        self.assertNotIn("1.2.3", text, "the dialog already names the version")
        self.assertIn("<style>", text)
        self.assertIn("Maintenance changes only.", feed.release_notes_html("## [1.2.4] - 2026-10-14\n", [7]))

    def test_scope_and_breaking_markers_render_as_html(self):
        text = feed.release_notes_html("### Bug Fixes\n\n- *(update)* [**breaking**] Restart <now>\n", [7])
        self.assertIn("<li><em>update:</em> <strong>Breaking:</strong> Restart &lt;now&gt;</li>", text)
        self.assertNotIn("*", text.split("</style>")[1])

    def test_each_feed_entry_carries_the_release_notes(self):
        root = ET.fromstring('<rss version="2.0"><channel><title>Daisy Updates</title></channel></rss>')
        feed.add_release(root, dict(version=INSTALLED_VERSION, protocol=7, supported_protocols=[6, 7]), 42, 'signature',
                         "### Features\n\n- Trace pointer jumps\n")
        descriptions = {item.findtext('description') for item in root.findall('channel/item')}
        self.assertEqual(len(descriptions), 1)
        self.assertIn("<li>Trace pointer jumps</li>", descriptions.pop())

    def test_changed_archive_is_rejected_before_signing_or_feed_download(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / f'Daisy-{INSTALLED_VERSION}-macos-arm64.zip'
            archive.write_bytes(b'changed')
            manifest = root / 'metadata.toml'
            manifest.write_text(f'format=2\nversion="{INSTALLED_VERSION}"\narchive_sha256="' + hashlib.sha256(b'original').hexdigest() + '"\n')
            with patch('sparkle_appcast.subprocess.check_output') as signer, patch('sparkle_appcast.previous_feed') as old_feed:
                with self.assertRaisesRegex(ValueError, 'checksum'):
                    feed.generate(archive, manifest, root/'appcast.xml')
                signer.assert_not_called()
                old_feed.assert_not_called()


if __name__ == '__main__':
    unittest.main()
