"""Exercise protocol compatibility across releases in the signed feed."""
import hashlib
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import sparkle_appcast as feed


class Appcast(unittest.TestCase):
    def test_a_protocol_retirement_never_enters_an_older_routine_channel(self):
        root = ET.fromstring('<rss version="2.0"><channel><title>Daisy Updates</title></channel></rss>')
        def release(version, protocols):
            feed.add_release(root, dict(version=version, protocol=max(protocols), supported_protocols=protocols), 42, 'signature')
        release('0.10.0', [6, 7])
        release('0.11.0', [7, 8])
        release('1.0.0', [6, 7, 8])
        def versions(channel):
            return {item.findtext(f'{{{feed.NS}}}version') for item in root.findall('channel/item')
                    if item.findtext(f'{{{feed.NS}}}channel') == channel}
        self.assertEqual(versions('major-0-protocols-6-7'), {'0.10.0'})
        self.assertEqual(versions('major-0-protocols-7'), {'0.10.0', '0.11.0'})
        self.assertEqual(versions('stable'), {'0.10.0', '0.11.0', '1.0.0'})
        release('0.10.0', [6, 7])
        self.assertEqual(len([item for item in root.findall('channel/item')
                              if item.findtext(f'{{{feed.NS}}}version') == '0.10.0']), 4)

    def test_changed_archive_is_rejected_before_signing_or_feed_download(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'Daisy-0.10.0-macos-arm64.zip'
            archive.write_bytes(b'changed')
            manifest = root / 'metadata.toml'
            manifest.write_text('format=2\nversion="0.10.0"\narchive_sha256="' + hashlib.sha256(b'original').hexdigest() + '"\n')
            with patch('sparkle_appcast.subprocess.check_output') as signer, patch('sparkle_appcast.previous_feed') as old_feed:
                with self.assertRaisesRegex(ValueError, 'checksum'):
                    feed.generate(archive, manifest, root/'appcast.xml')
                signer.assert_not_called()
                old_feed.assert_not_called()


if __name__ == '__main__':
    unittest.main()
