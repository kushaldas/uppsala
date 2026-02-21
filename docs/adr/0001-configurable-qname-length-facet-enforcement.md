# ADR 0001: Configurable QName/NOTATION Length Facet Enforcement

## Status

Accepted

## Context

XSD 1.0 Part 2 defines length, minLength, and maxLength facets that can constrain
the value space of simple types. For most types, the meaning of "length" is clear
(e.g., number of characters for strings, number of octets for hexBinary). However,
for `xs:QName` and `xs:NOTATION`, the specification does not define a well-defined
length measure.

W3C Bug #4009 identifies this ambiguity. The XSD 1.0 Part 2 spec (section 4.3.1.3,
clause 1.3) states that length is measured in "units of length" varying by type, but
provides no definition for QName or NOTATION. Different implementations interpret
this differently.

The two major W3C XSD conformance test suites directly contradict each other:

- **NIST Datatypes** (19,217 tests): Expects length facets on QName to be
  **ignored**. Tests define schemas with `maxLength="1"` on QName types, then
  provide multi-character prefixed QName values and mark them as valid.

- **MS DataTypes** (1,213 tests): Expects length facets on QName to be
  **enforced**. Tests define schemas with reasonable length constraints on
  unprefixed QNames and mark non-matching values as invalid.

Both test suites are authoritative W3C conformance tests. Achieving 100% on both
simultaneously requires supporting both interpretations.

## Decision

We make QName/NOTATION length facet enforcement **configurable** via a runtime flag
on `XsdValidator`:

- Added a `enforce_qname_length_facets: bool` field to `XsdValidator`, defaulting
  to `true` (enforcement enabled).
- Added a public setter `set_enforce_qname_length_facets(bool)` to allow callers
  to disable enforcement.
- When the flag is `false`, `validate_facet()` skips `Length`, `MinLength`, and
  `MaxLength` facets when the base type is `QName` or `NOTATION`.
- The default (`true`) matches the MS/Sun test suite behavior and the common
  implementation behavior of enforcing length facets.
- The NIST test runner explicitly sets the flag to `false` before running.

The length computation for QName (when enforcement is enabled) uses:
- Unprefixed QNames: `local_name.len()`
- Prefixed QNames: `namespace_uri.len() + local_name.len()`

This follows the interpretation that the "length" of a QName is related to its
expanded name, consistent with how MS tests define their expected behavior.

## Consequences

- **NIST Datatypes: 100% (19,217/19,217)** -- all 44 previously-failing QName
  length tests now pass with enforcement disabled.
- **MS DataTypes: 99.8% (1,211/1,213)** -- the 4 previously-failing QName length
  tests now pass with enforcement enabled. The 2 remaining failures are unrelated
  (anyURI schema composition, fixed-value whitespace semantics).
- **Sun Combined: 90.5% (180/199)** -- unchanged (failures are in unimplemented
  subsystems: identity constraints, substitution groups).
- Library users who need NIST-compatible behavior can call
  `validator.set_enforce_qname_length_facets(false)` after creating the validator.
- The default behavior (enforcement enabled) is the more conservative choice and
  matches the majority of real-world XSD processors.
- If the W3C resolves the ambiguity in a future spec revision, this flag can be
  deprecated and the behavior locked to whichever interpretation is standardized.
