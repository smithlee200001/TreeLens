# Memory and virtual-tree benchmark

Measured on Linux 6.18.57 x86_64, Rust 1.99.1, release builds (thin LTO,
1 codegen unit). These are synthetic headless-core measurements, not macOS
Activity Monitor measurements or a graphical rendering benchmark.

## Reproduce

From the project root, using a Rust >=1.88 toolchain:

```sh
ROOT=$(python3 benchmarks/create_fixture.py /tmp/tree-lens-fixture)
cargo build --release --no-default-features --example memory_profile
cargo build --release --manifest-path benchmarks/baseline/Cargo.toml --example memory_profile
for i in 1 2 3 4 5; do
  benchmarks/baseline/target/release/examples/memory_profile "$ROOT"
done
for i in 1 2 3 4 5; do
  target/release/examples/memory_profile "$ROOT"
done
```

The fixture is 100,000 empty regular files across 100 directories, under a
97-character final root component (118-byte full root path with the command
above). Both implementations scan the **same** paths. The baseline is a frozen
copy of the original scanner and tree model. Its separate Cargo manifest uses
only the standard library. The new example likewise adds no dependencies.
The optimized application still needs its own normal Cargo manifest resolved.

To test a large file without writing a terabyte of data, create a separate
single-file fixture and compare the two sizes:

```sh
mkdir -p /tmp/tree-lens-single-file
truncate -s 0 /tmp/tree-lens-single-file/large.bin
benchmarks/baseline/target/release/examples/memory_profile /tmp/tree-lens-single-file
target/release/examples/memory_profile /tmp/tree-lens-single-file
truncate -s 1T /tmp/tree-lens-single-file/large.bin
benchmarks/baseline/target/release/examples/memory_profile /tmp/tree-lens-single-file
target/release/examples/memory_profile /tmp/tree-lens-single-file
```

Alternatively, add `--sparse` to `create_fixture.py` to add a 1-TiB sparse file
to the 100,000-file fixture. An existing fixture is reused, so start with a
fresh destination when comparing fixture variants.

## What is counted

A `GlobalAlloc<System>` wrapper tracks successful Rust allocation requests,
subtracts deallocations, and records the maximum live requested bytes in each
phase. Scanner retained/peak bytes exclude command-line setup. View retained/
peak bytes exclude the scanner and the directory-only expansion `HashSet`;
that set's allocation is printed separately. New sorting is one-time work,
measured separately and performed before constructing the view.

The baseline materializes every visible `(NodeId, depth)` row. Its 50-row
viewport copy is reported for reference, but the old UI did not need that copy:
it already retained the entire row vector. The optimized view returns only
the requested 50 rows. End-scroll latency compares copying the last 50 cached rows on the baseline
with obtaining that range from the optimized view. The original UI reused
its cached row vector on scroll and could borrow rows without a copy; the
baseline copy is conservative overhead, not original rendering work. Full
`visible_rows` timing is rebuild work on expansion/filter/sort, not scrolling. First viewport and
end-scroll microsecond timings are noisy; compare trends, not individual runs.
Five process runs share filesystem caches. Raw results are preserved here.

These counters do **not** include allocator size classes/metadata, temporary
physical overlap during a system `realloc`, filesystem caches, C-library
allocations, stacks, executable pages, GUI/font/GPU storage, or RSS. They are
live requested Rust heap bytes, not an exact resident-memory measurement.
The scanner still retains one metadata node per filesystem entry; viewport
virtualization does not make the full scan result constant-space. Long paths
amplify the old path-per-node cost, so reduction varies by directory layout.

## Results

See the raw `baseline_results.txt` / `optimized_results.txt` and single-file
results. Summary values are the median of five runs; heap sizes were identical
across all five runs.

| Measurement | Original | Optimized |
|---|---:|---:|
| Scanner retained heap | 51,862,389.000 | 16,535,685.000 |
| Scanner peak heap | 51,863,843.000 | 20,997,459.000 |
| Expanded view retained heap | 2,097,152.000 | 10,384.000 |
| Expanded view peak heap | 2,230,837.000 | 14,560.000 |
| 50-row window heap | 800.000 | 1,024.000 |
| Scan time (ms) | 137.237 | 113.176 |
| View rebuild time (ms) | 7.073 | 1.235 |
| First 50 rows (μs) | 0.581 | 9.665 |
| Last 50 rows (μs) | 0.060 | 8.833 |

Heap values are bytes. Retained scanner heap decreased **68.12%** (49.46 MiB
to 15.77 MiB), peak scanner heap **59.51%** (49.46 MiB to 20.02 MiB), and
retained expanded-view heap **99.50%** (2.00 MiB to 10.14 KiB).
The 101-directory expansion set occupied 1,168 bytes in both cases, separately
from view retention. Sorting before the new view's first rebuild took a median
1.166 ms; add that to its rebuild time for the initial sorted-view operation.

The baseline cached slice is quicker to access. The optimized range lookup
trades several microseconds of CPU for dramatically lower retained view
storage. This is not evidence of faster scrolling or a GUI FPS result.

A separate root with one file used the same scanner heap at 0 bytes and 1 TiB
logical size in each implementation. Original: 876 retained / 1,020 peak bytes.

Optimized: 664 retained / 860 peak bytes. Both already scan metadata rather
than file contents; entry count, path layout, and expanded-view bookkeeping
are the relevant scaling factors, not a regular file's byte length.
