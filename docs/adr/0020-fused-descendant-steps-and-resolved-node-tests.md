# ADR 0020: Evaluate `//` as one descendant walk with pre-resolved node tests

## Status

Accepted (2026-10-08). Builds on ADR 0010 (document-order index).

## Context

Profiling pyuppsala against lxml on the 7.1 MB SWAMID aggregate (1032
entities, 44,823 nodes) on 2026-10-08 showed `//md:EntityDescriptor/@entityID`
at 23.7 ms in the uppsala evaluator against 8.7 ms for libxml2, with the
binding adding under 0.3 ms. `perf` placed the time in three places:

| Function | Self time | Why |
|---|---:|---|
| `collect_descendants` | 38% | `//` is parsed as `descendant-or-self::node()` followed by `child::T`. The first step materialized all 44,823 nodes into a vector through an explicit stack, then passed through a `node()` test that accepts everything. |
| `apply_step` + `matches_node_test` | 37% | The second step visited every child of every node again, 44,823 candidates, and for each one looked up the test's namespace prefix in the `HashMap<String, String>` of registered prefixes. |
| SipHash + `memcmp` | 10% | The per-candidate prefix lookup hashed the prefix string with SipHash and then compared namespace URIs. |

`count(//*)` showed the same shape (24.6 ms), so the cost was structural, not
specific to the attribute axis.

## Decision

- **Fuse `//child::T` and `//attribute::T`.** `apply_steps` evaluates a
  location path step by step but, when a step is the parser-injected
  `descendant-or-self::node()` with no predicates and the next step is a
  `child::` or `attribute::` step with no predicates, it runs
  `apply_descendant_test` once instead: a pre-order walk of each context
  node's subtree that applies the test to every node (or to every element's
  attributes) as it is visited, with one budget charge per node. The two
  spellings select the same nodes in the same order, so results are
  identical; a single context needs no document-order sort, several contexts
  are deduplicated as before. A predicate on the step after `//` is positional
  relative to the parent, so such pairs are not fused and keep the two-step
  evaluation.
- **Resolve the node test once per step.** `ResolvedTest` turns a `NodeTest`
  into a variant holding borrowed `&str` namespace URI and local name (or
  `Never` for an unbound prefix, per XPath 1.0 §2.3) before the candidate loop.
  Each candidate then costs one kind match and at most two string compares.
  `matches_node_test` remains as a thin wrapper for the XSLT pattern matcher.
- **Walk descendants by pointers.** `walk_descendants` follows first-child,
  next-sibling and parent links, so the explicit stack and its per-node pushes
  are gone. The explicit `descendant::` and `descendant-or-self::` axes use the
  same walk and test nodes in place instead of collecting a subtree first.

The budget semantics are unchanged in kind (one charge per node visited); the
fused form charges fewer visits than the two-step form for the same query,
which lowers the DoS bound for callers, never raises it.

## Results

Laptop (Intel Core Ultra 7 155H), uppsala evaluator timed through pyuppsala
with the attribute index prepared, medians of 7:

| Expression | Before | After | libxml2 (lxml) |
|---|---:|---:|---:|
| `//md:EntityDescriptor/md:IDPSSODescriptor` | 23.7 ms | 5.3 ms | 8.7 ms |
| `count(//md:EntityDescriptor/md:IDPSSODescriptor)` | 23.9 ms | 5.2 ms | 8.7 ms |
| `//md:EntityDescriptor` | 22.3 ms | 5.1 ms | - |
| `count(//*)` | 24.6 ms | 3.5 ms | - |
| `/md:EntitiesDescriptor/md:EntityDescriptor/md:IDPSSODescriptor` (no `//`) | 1.6 ms | 0.9 ms | - |

The last row has no `//` at all; its gain is the resolved node test alone.
`cargo test` (all suites, including the W3C XML conformance and XSTS suites and
the XSLT pyFF stylesheet acceptance) passes unchanged; `src/xpath.rs` gained
tests asserting the fused and explicit spellings select the same nodes,
that predicates after `//` keep per-parent positions, and that unbound
prefixes match nothing.

## Consequences

- Parse-time rewriting of `//` was rejected on purpose: the XSLT pattern
  matcher (`matches_steps`) relies on the injected `descendant-or-self::node()`
  step to recognise a `//` connector. The fusion lives in evaluation only.
- `//T[pred]` still takes the two-step path. Fusing it would require tracking
  `position()`/`last()` per parent inside the walk; do that only if a profile
  shows it.
- Any new axis evaluation should resolve its node test with `ResolvedTest`
  before its candidate loop rather than calling `matches_node_test` per node.
