# Performance

Uppsala uses accelerated byte scanning for parser hot loops:

- SSE2 delimiter scanning on x86_64 for text content and attribute values.
- SSE2 ASCII XML-name continuation scanning for element and attribute names.
- SSE2 single-byte search for reference parsing.
- Scalar reference implementations on non-x86_64 and for SIMD tail bytes.

Parsing throughput depends heavily on document shape. Long plain-text spans,
large attribute values, and ASCII-heavy names favor the bulk scanners. Very
small documents are dominated by fixed parser and allocation overhead.

## Current libxml2 comparison

The tables below compare Uppsala against a local sibling checkout of libxml2,
called directly through `xmlReadMemory`. The harness reports median parse time
from in-memory strings; file I/O and process startup are not included.

Measured 2026-10-08 after ADR 0019 restored direct DOM construction (the
0.9.0 to 0.12.0 releases routed `Parser::parse` through pull events and were
roughly 2x slower on node-dense inputs; see the ADR for the before/after).

Build setup used for these numbers:

- Uppsala: `RUSTFLAGS='-C target-cpu=native' cargo build --release`
- libxml2: static release library from `../libxml2` (commit `c8eaf223`,
  2026-07-02), built with `-O3 -DNDEBUG -fno-semantic-interposition -march=native`
- CPU pinning: `taskset -c <core>`

The `Ratio` columns are `libxml2 / Uppsala`; values above `1.0` mean Uppsala
parsed faster. `Uppsala ns` is the stable namespace-aware `Parser::parse`,
`no-ns` disables namespace resolution, `Pull scan` drains the `PullParser`
event stream without a DOM, and `Pull DOM` is `document_from_pull` (the
explicit event-to-DOM path, kept for differential tests and expected to be
slower than `Parser::parse`).

### Server: AMD EPYC 7551 (4 vCPU VM, Debian 13), 301 samples

| Input | Size | Uppsala ns | Uppsala no-ns | Pull scan | Pull DOM | libxml2 | Ratio ns | Ratio no-ns |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SAML small | 3.4 KB | 17.2 us | 16.1 us | 20.0 us | 24.9 us | 55.2 us | 3.21x | 3.43x |
| SAML medium | 8.9 KB | 39.3 us | 35.9 us | 42.7 us | 55.6 us | 119.0 us | 3.03x | 3.32x |
| SAML large | 27.2 KB | 113.1 us | 104.4 us | 123.9 us | 159.4 us | 341.6 us | 3.02x | 3.27x |
| SAML metadata aggregate | 666.3 KB | 3.985 ms | 3.666 ms | 4.117 ms | 5.439 ms | 8.999 ms | 2.26x | 2.45x |
| Atom feed archive | 848.2 KB | 6.808 ms | 6.936 ms | 6.400 ms | 9.517 ms | 13.431 ms | 1.97x | 1.94x |
| SOAP invoice batch | 715.2 KB | 5.870 ms | 5.564 ms | 6.573 ms | 9.560 ms | 11.734 ms | 2.00x | 2.11x |
| pyFF sample metadata | 3.5 KB | 23.5 us | 17.9 us | 21.3 us | 33.8 us | 66.7 us | 2.84x | 3.73x |
| libxml2 `nvdcve_0.xml` | 287.4 KB | 2.705 ms | 2.706 ms | 2.465 ms | 3.392 ms | 4.905 ms | 1.81x | 1.81x |
| libxml2 `comps_0.xml` | 607.9 KB | 5.472 ms | 5.784 ms | 5.117 ms | 8.796 ms | 10.489 ms | 1.92x | 1.81x |

This VM is shared; timings for inputs under 30 KB vary by up to 2x between
runs (libxml2 included), so the large rows are the reliable ones.

### Laptop: Intel Core Ultra 7 155H, 101 samples

