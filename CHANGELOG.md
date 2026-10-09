# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.12.0] - 2026-10-09

### Added

- `Document::write_node_to_with_options` serializes one node into any
  `fmt::Write` sink, and `Document::node_serialized_size_hint` suggests a
  capacity for it, so repeated fragment serialization can reuse one buffer.
- Add allocation-free `Document::children_count()` and double-ended child
  iteration, including mixed forward/backward traversal.
- Add `XPathEvaluator::clear_cache()` to release cached compiled expressions.
- Add a Criterion benchmark harness covering parsing, DOM traversal, XPath,
  attribute-node preparation, serialization, and pull parsing, plus a dedicated
  profiling build profile.

### Changed

- XPath `//child::T` and `//attribute::T` pairs without predicates are
  evaluated as one descendant walk that tests nodes in place, and every step
  resolves its node test's namespace prefix once instead of once per
  candidate. Over a 7 MB SAML aggregate,
  `//md:EntityDescriptor/md:IDPSSODescriptor` drops from 23.7 ms to 5.3 ms
  (libxml2: 8.7 ms), `//md:EntityDescriptor/@entityID` from 22.8 ms to
  5.4 ms (libxml2: 6.9 ms), and `count(//*)` from 24.6 ms to 3.5 ms. Results
  and ordering are unchanged. See ADR 0020.
- The explicit `descendant::` and `descendant-or-self::` axes walk the
  subtree by sibling/parent links and test in place rather than collecting
  the subtree first.
- Serialization pre-sizes its output buffer from the node's source range
  (`to_xml`, `node_to_xml`, and the `_with_options` variants), avoiding the
  doubling reallocations on multi-megabyte documents, and skips the
  per-element prefix hash map for elements with few namespace declarations.
- Reduce temporary allocations in DOM and XPath traversal by using child
  iterators. `Document::descendants()` now uses an iterative depth-first walk
  instead of recursive traversal.
- Cache compiled XPath expressions within each evaluator to avoid repeated
  tokenization and parsing. Oversized expressions still evaluate without being
  cached, and only successful evaluations populate the cache.
- Use dependency-free fast hashing for internal node-ID maps and sets while
  retaining randomized hashing for XML-derived entity names and namespace
  prefixes.
- Enable fat LTO, a single codegen unit, and stripping for this crate's release
  builds. Applications using Uppsala as a dependency control their own profiles.
- Stream XPath child and attribute steps directly into their result, compact
  predicate results in place, and skip redundant document-order sorting.
  Node-visit budgets and per-parent predicate positions remain enforced.
- Reserve virtual-attribute arena and index capacity before initial XPath
  preparation to avoid repeated growth on attribute-heavy documents.

### Fixed

- Preserve document order before evaluating positional XPath descendant
  predicates, and share exhaustion state between both ends of child iterators
  so mixed traversal cannot return the same child twice.
- Register the SAML namespace in the one-shot XPath benchmark so it performs
  the same query as the repeated-evaluation benchmark.
- Use fixed-width values in the hash byte-equivalence test for portability to
  both 32-bit and 64-bit targets.
- Restore direct DOM construction in `Parser::parse`. Since 0.9.0 the DOM
  parser consumed `PullParser` events, copying a 144-byte event several times
  per node; on node-dense inputs that doubled parse time and erased the
  parse-speed lead over libxml2. A shared crate-private tokenizer now drives
  either a DOM sink (arena nodes built in place) or the pull-event sink, so
  the public `PullParser`/`PullEvent` API and the `document_from_pull`
  signature are unchanged while DOM parsing is back to its pre-0.9.0 speed.
  See ADR 0019.
  `document_from_pull` now returns an error when handed a `PullParser` that
  has already produced events, instead of silently building a tree missing
  the consumed declaration, DOCTYPE, or ancestors.


### Security

- Bound the XPath cache to 128 entries, 4 KiB of source per entry, 64 KiB of
  owned key/AST capacity per entry, and 1 MiB of that capacity overall, plus
  bounded table and allocator overhead. These limits bound retained cache data,
  not total parsing or evaluation memory.
- Clear compiled expressions when the evaluator's depth limit changes so
  cached programs cannot bypass the new limit. Bypass a poisoned advisory
  cache instead of panicking.

## [0.11.0] - 2026-10-07

### Added

- Add `xslt::ParameterValue` and `Stylesheet::transform_with_params()` for
  invocation-local top-level XSLT parameters. Parameters accept literal Unicode
  strings or XPath expressions that retain their result types, including node
  sets. Call parameters override builder defaults without changing the compiled
  stylesheet or leaking values between transformations.
