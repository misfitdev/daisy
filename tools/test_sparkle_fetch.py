"""Exercise framework bootstrap with fresh and incomplete caches."""

import hashlib
import multiprocessing
from pathlib import Path
import plistlib
import queue
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import sparkle


def fetch_worker(destination, checksum, index, started, release, results):
    sparkle.DEST = destination
    sparkle.SHA256 = checksum

    def verify(*_args, **_kwargs):
        results.put(("verify", index))
        if index == 1 and not release.wait(10):
            raise TimeoutError("first bootstrap was not released")

    started.set()
    try:
        with patch.object(sparkle.subprocess, "run", side_effect=verify):
            sparkle.fetch()
        results.put(("done", index))
    except Exception as error:
        results.put(("error", index, str(error)))


class FetchTests(unittest.TestCase):
    def test_concurrent_bootstraps_wait_for_the_active_writer(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory)
            source = destination / "fixture/Sparkle.framework/Resources"
            source.mkdir(parents=True)
            (source / "Info.plist").write_bytes(plistlib.dumps({
                "CFBundleShortVersionString": sparkle.VERSION,
            }))
            archive = destination / f"Sparkle-{sparkle.VERSION}.tar.xz"
            with tarfile.open(archive, "w:xz") as tar:
                tar.add(source.parent, arcname="Sparkle.framework")
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            context = multiprocessing.get_context("spawn")
            release = context.Event()
            results = context.Queue()
            started = [context.Event(), context.Event()]
            workers = [context.Process(target=fetch_worker, args=(
                destination, checksum, index + 1, started[index], release, results,
            )) for index in range(2)]
            try:
                workers[0].start()
                self.assertEqual(results.get(timeout=10), ("verify", 1))
                workers[1].start()
                self.assertTrue(started[1].wait(10))
                with self.assertRaises(queue.Empty):
                    results.get(timeout=1)
                release.set()
                remaining = [results.get(timeout=10) for _ in range(3)]
                self.assertCountEqual(remaining, [("done", 1), ("verify", 2), ("done", 2)])
            finally:
                release.set()
                for worker in workers:
                    if worker.pid is not None:
                        worker.join(10)
                        if worker.is_alive():
                            worker.terminate()
                            worker.join()
                results.close()
                results.join_thread()
            self.assertTrue((destination / "Sparkle.framework/Resources/Info.plist").is_file())

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