| Input | Size | Uppsala ns | Uppsala no-ns | Pull scan | Pull DOM | libxml2 | Ratio ns | Ratio no-ns |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SAML small | 3.4 KB | 17.0 us | 15.3 us | 18.6 us | 26.5 us | 64.0 us | 3.77x | 4.19x |
| SAML medium | 8.9 KB | 39.1 us | 35.6 us | 40.5 us | 62.6 us | 133.5 us | 3.42x | 3.75x |
| SAML large | 27.2 KB | 121.1 us | 103.5 us | 116.5 us | 181.3 us | 350.0 us | 2.89x | 3.38x |
| SAML metadata aggregate | 666.3 KB | 3.601 ms | 3.439 ms | 3.779 ms | 5.673 ms | 8.502 ms | 2.36x | 2.47x |
| Atom feed archive | 848.2 KB | 6.238 ms | 6.108 ms | 7.323 ms | 12.288 ms | 11.818 ms | 1.89x | 1.93x |
| SOAP invoice batch | 715.2 KB | 6.197 ms | 5.983 ms | 6.905 ms | 11.205 ms | 10.608 ms | 1.71x | 1.77x |
| pyFF sample metadata | 3.5 KB | 23.4 us | 21.8 us | 25.3 us | 38.2 us | 73.4 us | 3.14x | 3.37x |
| libxml2 `nvdcve_0.xml` | 287.4 KB | 2.778 ms | 2.739 ms | 2.555 ms | 4.040 ms | 5.024 ms | 1.81x | 1.83x |
| libxml2 `comps_0.xml` | 607.9 KB | 5.861 ms | 5.802 ms | 6.623 ms | 11.422 ms | 10.660 ms | 1.82x | 1.84x |

Real SAML files on the same laptop (1001 samples; eduGAIN 5 samples):

| Input | Size | Uppsala ns | libxml2 | Ratio |
|---|---:|---:|---:|---:|
| saml-authn-request.xml | 3.7 KB | 10.1 us | 35.4 us | 3.50x |
| saml-metadata.xml | 5.2 KB | 14.8 us | 48.2 us | 3.26x |
| saml-response.xml | 11.5 KB | 38.4 us | 126.0 us | 3.28x |
| eduGAIN aggregate | 91.9 MB | 711 ms | 1060 ms | 1.49x |

These are local measurements, not a universal claim. Re-run the harness on the
target server class before making capacity decisions, and re-run it after any
change to the parse path (ADR 0019 records a 2x regression that went unnoticed
for three months because the table was not re-measured).

## Running the performance harness

The comparison harness is checked into the repo under `performance-harness/`.
It is kept outside the main crate's targets so normal library builds and tests
do not depend on libxml2.

### One-command libxml2 benchmark

With libxml2 checked out at the default `../libxml2` location, run:

```bash
just bench-libxml2
```

This configures/builds libxml2 as a local static release library, builds the
Uppsala harness with `RUSTFLAGS='-C target-cpu=native'`, pins the run to CPU 0
when `taskset` is available, and prints one final Markdown table containing
the SAML-shaped inputs, larger generated XML shapes, and representative
local/libxml2 XML fixtures. The report includes namespace-aware DOM,
namespace-disabled DOM, pull scan-only, and explicit pull-to-DOM timings.

The default sample count is `301`. Use a larger count for steadier medians:

```bash
just bench-libxml2 1001
```

To use a different libxml2 checkout:

```bash
LIBXML2_DIR=/path/to/libxml2 just bench-libxml2
```

### Manual setup

By default, the harness expects a sibling libxml2 checkout next to the Uppsala
repo:

```text
code/
  uppsala/
    performance-harness/
  libxml2/
```

Build libxml2 locally:

```bash
cmake -S ../libxml2 -B ../libxml2/build-uppsala-release -G Ninja \
  -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_SHARED_LIBS=OFF \
  -DLIBXML2_WITH_PROGRAMS=OFF \
  -DLIBXML2_WITH_TESTS=OFF \
  -DLIBXML2_WITH_ZLIB=OFF \
  -DLIBXML2_WITH_ICONV=OFF \
  -DLIBXML2_WITH_ICU=OFF \
  -DLIBXML2_WITH_MODULES=OFF \
  -DLIBXML2_WITH_PYTHON=OFF \
  -DCMAKE_C_FLAGS_RELEASE='-O3 -DNDEBUG -fno-semantic-interposition -march=native'
cmake --build ../libxml2/build-uppsala-release
```

The harness build script links `$LIBXML2_DIR/build-uppsala-release/libxml2.a`,
defaulting `LIBXML2_DIR` to `../libxml2`. Override the source checkout with
`LIBXML2_DIR=/path/to/libxml2`; override only the build-output directory with
`LIBXML2_LIB_DIR=/path/to/build` if needed.

### Run SAML-shaped inputs manually

```bash
RUSTFLAGS='-C target-cpu=native' cargo run --release \
  --manifest-path performance-harness/Cargo.toml -- saml 1001
```

### Run a single file manually

