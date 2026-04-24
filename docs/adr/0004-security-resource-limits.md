# ADR 0004: Configurable Resource Limits for Defence-in-Depth

## Status

Accepted

## Context

A security audit of Uppsala 0.3.0 identified eleven High-severity findings
covering the full "hostile XML" threat model: billion-laughs entity expansion,
stack overflow from deeply-nested elements, polynomial ReDoS in the XSD regex
matcher, arbitrary-file-read via `xs:include`, round-trip injection via
programmatic serialization, etc. Every finding shared the same root cause:
Uppsala's hand-rolled parsers and matchers had **no configurable limits
anywhere**.

The library's original design prioritised throughput and W3C conformance; the
assumption was that callers would validate input size themselves before
handing it to the parser. In practice that meant a single ~1 KB malicious
XML document could stack-overflow the parser, OOM the process, or peg a core
for minutes. For a library that validates untrusted input by design, this
was not an acceptable posture.

We needed a uniform way to express "here is the maximum I'm willing to spend
on this operation" across six distinct recursion-or-expansion sites in the
library: the XML parser (element nesting), the entity expander (total
bytes), the XSD regex compiler (group-nesting depth), the XSD regex matcher
(per-match step count), the XPath parser/evaluator (expression depth), and
the schema composition loader (include-chain depth + cycle detection + path
containment).

## Decision

Adopt a consistent **"default constant + per-type builder"** pattern across
every resource-bounded operation, with one named public constant per limit
plus a symmetric `with_*` builder on the entry type. Every default is sized
to accommodate every pattern in the official W3C conformance suites
(1 208 XML tests + 20 156 XSD tests) while catching adversarial input in
bounded time.

### The seven limits

| Constant | Value | Applies to | Builder |
|----------|-------|------------|---------|
| `parser::DEFAULT_MAX_DEPTH` | 128 | Element-nesting depth during `parse()` | `Parser::with_max_depth(u32)` |
| `parser::DEFAULT_MAX_ENTITY_EXPANSION` | 1 MiB | Total bytes written by entity expansion | `Parser::with_max_entity_expansion(usize)` |
| `xsd::composition::MAX_INCLUDE_DEPTH` | 16 | `xs:include` / `xs:redefine` / `xs:import` nesting | (internal; composition-state auto-tracked) |
| `xsd_regex::DEFAULT_MAX_REGEX_GROUP_DEPTH` | 64 | `(...)` + `-[...]` nesting in pattern | `XsdRegex::compile_with_max_depth(&str, u32)` |
| `xsd_regex::DEFAULT_MAX_REGEX_STEPS` | 1 000 000 | `match_node` invocations per `is_match` call (scaled with input length, see below) | `XsdRegex::is_match_with_max_steps(&str, usize)` |
| `xpath::DEFAULT_MAX_XPATH_DEPTH` | 32 | Expression nesting in the XPath parser/evaluator | `XPathEvaluator::with_max_depth(u32)` |

Plus the path-containment and cycle-detection logic in
`src/xsd/composition.rs` (no user-facing constant; composition state is
per-build).

### Sizing rationale

Each default was chosen to be:

1. **Well above any legitimate usage** — verified against every W3C test in
   the XML Conformance Suite and the XSD Test Suite (NIST Datatypes 19 217
   tests, MS DataTypes 1 213, Sun Combined 199). All still pass at 100 %.
2. **Well below the point where adversarial input causes observable DoS.**
3. **Aware of platform context**, most notably `DEFAULT_MAX_XPATH_DEPTH = 32`
   (the XPath grammar has ~15 Rust stack frames per re-entry, so 32 × 15 =
   480 frames fits comfortably inside a 2 MiB default worker-thread stack
   even in debug builds).

### Input-scaled match budget (F-1 refinement)

`DEFAULT_MAX_REGEX_STEPS = 1 000 000` is a *floor*. The actual budget for
`XsdRegex::is_match(text)` is `max(1 000 000, text.chars().count() * 100)`.
This scaling is important: a legitimate linear pattern like `[a-z]+` against
a several-MB text value does ~1 step per character, so a fixed 1 M budget
would false-reject. Scaling at 100 steps per input character keeps the
budget generous enough for any O(n) pattern while tight enough that
polynomial blow-up (`O(n²)` or `O(n³)` ReDoS shapes) fail-closed in bounded
time. Callers who need a non-scaled tight budget use
`is_match_with_max_steps`.

### Matcher bookkeeping fix (F-2)

