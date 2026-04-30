# ADR 0005: Sanitize `version` and `encoding` in the XML Declaration at Serialization Time

## Status

Accepted

## Context

[ADR 0004](0004-security-resource-limits.md) introduced sanitizers for
comment / PI / CDATA content in `src/writer.rs` and the DOM serializer in
`src/dom.rs`, closing F-13 / F-14 / F-15. The threat model for that fix is
**round-trip injection**: an attacker who can influence strings that become
part of a serialized XML document should not be able to terminate the
enclosing node early and smuggle arbitrary markup into the output.

A differential review of the `security` branch (see
`DIFFERENTIAL_REVIEW_REPORT.md`, finding **M-1**) identified a residual gap
in the same threat model: the XML declaration itself is still written
verbatim.

- `Document::write_document_to` (`src/dom.rs:1183-1197`) emits
  `<?xml version="{ver}" encoding="{enc}"?>` by straight
  `out.write_str(&decl.version)` / `out.write_str(enc)`, with no escape
  and no `?>` check.
- `XmlWriter::write_declaration_full` (`src/writer.rs:58-78`) does the
  symmetric thing for the imperative builder.

`Document::xml_declaration` is a `pub` field (`src/dom.rs:384`), so
consumer code that mutates the declaration with an attacker-influenced
string hits exactly the same smuggle:

```rust
doc.xml_declaration = Some(XmlDeclaration {
    version: "1.0".into(),
    encoding: Some("UTF-8\"?><inject/><?x ".into()),
    standalone: None,
});
doc.to_xml();
// => <?xml version="1.0" encoding="UTF-8"?><inject/><?x "?>...
```

The commit message for ADR 0004's work explicitly names "forge an XML
declaration" as an attack class it protected against — but only for the
forward direction (PI target `xml` being rewritten to `_xml`). The
mutate-the-existing-declaration case was not covered.

## Decision

Apply the same `pub(crate) fn safe_*` + `Cow<'_, str>` pattern used for
the existing sanitizers, to `version` and `encoding`:

| Helper | Matches | Fallback on mismatch |
|---|---|---|
| `writer::safe_xml_version` | Exact string `"1.0"` or `"1.1"` | `"1.0"` |
| `writer::safe_xml_encoding` | XML 1.0 §4.3.3 `EncName ::= [A-Za-z] ([A-Za-z0-9._] \| '-')*` | `"UTF-8"` |

For `version`, this is intentionally narrower than XML 1.0's general
`VersionNum ::= '1.' [0-9]+` production: the parser only accepts `"1.0"`
and `"1.1"`, so emitting any other syntactically-valid value (e.g.
`"1.42"`) would produce a document this library cannot reparse. The
helper therefore accepts only those two exact values and substitutes
`"1.0"` for everything else.

Wired into both serializer entry points:

- `XmlWriter::write_declaration_full` (`src/writer.rs`)
- `Document::write_document_to` (`src/dom.rs`), which is the single funnel
  for `to_xml`, `to_xml_with_options`, `write_to`, `write_to_with_options`,
  `node_to_xml`, and the `Display` impl.

### Why substitute rather than escape or reject

The attribute-value escapes used elsewhere (`&quot;` etc.) are not legal
inside the XML declaration — the parser doesn't process entity references
there, so an escaped `&quot;` would end up literal in the output and fail
to round-trip. Rejecting via `Result` would require changing the signature
of `write_document_to` and `write_declaration_full`, breaking public API.

Substituting to a safe, universally-valid sentinel (`"1.0"` / `"UTF-8"`)
matches the pattern already established by `sanitize_pi_target("xml") →
"_xml"`: silently rewrite the invalid input to a safe form. The output is
always well-formed; the round-trip always reparses; no API signature
changes.

### Why the fallback values

- `"1.0"` — universally accepted, required by `XML 1.0` conformance.
  `"1.1"` would satisfy the same `VersionNum` production, but `1.0` is the
  broader-compat default and matches `XmlWriter::write_declaration()`'s
  hard-coded behaviour.
- `"UTF-8"` — the only encoding an XML processor is required to accept
  (plus UTF-16) per XML 1.0 §4.3.3. Uppsala parses input as UTF-8 by
  default, so this is also the actual on-wire encoding.

### Fail-closed semantics

Consistent with ADR 0004:

- Invalid `version` → output gets `"1.0"`. Parses cleanly.
- Invalid `encoding` → output gets `"UTF-8"`. Parses cleanly.

The smuggled bytes never reach the output. Consumers that were relying on
an invalid declaration (e.g. `version="2.0"` or `encoding="foo bar"`) get
silent substitution — a behaviour delta, but the previous behaviour was
to emit a non-round-trippable document anyway.

## Consequences

### Positive

- **Closes M-1** from the differential review. The XML declaration joins
  comment / PI / CDATA / PI-target as a fully-sanitized serializer output.
- **No API break.** Both entry points keep their existing signatures and
  infallible return types.
- **Symmetric with the existing pattern.** `pub(crate) fn safe_*` helpers
  returning `Cow<'_, str>`, with borrowed fast path on valid input.
- **No allocation on the safe path.** A valid `"UTF-8"` passes through as
  `Cow::Borrowed`.
- **Defaults applied automatically.** Callers who upgrade get the
  hardening with no opt-in.

### Negative

- **Silent substitution is surprising.** A caller who sets
  `encoding = "weird encoding"` now silently sees `UTF-8` in the output.
  Acceptable because the pre-fix behaviour produced malformed XML; any
  such caller was already broken.
- **No diagnostic.** A caller with genuinely malformed input doesn't get
  an error — the fix is invisible at call time. The same property applies
  to the other four sanitizers; consistency wins.

### Future work (out of scope for this ADR)

- A `strict` mode on `Document` / `XmlWriter` that returns `Result` from
  serialization when any sanitizer fires. Useful for libraries that want
  to detect (not just contain) attacker-controlled input.
- Validation at the `XmlDeclaration` constructor (rather than at
  serialization time), so an invalid value never enters the DOM in the
  first place.

## Tests

Added to `src/writer.rs` test module:

1. `safe_xml_version_passes_valid` — `"1.0"`, `"1.1"`
2. `safe_xml_version_rejects_invalid` — empty, `"1"`, `"1."`, `"2.0"`,
   `"1.10"`, `"1.42"`, `"1.0a"`, `"1.0 "`, injection string
3. `safe_xml_encoding_passes_valid` — `"UTF-8"`, `"utf-8"`,
   `"ISO-8859-1"`, `"US_ASCII.1"`
4. `safe_xml_encoding_rejects_invalid` — empty, digit-first, leading
   dash, injection, space, embedded NUL
5. `roundtrip_xml_writer_declaration_version_injection_blocked` —
   attacker-controlled version through `XmlWriter::write_declaration_full`
6. `roundtrip_xml_writer_declaration_encoding_injection_blocked` — ditto
   for encoding
7. `roundtrip_dom_declaration_version_injection_blocked` — same threat
   model exercised through `Document::to_xml`
8. `roundtrip_dom_declaration_encoding_injection_blocked` — ditto

All W3C conformance suites (XML 100 %, NIST 100 %, Sun 100 %, MS 99.8 %)
unchanged.