```bash
RUSTFLAGS='-C target-cpu=native' cargo run --release \
  --manifest-path performance-harness/Cargo.toml -- \
  file test-data/pyff-xslt/sample-metadata.xml 1001
```

The trailing number is the sample count (default `31`). Each run does a short
warmup, then reports medians in microseconds. Output is tab-separated:

```text
file  bytes  uppsala_ns_us  uppsala_no_ns_us  uppsala_pull_scan_us  uppsala_pull_dom_us  libxml2_us  ratio_ns  ratio_no_ns  ratio_pull_scan  ratio_pull_dom
```

The `ratio_ns` column corresponds to the default namespace-aware mode; SAML
users should usually care about this column. `ratio_no_ns` is Uppsala with
namespace resolution disabled. `ratio_pull_scan` is the direct pull event stream
without DOM allocation, and `ratio_pull_dom` is the explicit pull-to-DOM builder.

## Profiling

To find hot functions when optimizing the parser, build the harness and record
a profile on Linux:

```bash
RUSTFLAGS='-C target-cpu=native' cargo build --release \
  --manifest-path performance-harness/Cargo.toml
sudo perf record -g --call-graph dwarf \
  performance-harness/target/release/uppsala-performance-harness saml 1001
sudo perf report --no-children --sort=dso,symbol
```

Focus on self-time (`--no-children`) to identify the real bottleneck rather
than its callers, and use `perf annotate --symbol=<fn>` to inspect a hot loop's
per-instruction sample counts.

On this container, hardware counters such as `branch-misses` and `cycles` were
reported as unsupported even under `sudo perf stat`; use a host with PMU access
for branch-prediction measurements.

---

## Playbook pass: 2026-10-07 (traversal, XPath compile-once, fast hashing)

Four experiments from the same performance playbook used on bergshamra
(allocation hygiene, algorithmic wins, string/hash choices, build profiles),
each validated against the full test suite (738 tests, 0 failed — including
the 68-test XML conformance, 66-test XPath conformance, and serialization
conformance suites) and measured with the new criterion harness
(`benches/uppsala.rs`, baselines `u0full` = original, `e4final` = final).

### Changes

1. **E1 — zero-alloc traversal** (01/08): converted hot internal `.children()`
   (Vec-per-call) sites to the existing `children_iter`; `descendants()` is now
   an iterative walk instead of recursion with a `Vec` per node; XPath
   `collect_descendants` uses a depth-first stack over reversed `children_iter`
   (no per-node Vec + reverse), preserving document order before predicates;
   `Child` axis pre-sizes its result via the new `children_count()`;
   `ChildrenIter` supports mixed forward/backward traversal with shared exhaustion.
2. **E2 — XPath compile-once** (08): `XPathEvaluator` caches the parsed AST
   keyed by expression text (`Mutex`-protected map of `Arc<Expr>`). Admission
   is limited to 128 entries, 4 KiB of source per entry, 64 KiB of owned key/AST
   capacity per entry, and 1 MiB of that capacity in total, plus bounded
   hash-table and allocator overhead. Only successful evaluations are admitted;
   larger expressions still evaluate without caching. `clear_cache()` releases
   cached programs, and changing `with_max_depth()` clears them automatically.
   These bounds cover retained cache data, not total parsing/evaluation memory;
   applications should still bound input sizes and concurrent work.
   Repeated evaluation of the same expression through one
   evaluator — the pyuppsala `XPath` pattern — skips tokenize+parse entirely.
   Measured: cheap repeated expressions **~6.5x faster** (671 -> 103 ns for
   `1 + 1`; tokenize+parse of even a 5-char expression costs ~3.2 us).
3. **E3 — FxHash for internal maps** (05): vendored `src/fasthash.rs`
   (`FastHashMap`/`FastHashSet`) — keeps the zero-dependency property that
   adding `rustc-hash` would break. Applied to `Document.attribute_nodes`
   (hot on XPath attribute axis and `prepare_xpath`) and internal NodeId sets.
   Entity maps and namespace-prefix maps/sets retain randomized standard-library
   hashing because their string keys come from XML input. The `fasthash` module
   is private and is not part of the supported public API.
4. **E4 — build profiles** (09): `[profile.release]` (fat LTO, codegen-units 1,
   strip), `[profile.dev] debug = "line-tables-only"`, and a
   `[profile.profiling]` for flamegraph sessions. Note: consumers building
   uppsala as a dependency govern their own profile; copy the release shape
   (LTO + codegen-units) if you want the same code in your binary.

