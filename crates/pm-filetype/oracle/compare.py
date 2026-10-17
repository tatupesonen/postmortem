#!/usr/bin/env python3
"""Compares tika.tsv (real Tika) with rust.tsv (pm-filetype).

    python3 compare.py tika.tsv rust.tsv [--show N]

Prints agreement for pass 1 (no name), pass 2 (with name) and Autopsy's
combined answer (pass 2 only when pass 1 is not octet-stream, `tika-` and
parameters removed), then the most common differences.
"""
import collections
import sys

def load(p):
    d = {}
    with open(p, encoding='utf-8', errors='replace') as f:
        for line in f:
            parts = line.rstrip('\n').split('\t')
            if len(parts) == 3:
                d[parts[0]] = (parts[1], parts[2])
    return d

def autopsy(p1, p2):
    def clean(t):
        t = t.replace('tika-', '')
        return t.split(';')[0].strip()
    if p1 == 'ERROR':
        return 'application/octet-stream'
    a = clean(p1)
    if a == 'application/octet-stream':
        return a
    return clean(p2) if p2 != 'ERROR' else 'application/octet-stream'

def main():
    t = load(sys.argv[1])
    r = load(sys.argv[2])
    show = int(sys.argv[4]) if len(sys.argv) > 4 else 40
    paths = [p for p in t if p in r]
    same = [0, 0, 0]
    diffs = collections.Counter()
    examples = {}
    for p in paths:
        (t1, t2), (r1, r2) = t[p], r[p]
        a, b = autopsy(t1, t2), autopsy(r1, r2)
        same[0] += t1 == r1
        same[1] += t2 == r2
        same[2] += a == b
        if a != b:
            k = (a, b)
            diffs[k] += 1
            examples.setdefault(k, p)
    n = len(paths)
    print(f"files: {n}")
    for label, s in zip(["pass 1", "pass 2", "autopsy"], same):
        print(f"{label}: {s}/{n} identical ({100.0 * s / max(n, 1):.2f} %)")
    print("\nmost common Autopsy-level differences (tika -> rust):")
    for (k, c) in diffs.most_common(show):
        print(f"{c:5d}  {k[0]}  ->  {k[1]}   e.g. {examples[k]}")

if __name__ == '__main__':
    main()
