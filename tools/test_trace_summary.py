import json
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("trace_summary.py")


def event(message, time=1, sequence=0, **fields):
    return {
        "system": "local",
        "record": {
            "unix_ms": time,
            "sequence": sequence,
            "target": "daisy::share",
            "fields": {"message": message, **fields},
        },
    }


class TraceSummaryTests(unittest.TestCase):
    def test_recipe_default_path_is_expanded_before_shell_quoting(self):
        result = subprocess.run(
            ["just", "--justfile", str(SCRIPT.parent.parent / "justfile"), "--dry-run", "trace-summary"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            shlex.split(result.stdout + result.stderr),
            ["python3", "tools/trace_summary.py", str(Path.home() / "daisy-trace.ndjson")],
        )

    def run_summary(self, records, tail=""):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "capture with spaces.ndjson"
            path.write_text("".join(json.dumps(row) + "\n" for row in records) + tail)
            result = subprocess.run(
                [sys.executable, str(SCRIPT), str(path)], capture_output=True, text=True
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        counts, timeline = result.stdout.split("Timeline:\n")
        return (
            [json.loads(line) for line in counts.splitlines()[1:]],
            [json.loads(line) for line in timeline.splitlines()],
            result.stderr,
        )

    def test_sparse_delayed_input_keeps_queue_detail(self):
        record = event("delayed peer input", queue_wait_ms="123", kind="Motion", generation="7")
        counts, timeline, errors = self.run_summary([record])
        self.assertEqual(counts[0]["count"], 1)
        self.assertEqual(timeline, [record])
        self.assertEqual(errors, "")

    def test_mixed_events_are_counted_and_timeline_is_ordered(self):
        later = event("local capture decision", time=2, reason="edge")
        earlier = event("delayed peer input", sequence=1, queue_wait_ms="12")
        first = event("sharing transition")
        other = event("peer connected")
        counts, timeline, _ = self.run_summary([later, earlier, other, first, other])
        self.assertEqual(counts[0]["message"], "peer connected")
        self.assertEqual(counts[0]["count"], 2)
        self.assertEqual(timeline, [first, earlier, later])

    def test_empty_capture(self):
        self.assertEqual(self.run_summary([]), ([], [], ""))

    def test_partial_or_invalid_records_are_reported_without_echoing_content(self):
        record = event("delayed peer input", queue_wait_ms="20")
        _, timeline, errors = self.run_summary([{}, record], tail='{"private":"unfinished')
        self.assertEqual(timeline, [record])
        self.assertEqual(errors, "Skipped 2 invalid trace records.\n")


if __name__ == "__main__":
    unittest.main()
