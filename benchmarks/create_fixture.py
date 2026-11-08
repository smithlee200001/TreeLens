#!/usr/bin/env python3
"""Create 100,000 empty files, optionally one 1-TiB sparse file. Prints scan root."""
import argparse
from pathlib import Path
p = argparse.ArgumentParser()
p.add_argument('destination', type=Path)
p.add_argument('--sparse', action='store_true')
a = p.parse_args()
root = a.destination / ('long-prefix-' + 'x' * 85)
root.mkdir(parents=True, exist_ok=True)
for d in range(100):
    sub = root / f'dir-{d:03d}'
    sub.mkdir(exist_ok=True)
    for f in range(1000):
        (sub / f'file-{f:04d}.txt').touch()
if a.sparse:
    with (root / 'one-terabyte-sparse.bin').open('wb') as f:
        f.truncate(1 << 40)
print(root)
