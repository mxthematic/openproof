#!/usr/bin/env python3
"""
Bulk-import Mathlib dependency edges into the OpenProof corpus_edges table.
Reads JSONL from stdin (one {"from":"...","to":"..."} per line).
Only inserts edges where both from and to keys exist in verified_corpus_items.

Usage: cat edges.jsonl | python3 benchmarks/import_edges.py
"""
import json
import sqlite3
import sys
import os
from datetime import datetime, timezone

DB_PATH = os.path.expanduser("~/.openproof/native/openproof-native.sqlite")

def main():
    conn = sqlite3.connect(DB_PATH)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA busy_timeout=30000")

    # Build label -> identity_key lookup from corpus
    print("Loading corpus labels -> identity keys...", file=sys.stderr)
    label_to_key = {}
    for row in conn.execute("SELECT label, identity_key FROM verified_corpus_items"):
        label, key = row
        # If multiple items have the same label, prefer library-seed over user-verified
        if label not in label_to_key or key.startswith("library-seed"):
            label_to_key[label] = key
    print(f"  {len(label_to_key)} unique labels loaded", file=sys.stderr)

    now = datetime.now(timezone.utc).isoformat()
    batch = []
    total = 0
    inserted = 0
    skipped = 0

    print("Reading edges from stdin...", file=sys.stderr)
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            edge = json.loads(line)
        except json.JSONDecodeError:
            continue

        # Extract label (last component of the identity key path)
        from_raw = edge.get("from", "")
        to_raw = edge.get("to", "")
        from_label = from_raw.rsplit("/", 1)[-1] if "/" in from_raw else from_raw
        to_label = to_raw.rsplit("/", 1)[-1] if "/" in to_raw else to_raw
        total += 1

        # Resolve labels to actual identity keys in corpus
        from_key = label_to_key.get(from_label)
        to_key = label_to_key.get(to_label)

        if not from_key or not to_key:
            skipped += 1
            continue

        edge_id = f"edge_mathlib_{inserted}"
        batch.append((edge_id, from_key, to_key, "uses", 1.0, now))
        inserted += 1

        if len(batch) >= 5000:
            conn.executemany(
                "INSERT OR IGNORE INTO corpus_edges (id, from_item_key, to_item_key, edge_type, confidence, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                batch
            )
            conn.commit()
            print(f"  {inserted} inserted, {skipped} skipped of {total} total...", file=sys.stderr)
            batch = []

    # Final batch
    if batch:
        conn.executemany(
            "INSERT OR IGNORE INTO corpus_edges (id, from_item_key, to_item_key, edge_type, confidence, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            batch
        )
        conn.commit()

    conn.close()
    print(f"\nDone: {inserted} edges inserted, {skipped} skipped (missing endpoints), {total} total read", file=sys.stderr)

if __name__ == "__main__":
    main()
