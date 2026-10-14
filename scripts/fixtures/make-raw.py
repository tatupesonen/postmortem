#!/usr/bin/env python3
"""Deterministic raw source image for container fixtures.

Layout (in 64 KiB blocks): zeros, ASCII text, pseudo-random (incompressible),
repeating pattern — exercises compressed, stored and sparse-ish chunks.
Usage: make-raw.py OUT SIZE_KIB
"""
import sys, random
out, kib = sys.argv[1], int(sys.argv[2])
rng = random.Random(0x5EED)
blocks = []
text = (b"postmortem fixture text block. " * 4096)[:65536]
for i in range(kib // 64):
    kind = i % 4
    if kind == 0:
        blocks.append(bytes(65536))
    elif kind == 1:
        blocks.append(text)
    elif kind == 2:
        blocks.append(rng.randbytes(65536))
    else:
        blocks.append(bytes((j * 7 + i) & 0xFF for j in range(65536)))
data = b"".join(blocks)
open(out, "wb").write(data)
