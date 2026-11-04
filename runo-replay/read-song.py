#!/usr/bin/env python3
"""Read a Runo chorus directory's song.jsonl and check its chains. Python 3, stdlib only.

The milestone-2 gate (technical spec §14): a chain written through the store service is
readable by the replay kit. This script is that reader. It does not verify signatures or sums
(that needs blake3 and canonical CBOR; Runo's ingress does it); it checks what a reader can see
on its own:

  - every line is {"class": "current" | "history" | "control", "verse": {...}}
  - per chain (singer, task, epoch), seq runs 0, 1, 2, ... and each verse's prev is the sum of
    the verse before it
  - two verses at one (singer, task, epoch, seq) are a fork: listed, not an error

    python3 read-song.py <chorus-dir> [--csv chains.csv]

Exit 0 when every chain is continuous, 1 otherwise, 2 if the file cannot be read.
"""

import argparse, csv, json, os, sys
from collections import Counter, defaultdict

CLASSES = {"current", "history", "control"}


def b3(h):
    """A hash as the specs print it: b3:<hex>. On the wire it is a list of 32 byte values."""
    return "b3:" + bytes(h).hex() if h is not None else ""


def load(path):
    records = []
    with open(path, encoding="utf-8") as f:
        for n, line in enumerate(f, 1):
            if not line.strip():
                continue
            rec = json.loads(line)
            if set(rec) != {"class", "verse"} or rec["class"] not in CLASSES:
                raise ValueError(f"line {n}: not a song record: {sorted(rec)}")
            records.append(rec)
    return records


def check(records):
    chains = defaultdict(dict)  # (singer, task, epoch) -> seq -> [verse, ...]
    for rec in records:
        h = rec["verse"]["header"]
        key = (h["singer"], h["task"], h["epoch"])
        chains[key].setdefault(h["seq"], []).append(rec)
    rows, problems, forks = [], [], []
    for (singer, task, epoch), seqs in sorted(chains.items()):
        expected_prev, broken = None, False
        for seq in range(max(seqs) + 1):
            if seq not in seqs:
                problems.append(f"{singer} {task} epoch {epoch}: seq {seq} missing")
                broken = True
                break
            variants = seqs[seq]
            if len(variants) > 1:
                forks.append(f"{singer} {task} epoch {epoch} seq {seq}: "
                             + ", ".join(b3(v["verse"]["seal"]["sum"])[:15] for v in variants))
            prevs = {b3(v["verse"]["header"]["prev"]) for v in variants}
            if expected_prev is not None and expected_prev not in prevs:
                problems.append(f"{singer} {task} epoch {epoch}: seq {seq} prev does not follow")
                broken = True
                break
            if expected_prev is None and prevs != {""}:
                problems.append(f"{singer} {task} epoch {epoch}: seq 0 has a prev")
                broken = True
                break
            expected_prev = b3(variants[0]["verse"]["seal"]["sum"])
        classes = Counter(r["class"] for vs in seqs.values() for r in vs)
        kinds = Counter(r["verse"]["payload"]["kind"] for vs in seqs.values() for r in vs)
        rows.append({
            "singer": singer, "task": task, "epoch": epoch,
            "verses": sum(classes.values()),
            "current": classes["current"], "history": classes["history"],
            "control": classes["control"],
            "kinds": " ".join(f"{k}:{v}" for k, v in sorted(kinds.items())),
            "continuous": not broken,
        })
    return rows, problems, forks


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("chorus", help="the chorus directory (holds song.jsonl)")
    ap.add_argument("--csv", help="also write one row per chain here")
    a = ap.parse_args()
    path = os.path.join(a.chorus, "song.jsonl")
    try:
        records = load(path)
    except (OSError, ValueError, json.JSONDecodeError) as e:
        print(f"read-song: {path}: {e}", file=sys.stderr)
        return 2
    rows, problems, forks = check(records)
    print(f"{len(records)} records in {len(rows)} chains, "
          f"{len({r['task'] for r in rows})} tasks, {len({r['singer'] for r in rows})} singers")
    for r in rows:
        mark = "ok " if r["continuous"] else "BAD"
        print(f"  {mark} {r['singer']:<12} {r['task']:<12} epoch {r['epoch']}  "
              f"{r['verses']} verses (current {r['current']}, history {r['history']}, "
              f"control {r['control']})  {r['kinds']}")
    for f_ in forks:
        print(f"  fork: {f_}")
    for p in problems:
        print(f"  problem: {p}")
    if a.csv:
        with open(a.csv, "w", newline="") as f:
            w = csv.DictWriter(f, fieldnames=list(rows[0]) if rows else ["singer"])
            w.writeheader()
            w.writerows(rows)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
