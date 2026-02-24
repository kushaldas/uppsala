# ADR 0002: SIMD-Accelerated Byte Scanning for Parser Hot Loops

## Status

Accepted

## Context

After profiling Uppsala's parser on SAML and large XML files, the two hottest
functions were `parse_content` (19% of CPU) and `parse_quoted_value_with_entities`
(5% of CPU). Both contained tight byte-at-a-time loops scanning for delimiter
characters using 256-byte lookup tables (`CONTENT_SCAN` and `ATTR_SCAN`).

These loops process one byte per iteration to find the next "interesting" byte:
- Content scanning: `<`, `&`, `\r`, `]`
- Attribute scanning: `&`, `<`, and the closing quote character

On text-heavy documents (long runs of plain text or large attribute values between
markup), this byte-at-a-time approach becomes the dominant cost. SSE2 intrinsics
can compare 16 bytes simultaneously using packed equality comparisons, reducing
the per-byte overhead by an order of magnitude.

**Constraint:** Uppsala has zero external dependencies, so all SIMD code must use
`std::arch::x86_64` from the standard library (stable since Rust 1.27).

## Decision

We introduced a new `src/simd.rs` module with two public functions:

- `scan_content_delimiters(data: &[u8]) -> (usize, bool)` -- scans for `<`, `&`,
  `\r`, `]` and returns the byte offset of the first delimiter plus a flag
  indicating whether any non-ASCII or illegal control characters were seen.

- `scan_attr_delimiters(data: &[u8], quote: u8) -> (usize, bool)` -- scans for
  `&`, `<`, and the closing quote byte, with the same validation flag.

**SSE2 implementation (x86_64):** Each function broadcasts its delimiter bytes
into 128-bit registers and uses `_mm_cmpeq_epi8` + `_mm_or_si128` to test all
delimiters in parallel. `_mm_movemask_epi8` extracts a bitmask, and
`trailing_zeros()` locates the first match. Non-ASCII detection comes for free
from `_mm_movemask_epi8` on the raw chunk (high bit = byte >= 0x80). Control
characters (bytes < 0x20 excluding TAB, LF, CR) are detected using
`_mm_min_epu8` for unsigned comparison. A scalar tail handles the remaining
< 16 bytes.

**Scalar fallback (non-x86_64):** Inline byte comparisons equivalent to the
former lookup tables, ensuring correct behavior on aarch64, wasm, and other
targets.

**Architecture dispatch:** SSE2 is part of the x86_64 baseline (guaranteed on all
x86_64 processors), so no runtime feature detection is needed. The dispatch is
purely compile-time via `#[cfg(target_arch = "x86_64")]`.

The lookup tables (`CONTENT_SCAN`, `ATTR_SCAN`) were removed from `parser.rs`
since their logic is now fully subsumed by the SIMD and scalar implementations
in `simd.rs`.

## Consequences

- **Text-heavy documents see large speedups.** `gigantic.svg` (1.3 MB) went from
  1.7x to 5.3x faster than roxmltree. `text.xml` (126 KB) reached 9.3x faster.
- **Attribute-heavy documents improved.** `attributes.xml` (265 KB) went from
  1.1x to 2.0x faster.
- **SAML files (3-11 KB) improved modestly** from 1.1-1.8x to 1.5-1.8x faster,
  since text runs between markup are shorter (10-500 bytes).
- **Small documents (< 1 KB) are unchanged** -- SIMD setup cost is negligible but
  so is the benefit over few bytes.
- **All W3C conformance suites remain at 100%.** The SIMD path produces identical
  results to the former lookup-table path.
- **No new dependencies.** `std::arch::x86_64` is part of the standard library.
- **Future work:** aarch64 NEON intrinsics could be added behind
  `#[cfg(target_arch = "aarch64")]` using the same function signatures. AVX2
  (32 bytes/iter) is possible but has diminishing returns and vzeroupper overhead.
