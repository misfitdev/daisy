#!/usr/bin/env python3
"""Summarize developer traces without requiring every event to have the same fields."""

import argparse
from contextlib import closing
import json
import sqlite3
import sys
from pathlib import Path


TIMELINE = (
    "local capture decision",
    "sharing transition",
    "injected motion resumed",
    "cursor strayed from injected moves",
    "local input backlog",
    "peer message backlog",
    "delayed peer input",
    "slow peer message handling",
    "slow round trip to the peer",
    "edge leave decision",
    "remote transition decision",
    "modifier replayed",
    "idle timer reset carries the function flag",
)


def summarize(path):
    skipped = 0
    # An on-disk temporary database keeps long captures out of memory and
    # orders records from different systems without changing the capture.
    with closing(sqlite3.connect("")) as database, path.open("rb") as source:
        database.execute(
            "CREATE TABLE events (time INTEGER, sequence INTEGER, "
            "target TEXT, message TEXT, payload TEXT)"
        )
        for line in source:
            try:
                collected = json.loads(line.decode("utf-8"))
                record = collected["record"]
                fields = record["fields"]
                time, sequence = record["unix_ms"], record["sequence"]
                if not isinstance(fields, dict) or any(
                    type(value) is not int or not 0 <= value < 2**63
                    for value in (time, sequence)
                ):
                    raise ValueError("invalid trace record")
                target, message = record["target"], fields.get("message", "")
                if not isinstance(target, str) or not isinstance(message, str):
                    raise ValueError("invalid event name")
            except (KeyError, TypeError, ValueError, UnicodeDecodeError):
                skipped += 1
                continue
            database.execute(
                "INSERT INTO events VALUES (?, ?, ?, ?, ?)",
                (time, sequence, target, message, json.dumps(collected)),
            )
        print("Event counts:")
        for target, message, count in database.execute(
            "SELECT target, message, count(*) FROM events "
            "GROUP BY target, message ORDER BY count(*) DESC, target, message"
        ):
            print(json.dumps({"target": target, "message": message, "count": count}))
        print("Timeline:")
        placeholders = ",".join("?" for _ in TIMELINE)
        for (payload,) in database.execute(
            f"SELECT payload FROM events WHERE message IN ({placeholders}) "
            "ORDER BY time, sequence, rowid",
            TIMELINE,
        ):
            print(payload)
    if skipped:
        print(f"Skipped {skipped} invalid trace records.", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("file", type=Path)
    args = parser.parse_args()
    try:
        summarize(args.file)
    except (OSError, sqlite3.Error) as error:
        parser.error(f"could not summarize trace: {error}")


if __name__ == "__main__":
    main()