- Support unprefixed, namespace-qualified, and expanded parameter names. Literal
  arguments bind before expression arguments, which evaluate in supplied order.

### Fixed

- Resolve XSLT global variable and parameter dependencies, including forward
  references, and report unresolved or circular dependencies. Buffer
  `xsl:message` output during deferred global initialization so retries do not
  emit duplicate messages.
- Preserve declaration-site namespace bindings for XSLT expressions, match
  patterns, and local variable and parameter names.
- Ignore `with_param()` defaults that do not name declared top-level parameters
  before evaluating invocation-local expressions, preventing ignored defaults
  from becoming visible as variables.
- Provide the implicit reserved `xml` namespace binding in XPath and XSLT,
  including match patterns, and prevent public XPath namespace additions or
  programmatically constructed stylesheet declarations from rebinding it.

- Validate inherited simple-content values against their underlying simple type
  and reject child elements inside simple-content elements ([#43](https://github.com/kushaldas/uppsala/issues/43)).

## [0.10.1] - 2026-09-02

### Fixed

- Preserve the existing document element `NodeId` in
  `Document::replace_tree_from()` so live root views observe the replacement.
  Removed descendants are detached and replacement nodes have no stale source
  ranges.

## [0.10.0] - 2026-09-02

### Added

- Added `Document::replace_tree_from()` for replacing a document's attached
  tree from another DOM while keeping the existing arena alive. Previously
  issued `NodeId` handles therefore remain valid and refer to detached old
  nodes rather than aliasing unrelated nodes in the replacement tree. This
  enables bindings to replace documents safely after a serialize/process/parse
  boundary without sharing Rust-owned DOM pointers between dynamic libraries.

## [0.9.0] - 2026-07-05

### Security

- Hardened the pull parser fail-closed contract: direct `PullParser::next_event()`
  now fuses after any parse error, matching the iterator adapter and preventing
  post-error events or repeated errors from being observed by streaming
  consumers.
- XSLT computed `xsl:element` and `xsl:attribute` names are now validated as XML
  QNames before serialization, rejecting markup-smuggling values from attribute
  value templates.
- XPath public evaluation now rejects trailing tokens, and flat binary operator
  chains count against the configured expression-depth cap before they can build
  deeply nested ASTs.
- XSD identity constraint validation now indexes normalized key/unique/keyref
  tuples instead of linearly scanning prior tuples, removing quadratic duplicate
  and keyref lookup paths while preserving decimal-equivalent comparisons.
- XSD schema building now rejects duplicate top-level `xs:group` and
  `xs:attributeGroup` definitions before parsing replacement bodies, preventing
  overwrite-and-clone model group expansion from exhausting memory.
- XSLT stylesheets can now opt into materialized result-tree and serialized
  output byte caps with `Stylesheet::set_max_result_tree_bytes` and
  `Stylesheet::set_max_output_bytes`, letting fuzz and hostile-input callers
  bound legal output-amplification cases without changing the default
  unbounded transform API.
- `parse_bytes()` now normalizes any retained XML declaration encoding to
  `UTF-8`, so string serialization cannot emit stale metadata such as UTF-8
  bytes declaring `UTF-16`.

### Added

- Regression and fuzz coverage for the security hardening above, including
  direct pull error fusion, computed XSLT QName injection attempts, XPath
  trailing-token and flat-chain depth checks, XSD identity tuple lookup, and
  stale encoding declarations in `parse_bytes()`.
- Pull-based XML event parser (`uppsala::pull`, ADR 0018): `PullParser`
  iterates `PullEvent`s — XML declaration, DOCTYPE, start/end namespace,
  start/end element (with resolved QNames, attributes, depth, and byte
  ranges), text, CDATA, comment, and processing instruction — over a decoded
  string without building a DOM. It carries the same namespace awareness,
  nesting-depth cap, entity-expansion budget, and `forbid_dtd` /
  `forbid_entities` hardening switches as `Parser`, and
  `pull::document_from_pull` / `pull::parse_document` materialize a DOM from
  the event stream. `Parser::parse` is now layered on the pull parser, so the
  two surfaces share one scanner and one set of well-formedness checks.
- Pull-parser regression coverage at parity with the DOM parser.
  `tests/pull_differential.rs` runs the accepted and invalid regression
  corpora through both `Parser::parse` and `document_from_pull` (comparing
  serialized output, XML declaration, DOCTYPE, source ranges, and exact error
  text, across all parser options) and additionally drives the scan-only
  event stream under the ADR 0018 stream invariants (element and namespace
  balance, matching names and depths, in-bounds byte ranges, fused iterator
  after an error). Every hand-crafted conformance suite (XML, namespace,
  XPath, XSD, XSD composition, serialization, ranges) now parses through a
  shared checked helper (`tests/common/mod.rs`) that asserts pull/DOM
  agreement on each fixture, and each W3C family has a pull-agreement
  counterpart: `w3c_pull_event_stream_agrees_with_dom_parser` sweeps every
  UTF-8 XML conformance file (~2250), and
  `xsts_{nist_datatypes,ms_datatypes,sun_combined}_pull_agreement` sweep all
  ~26,500 schema/instance documents of the three XSTS families, asserting
  the raw event stream accepts/rejects with the same error text as the DOM
  parser. All of these run in CI (the XSTS steps' substring filters pick up
  the new sweeps; `--test pull_differential` was added to the hand-crafted
  step; `just test-pull` and `just test-handcrafted` cover them locally).
  During bring-up the xmlconf sweep
  caught (and this release fixes, before it ever shipped) an empty-entity
  end-of-document bug: `<!ENTITY e "">` expanded in content made
  `Parser::parse` fail with `UnexpectedEof` on valid documents (W3C
  valid-sa-023/085/086, rmt-e2e-15a), a divergence the suites' pass-rate
  thresholds had let through.
- New `fuzz_pull` libFuzzer harness (12th target): asserts the ADR 0018
  event-stream invariants and the pull-vs-DOM accept/reject agreement on
  arbitrary input. Wired into the fuzz scripts, dictionaries, seed import,
  and the weekly CI fuzz smoke exactly like the existing parser targets, with
  the empty-entity witness tracked as a regression seed.
- `just bench-libxml2` now also reports pull scan-only and pull-to-DOM
  timings alongside the DOM and libxml2 numbers.

## [0.8.0] - 2026-07-03

### Added

- In-repo cargo-fuzz / libFuzzer harness suite (`audit/fuzz/`, ADR 0014) with
  eleven targets covering the untrusted-input surfaces: the parser (`&str` and
  bytes/UTF-16 entry points), a parse→serialize→reparse round-trip fixpoint
  oracle, arbitrary-DOM serialization, DOM mutation + `prepare_xpath()`, XPath,
  XSLT transforms, the XSD builder, the XSD regex engine, and two differential
  harnesses that assert the `unsafe` SSE2 SIMD scanners return byte-identical
  results to their scalar references. The harness crate is a detached workspace,
  so the library keeps its zero-dependency guarantee; a fuzz-only `fuzzing`
  feature (off by default, no dependencies) exposes the internal scan halves via
  `uppsala::fuzz_exports`. Crash inputs found and fixed during the campaign are
  preserved as tracked regression seeds under `audit/fuzz/seeds/`.

### Changed

- The parser now enforces the reserved namespace-binding rules of Namespaces in
  XML 1.0 (Third Edition) §3 and rejects: the XML or XMLNS namespace declared as
  the default namespace, the XML namespace bound to any prefix other than `xml`,
  any binding of the XMLNS namespace, and `xmlns:*` declarations whose prefix is
  not an NCName (`xmlns:=`, `xmlns:a:b=`). Such documents were never
  namespace-well-formed (conformant parsers reject them) but were previously
  accepted, which made serialization non-idempotent. The redundant, legal
  `xmlns:xml="http://www.w3.org/XML/1998/namespace"` is still accepted. See
  ADR 0017. W3C conformance is unchanged (xmlconf and XSTS suites at 100%).
- `NamespaceResolver::declare` now also ignores a binding of the XML namespace
  to any prefix other than `xml` (including the default namespace), and the
  serializer never emits such a stored declaration.
- Serializer performance: element and attribute QNames are written piecewise
  (no per-name join allocation), seen attribute names are tracked as borrows,
  children are walked through sibling links instead of a per-element `Vec`, the
  `fmt::Write` escape path is run-based and SIMD-accelerated, and
  `is_valid_xml_ncname` validates ASCII names in a single SIMD pass
  (`scan_ncname_continuation`, with the scalar/SSE2 pair under differential
  fuzz + unit-test guard). Output is byte-identical.
- `xsd_regex` internals avoid `unwrap()` in `Result`-returning parse functions
  (no functional change).

### Fixed

- `prepare_xpath()` no longer grows the node arena on every re-preparation:
  superseded virtual attribute slots are recycled in place, keeping arena size
  flat across mutate→query→mutate rounds (previously quadratic growth, observed
  as a multi-GB blowup under pyFF-style workloads).
- Attribute `NodeId`s remain stable across `prepare_xpath()` re-preparation for
  elements whose attribute list did not change shape; a cached attribute handle
  can no longer silently alias a different element's attribute after an
  unrelated mutation. The invalidation rule is documented on
  `get_attribute_nodes`/`prepare_xpath`.
- DOM tree mutators (`append_child`, `detach`, `remove_child`, `insert_before`,
  `insert_after`, `replace_child`) reject virtual attribute nodes and the
  document node as operands instead of corrupting the owner element's child
  list (e.g. `append_child` with an attribute node could silently drop all of
  an element's real children).
- The SSE2 content/attribute scanners' `needs_validation` flag is now
  byte-identical to the scalar reference: it previously accumulated over bytes
  past the first delimiter (a benign over-report that only caused redundant
  validation, confirmed by 3M differential trials, but a real cross-path
  divergence).
- Serialization is a one-pass parse→serialize fixpoint for the reserved-prefix
  family, closing all 129 `fuzz_roundtrip` findings: a stripped reserved
  `xml:`/`xmlns:` prefix leaves an NCName-sanitized local name (multi-colon
  names collapse to `_` instead of shedding one prefix layer per round, ADR
  0015), and the bare name is emitted with an `xmlns=""` undeclaration when a
  non-empty default namespace is in scope, so re-parsing no longer captures it
  into that namespace (ADR 0017).
- XPath `substring()` follows XPath 1.0 §4.2 exactly: bounds are compared as
  f64 character positions, so a huge-negative or `-inf` start argument can no
  longer overflow an integer cast (a panic in builds with overflow checks,
  found by `fuzz_xpath`), and the spec's NaN/infinity examples all hold
  (`substring('12345', 0, 3)` = `'12'`, `substring('12345', 0 div 0)` = `''`,
  `substring('12345', -42, 1 div 0)` = `'12345'`).
- XPath `round()` rounds half-way values toward positive infinity per XPath 1.0
  §4.4 (`round(-2.5)` = `-2`, `round(-0.5)` = negative zero) instead of away
  from zero, and `substring()` uses the same rounding for its bounds so the two
  stay consistent.

## [0.7.1] - 2026-07-02

### Security

- XSD validation now fails closed for unresolved element references instead of
  validating them as unconstrained content, and namespace-sensitive attribute
  declarations and strict attribute wildcards now compare expanded names instead
  of falling back to local-name matches.
- XSD datatype and facet validation is stricter for hostile inputs: date/time
  facets compare actual instants, non-temporal enumerations no longer receive
  date/time normalization, malformed negative dates are rejected, invalid pattern
  facets fail closed, and `xs:QName` rejects unbound prefixes.
- XSD identity constraints now reject `xs:unique`, `xs:key`, and `xs:keyref`
  fields that select more than one node, preserving the XSD single-field-value
  rule instead of silently choosing the first value.
- DTD content-model parsing now observes the parser nesting-depth limit, so
  deeply nested declarations fail gracefully with the same configurable cap as
  element nesting.
- XSLT generated comments and processing instructions reject content that would
  break out of XML markup, and opt-in EXSLT `str:padding()` is capped to prevent
  attacker-selected output allocation.

## [0.7.0] - 2026-07-01

### Added

- XSLT 1.0 transform engine (`uppsala::transform`, plus `Stylesheet::compile` /
  `Stylesheet::transform`), layered on the existing XPath 1.0 evaluator with no
  second XPath implementation. Implements the "Tier A" subset that pyFF's
  stylesheets use (see ADR 0010 and the crate docs). Adds the XPath
  `$variable`/extension-function resolution seams, a compiled-expression entry
  point, XSLT match patterns, and the `lang()`/`current()` core functions.
- Opt-in EXSLT extension-function library (`src/exslt.rs`): `math:` (abs, sqrt,
  power, log, exp, sin, cos, tan, constant, min, max, highest, lowest), `str:`
  (concat, padding, align), `set:` (distinct, difference, intersection,
  has-same-node), and `exsl:object-type`. Enabled per stylesheet via
  `Stylesheet::with_exslt(true)`; matched by the conventional EXSLT prefix.
  `date:date-time()` remains available unconditionally.
- `Stylesheet::set_max_depth` and `DEFAULT_MAX_XSLT_DEPTH` (default 500): a bound
  on XSLT template-activation recursion.
- Opt-in libxml2-compatible lenient XSD datatype validation via
  `XsdValidator::set_lenient(bool)` (default off, strict/spec-faithful). The one
  relaxed rule is `xs:anyURI`: a value containing a space is accepted, matching
  libxml2/lxml/pyFF (strict mode still rejects it per RFC 3987). Applies to
  `anyURI` in element content and attribute values alike; no other datatype,
  facet, or structural check is weakened. See ADR 0012.
- `Document::import_subtree(&mut self, src, src_id) -> Option<NodeId>`: deep-copy
  a node and its entire subtree from another document into this one in a single
  native pass, returning the new detached root node id (used by the pyuppsala DOM
  wrapper for cross-tree element moves).

### Changed

- XSLT transforms of large, wide documents are linear instead of quadratic.
  Several O(width²) hot spots were removed: the XSLT match-pattern tester no
  longer materializes every sibling to check membership (it tests the node
  locally and only falls back to the full set for positional predicates);
  `prepare_xpath` now precomputes a per-node document-order index so node-set
  deduplication (`dedup_document_order`) sorts by an O(1) key instead of walking
  each node to the document root and re-indexing wide ancestor sibling lists on
  every call (previously quadratic for relative paths like `name/text()`
  evaluated once per sibling); and a per-node template-dispatch pre-filter skips
  patterns that cannot match a node's kind/name. Net effect: a 91 MB eduGAIN
  aggregate that previously did not complete now transforms in a few seconds
  with every pyFF stylesheet (1–5 s each).
- `xs:import/@schemaLocation` is now treated as a hint (XSD 1.0 Part 1 §4.2.3):
  an import whose location cannot be resolved (unsupported scheme, missing file,
  or a path outside the base directory) is skipped instead of aborting the schema
  build, matching libxml2/Xerces. `xs:include`/`xs:redefine` are unchanged (they
  still treat base-directory escapes and unsupported absolute-URI schemes as hard
  errors). This lets composite schemas such as pyFF's `schema.xsd` build. See
  ADR 0011.
- XML serialization escaping (`XmlWriter` text/attribute output and the DOM
  serializer) is now run-based: safe byte runs are bulk-copied with one
  `push_str` (SIMD `memcpy`) instead of pushing one character at a time, which is
  faster on ASCII-heavy payloads like SAML metadata. Output is byte-identical to
  the previous per-character version.

### Security

- XSLT recursion is now bounded by `DEFAULT_MAX_XSLT_DEPTH` (default 500,
  overridable via `Stylesheet::set_max_depth`). A self-recursive
  `xsl:call-template`, mutually-recursive named templates, or an
  `xsl:apply-templates select="."` cycle previously recursed unbounded and
  aborted the process with an uncatchable stack-overflow `SIGABRT`; they now
  return a graceful `XmlError`.

## [0.6.0] - 2026-06-30

### Added

- In-repo `performance-harness/` crate for comparing Uppsala against a sibling
  `roxmltree` checkout, with `suite`, `saml`, and `file` run modes. Kept outside
  the main crate's targets so library builds and tests do not depend on
  `roxmltree`.
- `docs/performance.md`: benchmark tables and reproducible commands for running
  the performance harness and profiling the parser with `perf`.
- README "Performance" section with measured comparison tables (roxmltree
  benchmark inputs and SAML-shaped inputs) on x86_64.
- ADR 0009: decision to keep parser byte-scanning single-pass and
  dependency-free (no `memchr`).
- `justfile` targets for focused test suites and performance-harness runs.

### Changed

- Faster default-namespace resolution: `NamespaceResolver` caches default
  namespace state and restores it via a stack on scope push/pop instead of
  re-scanning scopes.
- Parser hot-path optimizations: improved attribute-value scanning, reduced
  per-reference allocations on the predefined-entity and numeric-character
  -reference paths, and denser arena pre-allocation heuristics for large inputs.
- Added a dependency-free `find_byte` helper in `src/simd.rs` used by the parser.

### Fixed

- Reject empty numeric character references (`&#;` and `&#x;`) with a clear
  "Invalid decimal/hex character reference" error instead of a misleading
  "Character reference U+0000 is not a valid XML character" report.
- Numeric character-reference overflow now reports the hex/decimal-specific
  message, matching the invalid-digit error wording.

### Security

- Node-arena pre-allocation now uses `Vec::try_reserve` instead of `reserve`, so
  a hostile or very large input cannot trigger an aborting upfront allocation via
  the global allocation-error handler. The reservation is a best-effort hint:
  on failure it falls back to a sparser estimate, then to no pre-reservation,
  and parsing still succeeds by regrowing the arena on demand.