The step budget alone does not cap wall-clock time per step. The original
`match_repetition` implementation did `results.sort_unstable();
results.dedup();` on a growing vector every iteration, producing an
O(n² log n) wall-clock even on legitimate O(n) matches. This has been
replaced with a linear sorted merge (O(|a| + |b|) per iteration), since
both `results` and `next` are already sorted and deduplicated at the merge
point. Net effect: a linear pattern over N characters now runs in O(N) time
instead of O(N² log N).

### Fail-closed semantics

Every limit fails closed:

- Parser depth exceeded → `XmlError::parse("Element nesting exceeds maximum
  depth of N")`.
- Entity-expansion budget exceeded → `XmlError::parse("Entity expansion
  exceeds configured limit (0 bytes remaining)")`.
- Schema include depth / path containment / cycle → `XmlError::validation(...)`
  with a concrete reason.
- Regex group-depth exceeded → `Err("Pattern group nesting exceeds maximum
  depth of N")`.
- Regex match-step budget exhausted → `is_match` returns `false` (treated
  as "does not match", which is the security-correct outcome: an
  over-expensive value is rejected by the surrounding validator).
- XPath nesting exceeded → `XmlError::xpath("XPath expression nesting
  exceeds maximum depth of N")`.

### Serializer sanitization (not a cap, but same spirit)

The companion hardening work on the XML writer (`src/writer.rs`) and DOM
serializer (`src/dom.rs`) uses the same design language: reject or
transparently sanitize content that would break XML well-formedness —
comments containing `--`, PIs containing `?>`, PIs with reserved target
`xml`, CDATA containing `]]>`. Fail-closed in the same sense: "we cannot
emit this safely, so we emit a sanitized form that round-trips
semantically even if not byte-for-byte".

## Consequences

### Positive

- **All eleven High-severity audit findings are closed.** F-01 through F-15
  (minus gaps in numbering for Medium/Low items). See `SECURITY_AUDIT.md`
  for the full table.
- **Every fix is composable.** Callers can chain:
  `Parser::new().with_max_depth(256).with_max_entity_expansion(8 << 20)`.
- **Defaults are safe.** A caller who upgrades from a previous version and
  does nothing gets the hardening automatically, with no API break.
- **API is uniform.** Every public constant follows `DEFAULT_MAX_*`
  naming; every builder is `with_max_*` or `compile_with_max_*`.
- **W3C conformance unchanged.** 100 % on all suites.

### Negative

- **Consumer-visible behaviour changes for adversarial inputs.** Inputs
  that previously "worked" (hung forever, OOM'd, crashed) now return clean
  errors. Good for security; an upgrade surprise for anyone who was
  relying on those failure modes.
- **Legitimate inputs past the defaults need explicit opt-in.** A schema
  with a 300-level-deep `xs:import` chain, or an XPath expression with 50
  nested `[...]` predicates, will now error. The builder APIs are the
  escape hatch.
- **Tuning is empirical.** The seven constants were picked from a mix of
  "platform limits" (thread stack size drives the XPath cap), "empirical
  fit" (NIST passes at 100 %), and "round numbers that feel right" (128,
  64, 32, 16). They are not mechanically derived; they are a committed
  default that future ADRs can revise.

### Future work (out of scope for this ADR)

- A single `Limits` struct that bundles all seven constants, letting a
  caller configure the whole library with one builder call.
- A `SchemaResolver` trait so hosts can fully replace the default
  filesystem-backed `schemaLocation` loader (sandboxing, authenticated
  caches, deny-by-default policies).
- A Thompson-style NFA rewrite of the XSD regex matcher. Would drop the
  step budget entirely (matcher becomes genuinely O(n · m) in all cases)
  and remove the need for the input-scaled budget refinement. Large
  rewrite; deferred until it's clear users hit the current cap.
- Fuzz harnesses wired into CI. Source exists under `audit/fuzz/`.

### Migration note for downstream callers

Callers who hit a default and need more room can bump any limit locally:

```rust
use uppsala::{Parser, XPathEvaluator, XsdRegex};

// Accept deeper XML
let parser = Parser::new().with_max_depth(1024);

// Accept more generous entity expansion (5 MiB)
let parser = parser.with_max_entity_expansion(5 << 20);

// Accept regex with very deep groups
let re = XsdRegex::compile_with_max_depth(pattern, 256)?;

// Tight XPath budget for extra-strict environments
let eval = XPathEvaluator::new().with_max_depth(16);
```

No existing API signature changed; all adjustments are additive.
