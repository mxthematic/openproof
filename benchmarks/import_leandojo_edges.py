#!/usr/bin/env python3
"""
Import Mathlib dependency edges from LeanDojo Benchmark 4.
Downloads the dataset from HuggingFace, extracts premise usage from
tactic annotations, and bulk-inserts edges into corpus_edges.

Usage: python3 benchmarks/import_leandojo_edges.py
"""
import json
import os
import re
import sqlite3
import sys
from collections import defaultdict
from datetime import datetime, timezone

DB_PATH = os.path.expanduser("~/.openproof/native/openproof-native.sqlite")
CACHE_DIR = os.path.expanduser("~/.cache/leandojo")

def load_corpus_keys(conn):
    """Build label → identity_key lookup from existing corpus."""
    label_to_key = {}
    fullname_to_key = {}
    for row in conn.execute("SELECT label, identity_key FROM verified_corpus_items"):
        label, key = row
        # Prefer library-seed items over user-verified
        if label not in label_to_key or key.startswith("library-seed"):
            label_to_key[label] = key
        # Also index by reconstructed full name (e.g., "Algebra_Star_Prod/fst_star" -> "Prod.fst_star")
        # Extract the label part after the last /
        parts = key.split("/")
        if len(parts) >= 4:
            fullname_to_key[label] = key
    return label_to_key, fullname_to_key

def extract_premises_from_tactic(annotated_tactic):
    """Extract premise names from annotated tactic string with <a>name</a> tags."""
    return re.findall(r'<a>([\w.\']+)</a>', annotated_tactic)

def main():
    print("Loading corpus identity keys...", file=sys.stderr)
    conn = sqlite3.connect(DB_PATH)
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA busy_timeout=30000")
    label_to_key, _ = load_corpus_keys(conn)
    print(f"  {len(label_to_key)} unique labels", file=sys.stderr)

    # Try to load from HuggingFace datasets
    print("Loading LeanDojo Benchmark 4...", file=sys.stderr)
    try:
        from datasets import load_dataset
        ds = load_dataset("kaiyuy/LeanDojo_Benchmark_4", cache_dir=CACHE_DIR)
    except Exception as e:
        print(f"Failed to load dataset: {e}", file=sys.stderr)
        print("Trying direct download...", file=sys.stderr)
        # Fallback: try loading from local files
        ds = None

    if ds is None:
        print("Could not load LeanDojo dataset. Exiting.", file=sys.stderr)
        sys.exit(1)

    # The dataset has splits: train, val, test
    # Each entry has: url, commit, file_path, full_name, start, end, traced_tactics
    # traced_tactics contain annotated_tactic with <a>premise</a> tags

    now = datetime.now(timezone.utc).isoformat()
    batch = []
    total_edges = 0
    inserted = 0
    skipped = 0

    for split_name in ds.keys():
        split = ds[split_name]
        print(f"Processing split '{split_name}' ({len(split)} entries)...", file=sys.stderr)

        for entry in split:
            theorem_name = entry.get("full_name", "")
            if not theorem_name:
                continue

            # Get the theorem's label (last component)
            theorem_label = theorem_name.rsplit(".", 1)[-1] if "." in theorem_name else theorem_name
            theorem_key = label_to_key.get(theorem_label)
            if not theorem_key:
                skipped += 1
                continue

            # Extract premises from traced tactics
            traced_tactics = entry.get("traced_tactics", [])
            if not traced_tactics:
                continue

            premises_used = set()
            for tactic_entry in traced_tactics:
                annotated = tactic_entry.get("annotated_tactic", "")
                if annotated:
                    for premise_name in extract_premises_from_tactic(annotated):
                        premises_used.add(premise_name)

            for premise_name in premises_used:
                premise_label = premise_name.rsplit(".", 1)[-1] if "." in premise_name else premise_name
                premise_key = label_to_key.get(premise_label)
                if not premise_key or premise_key == theorem_key:
                    skipped += 1
                    continue

                total_edges += 1
                edge_id = f"edge_leandojo_{total_edges}"
                batch.append((edge_id, theorem_key, premise_key, "uses", 1.0, now))
                inserted += 1

                if len(batch) >= 5000:
                    conn.executemany(
                        "INSERT OR IGNORE INTO corpus_edges (id, from_item_key, to_item_key, edge_type, confidence, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                        batch
                    )
                    conn.commit()
                    print(f"  {inserted} inserted, {skipped} skipped...", file=sys.stderr)
                    batch = []

    # Final batch
    if batch:
        conn.executemany(
            "INSERT OR IGNORE INTO corpus_edges (id, from_item_key, to_item_key, edge_type, confidence, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            batch
        )
        conn.commit()

    conn.close()
    print(f"\nDone: {inserted} edges inserted, {skipped} skipped, {total_edges} total", file=sys.stderr)

if __name__ == "__main__":
    main()
