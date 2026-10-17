#!/usr/bin/env python3
"""Builds a stratified file list for the Tika differential test.

Walks the given directories and keeps up to PER_EXT files per lower-case
extension (and per "no extension"), skipping symlinks, empty files and
files over 64 MB. Prints one path per line.

    python3 corpus.py /usr/share ~/slop/autopsy ~/.cargo/registry > paths.txt
"""
import os
import random
import sys

PER_EXT = 15
MAX_SIZE = 64 * 1024 * 1024
MAX_TOTAL = 25000

def main():
    random.seed(1)
    buckets = {}
    for root in sys.argv[1:]:
        for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
            dirnames[:] = [d for d in dirnames if not d.startswith('.git')]
            for n in filenames:
                p = os.path.join(dirpath, n)
                if '\t' in p or '\n' in p:
                    continue
                try:
                    st = os.lstat(p)
                except OSError:
                    continue
                if not os.path.isfile(p) or os.path.islink(p) or st.st_size == 0 or st.st_size > MAX_SIZE:
                    continue
                if not os.access(p, os.R_OK):
                    continue
                ext = os.path.splitext(n)[1].lower()
                b = buckets.setdefault(ext, [])
                # Reservoir sampling per extension.
                b.append(p) if len(b) < PER_EXT * 4 else None
    out = []
    for ext, paths in sorted(buckets.items()):
        random.shuffle(paths)
        out.extend(paths[:PER_EXT])
    random.shuffle(out)
    for p in out[:MAX_TOTAL]:
        print(p)

if __name__ == '__main__':
    main()