### Results (criterion medians, full before/after, same machine)

These measurements predate the security-review corrections to traversal,
cache limits, and string hashing above; they have not been remeasured. The
preparation benchmark now times only `prepare_xpath()` on a fresh, unprepared
document per iteration, with parsing and destruction outside the timed routine.
Its historical results below included those costs and are not directly
comparable with the corrected benchmark.

| Benchmark | before | after | Delta |
|---|---:|---:|---:|
| traverse/descendants | 16.8 us | 4.3 us | **-74%** |
| xpath/repeat_cheap (cached eval) | 671 ns | 110 ns | **-84%** |
| xpath/repeat_position | 103.9 us | 81.2 us | **-22%** |
| prepare/attribute_nodes (attr_heavy) | 1155 us | 920 us | **-20%** |
| serialize/saml | 192 us | 158 us | -18% |
| pull/to_dom_saml | 289 us | 233 us | -19% |
| xpath/oneshot_axis | 70.7 us | 61.6 us | -13% |
| xpath/repeat_axis | 86.7 us | 77.2 us | -11% |
| traverse/children_root | 41.4 ns | 32.2 ns | -22% |
| pull/scan_saml | 274 us | 258 us | -6% |
| parse/* (parser untouched) | — | — | noise |

Reproduce:

```sh
cargo bench --bench uppsala -- --baseline u0full   # if the baseline exists
cargo bench --bench uppsala                        # absolute numbers
```

## Third pass: 2026-10-08 (`//` evaluation and serializer buffers)

Driven by the pyuppsala-vs-lxml comparison (`pyuppsala/PERFORMANCE.md`,
2026-10-08) on the 7.1 MB SWAMID aggregate (44,823 nodes). Laptop (Intel Core
Ultra 7 155H), evaluator timed through the binding, medians of 7. Full
rationale and profile in ADR 0020.

| Expression | Before | After | libxml2 (lxml) |
|---|---:|---:|---:|
| `//md:EntityDescriptor/md:IDPSSODescriptor` | 23.7 ms | 5.3 ms | 8.7 ms |
| `//md:EntityDescriptor/@entityID` (pyuppsala `xpath_ns` bench row) | 22.8 ms | 5.4 ms | 6.9 ms |
| `count(//*)` | 24.6 ms | 3.5 ms | - |
| `/md:EntitiesDescriptor/md:EntityDescriptor/md:IDPSSODescriptor` | 1.6 ms | 0.9 ms | - |

Changes:

- `//child::T` and `//attribute::T` without predicates run as one pre-order
  pointer walk that tests nodes in place (`apply_descendant_test`), instead of
  materializing every node and re-scanning each node's children.
- Node tests are resolved once per step (`ResolvedTest`): no per-candidate
  `HashMap` lookup of the namespace prefix.
- `descendant::`/`descendant-or-self::` test in place during the walk.
- Serializer buffers are pre-sized from the node's source range, and
  `write_node_to_with_options` lets callers serialize fragments into a reused
  buffer. Whole-document `node_to_xml` of the aggregate was dominated by page
  faults on the fresh 7 MB buffer and its doubling reallocations.

## Second pass: 2026-10-07 (XPath result construction and preparation)

Baseline: `fix/perf` at `1751d355`, including the review corrections above.
After: the uncommitted second-pass changes to `src/xpath.rs` and `src/dom.rs`.
The new `xpath_shapes` benchmarks were also run against the unchanged baseline
library before implementation. These results do not replace the historical
libxml2 comparison; no cross-library comparison was run in this pass.

Environment: x86_64 Linux KVM guest, Intel Core Ultra 7 155H, 8 vCPUs;
rustc 1.98.0 (`88d9e12ae`), Cargo 1.98.0. The bench profile used opt-level 3,
fat LTO, one codegen unit and stripped symbols, with no custom RUSTFLAGS or
CPU pinning. Hardware profiling was unavailable (`perf stat` reported no
supported events), so phase-separated Criterion measurements guided the work.

Changes:

- Child and attribute steps filter directly into the final result vector,
  eliminating intermediate axis and per-parent result vectors. Child scans
  charge each candidate before pushing, including candidates that do not match.
- Predicates compact only the current parent's suffix in place, retaining its
  `position()`/`last()` context and predicate-scan budget charges.
- A single child/attribute context needs no document-order sort. Prepared
  multi-context results are checked for ordering before allocating cached sort
  keys; unordered results retain the stable sorting fallback.
- Initial XPath preparation makes best-effort arena/index reservations for
  virtual attributes, accounting for recycled slots before growing the arena.

### Measured latency

Criterion medians in microseconds, 30 samples, 1-second warmup and 2-second
measurement per case. SAML has 40 assertions (29,955 bytes); preparation uses
400 elements with 8 attributes each (62,883 bytes). Wide cases use 1,024
`<item id='x'/>` children (13,319 bytes), prepared except where stated.
Parsing and destruction are excluded from the preparation timing; query
timings exclude document parsing/preparation and include returned-vector drop.

| Benchmark | Before (us) | After (us) | Delta |
|---|---:|---:|---:|
| SAML one-shot XPath | 60.99 | 45.55 | -25.3% |
| SAML repeated XPath | 56.34 | 40.33 | -28.4% |
| SAML attribute selection (`repeat_position`) | 61.13 | 41.97 | -31.3% |
| Cached `1 + 1` | 0.182 | 0.176 | -3.2% |
| Wide child selection | 24.43 | 11.07 | -54.7% |
| Wide child selection, no matches | 17.59 | 8.84 | -49.8% |
| Wide attribute selection | 126.50 | 40.13 | -68.3% |
| Wide chained predicates | 282.52 | 265.58 | -6.0% |
| Wide child selection, unprepared DOM | 185.65 | 10.09 | -94.6% |
| Initial attribute preparation | 705.09 | 282.74 | -59.9% |

The small-expression and predicate improvements were not consistent across
every run; the substantial wins were result construction and preparation.
The full suite initially flagged text parsing and pull-to-DOM slowdowns.
Back-to-back reruns of the saved binaries (40 samples, 3-second measurement)
did not reproduce them: text parsing was 246.08 -> 226.47 us and pull-to-DOM
292.77 -> 274.35 us. Neither path was optimized; treat these fluctuations as
host/build variability, not as parser speedup claims.

Reproduction, with the same benchmark source present in both builds:

```sh
# Baseline library, then modified library:
cargo bench --offline --bench uppsala -- --save-baseline round2-before \
  --warm-up-time 1 --measurement-time 2 --sample-size 30 --noplot
cargo bench --offline --bench uppsala -- --baseline round2-before \
  --warm-up-time 1 --measurement-time 2 --sample-size 30 --noplot
```

Raw estimates are local artifacts under `target/criterion/`; the table uses
`median.point_estimate`, not Criterion's printed regression slope. Saved
executables in `target/perf-round2/{before,after}` allowed the control reruns:

```sh
for stage in before after; do
  target/perf-round2/$stage --bench 'parse/text_heavy|pull/to_dom_saml' \
    --save-baseline round2-check-$stage --warm-up-time 1 \
    --measurement-time 3 --sample-size 40 --noplot
done
```

### Process memory observations and limits

Fresh processes were run with `/usr/bin/time`, selecting one timed benchmark
per process. Criterion still executes other groups' untimed fixture setup, so
these are **whole-harness high-water RSS**, not per-input or live-heap figures.
Profile mode runs for a fixed duration: its elapsed time is not query latency.

| Selected benchmark | Before elapsed / peak RSS | After elapsed / peak RSS |
|---|---:|---:|
| Repeated SAML XPath | 1.91 s / 5,696 KiB | 1.35 s / 5,456 KiB |
| Wide attribute selection | 1.04 s / 5,664 KiB | 1.39 s / 5,460 KiB |
| Attribute preparation | 1.04 s / 6,168 KiB | 1.59 s / 6,512 KiB |

```sh
for stage in before after; do
  for query in '^xpath/repeat_axis$' '^xpath_shapes/attributes$' '^prepare/'; do
    /usr/bin/time -f 'elapsed_s=%e peak_rss_kib=%M' \
      target/perf-round2/$stage --bench "$query" --profile-time 1
  done
done
```

No peak-memory reduction is claimed. Preparation's reservation trades earlier
capacity allocation for fewer growth operations; its observed harness RSS was
slightly higher. Dedicated allocator profiling is needed to quantify live
heap and allocation counts.

Validation: `cargo test --offline --quiet` passed 747 tests (5 ignored),
including conformance, XSLT, security, mutation/recycling and new per-parent
predicate/budget regressions. Formatting and diff checks passed. Two small
XPath fuzz seeds cover chained predicates and overlapping contexts; no fuzz
campaign was run in this pass. No public API, dependency, cache-limit or
default-budget changes were made.
