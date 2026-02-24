# ADR 0003: Skipped MS DataTypes anyURI Test (anyURI_a004)

## Status

Accepted

## Context

The W3C XML Schema Test Suite (XSTS) MS DataTypes suite contains 1,213 test
groups. Uppsala passes 1,212 and skips 1: the test group `anyURI_a004_1339`.

This test group's schema contains an `xs:include` with a `schemaLocation`
pointing to an absolute FTP URI:

```
ftp://ftp.is.co.za/rfc/rfc1808.txt
```

Uppsala's schema composition (`xs:include`, `xs:import`, `xs:redefine`) resolves
`schemaLocation` URIs relative to the filesystem. It supports relative paths and
`file://` URIs, but does not implement network fetching for `http://`, `https://`,
or `ftp://` URIs. When it encounters the FTP URI, schema compilation fails with:

```
Cannot resolve include schemaLocation 'ftp://ftp.is.co.za/rfc/rfc1808.txt':
absolute URI not supported
```

The test harness skips any test group whose schema fails to compile, so this
group's single instance test is counted as skipped rather than failed.

## Decision

We accept this skip rather than implementing network-based schema fetching.

**Rationale:**

- **Zero-dependency constraint.** Implementing HTTP/FTP fetching would require
  either a network client dependency or a substantial hand-written implementation
  of TCP, TLS, and the FTP protocol. This directly conflicts with Uppsala's core
  design constraint of zero external dependencies.

- **Offline-only design.** Uppsala is a parsing and validation library, not a
  network client. Schema resolution from local files covers all practical use
  cases. Applications that need network-based schema fetching can download schemas
  themselves and provide local paths.

- **The test is testing anyURI validation, not schema composition.** The FTP URI
  in the schema is an `xs:anyURI` test value, and the schema happens to use an
  FTP-based include. The underlying anyURI datatype validation is fully functional
  and covered by the other 1,212 passing tests.

- **The FTP server is defunct.** `ftp.is.co.za` is no longer reachable, so even
  implementations that support network fetching would fail on this test in
  practice.

## Consequences

- **MS DataTypes reports 100% (1,212/1,212)** with 1 skipped. The skipped test
  is not counted toward the pass rate since it cannot be compiled, not because
  validation produces a wrong result.
- If Uppsala ever gains a pluggable schema resolver callback (allowing callers
  to provide custom URI resolution), this test could be unskipped by providing
  the referenced schema content through the callback.
