# Profile-free observations

`prog observe` captures stdin or a file without requiring a source profile. It
uses the same redaction, cache, cursor, envelope, and expansion contracts as
profile-backed `prog call`.

Use `observe` for one-off artifacts such as JSON payloads, SARIF reports,
NDJSON logs, JUnit XML, HTML pages, unified diffs, plain text logs, test output
saved to a file, or pasted command output.

See [Path discovery](paths.md) for filtering omitted regions and using ranked
`next_actions`.

## Parser/Indexer Pipeline

`prog observe` selects one parser from a deterministic parser/indexer registry.
The current registry recognizes JSON, SARIF, NDJSON, JUnit XML, basic HTML,
unified diff, and bounded text fallback artifacts.

The selected parser is reported under `observation.parser`:

```json
{
  "id": "json",
  "label": "JSON",
  "confidence": 1.0,
  "lossy": false,
  "fallback": false,
  "reason": "mime type and JSON syntax matched",
  "path_semantics": "JSON Pointer",
  "range_semantics": "JSON value ranges"
}
```

`confidence` explains how strong the parser match was. `lossy` is true when the
preview is a bounded or extracted representation instead of the complete source
structure. `fallback` is true when no structured parser accepted the artifact
and the text fallback was used instead.

Malformed structured inputs, unknown text MIME types, mixed encodings, long
lines, and repeated stack traces fall back to bounded text observations when the
content is text-like. Binary-looking input is rejected with a structured error.

## JSON and SARIF

```bash
prog observe --file ./large.json --mime application/json --name large-json
prog --lens-dir ./lenses observe --file ./large.json --mime application/json --lens json.items.triage
```

Observation cache entries default to a 24-hour TTL. Override it when a fixture
or workflow needs shorter-lived evidence:

```bash
prog observe --file ./large.json --ttl-seconds 300
```

JSON and SARIF observations keep the original JSON shape, so expansion uses
normal JSON Pointer paths. A typical agent loop is observe, list candidate paths,
then expand the specific evidence needed:

```bash
prog paths pc1_... --prefix /items
prog expand pc1_... --path /items/0/body
```

## NDJSON

```bash
cat events.ndjson | prog observe --stdin --mime application/x-ndjson --name events
cat events.ndjson | prog --lens-dir ./lenses observe --stdin --mime application/x-ndjson --name events --lens observe.ndjson.records
```

NDJSON observations are wrapped as:

```json
{
  "format": "ndjson",
  "records": [],
  "record_count": 0,
  "line_count": 0,
  "byte_count": 0
}
```

List and expand records with paths such as `/records/10`:

```bash
prog paths pc1_... --prefix /records
prog expand pc1_... --path /records/10
```

## JUnit XML, HTML, and Unified Diff

```bash
prog observe --file ./junit.xml --mime application/junit+xml --name tests
prog observe --file ./page.html --mime text/html --name page
prog observe --file ./change.diff --mime text/x-diff --name patch
```

These formats are indexed into bounded structured previews while preserving
cursor-backed source lines. Exact expansion is available through text line paths
such as `/lines/5/text`. The parser metadata marks these extracted previews as
lossy and describes their range semantics.

## Text

```bash
cargo test 2>&1 | prog observe --stdin --mime text/plain --name cargo-test
cargo test 2>&1 | prog --lens-dir ./lenses observe --stdin --mime text/plain --name cargo-test --lens observe.text.logs
```

Text observations expose a bounded head/tail preview plus cursor-backed line
paths:

```json
{
  "format": "text",
  "head": [],
  "tail": [],
  "lines": [{"number": 1, "text": "..."}],
  "line_count": 1,
  "byte_count": 10,
  "utf8_valid": true
}
```

List line paths, then expand a specific line with:

```bash
prog paths pc1_... --prefix /lines
prog expand pc1_... --path /lines/0/text
```

Unlike CLI/MCP text adapters that persist only head/tail finding input, an
observed text artifact retains every redacted line under `/lines`. Finding
derivation therefore examines the full stored artifact, not only the preview
windows. Re-observing the same file or named stdin source also keeps a stable
invocation fingerprint when its contents change, so `prog delta` can recognize
a finding that moved to another line as `persisting` rather than treating the
new payload as an unrelated invocation.

## Safety

- JSON and NDJSON are redacted through the normal persistence redaction policy.
- Text observations redact obvious `token=...`, `password=...`, `secret=...`,
  `api_key=...`, and `authorization: ...` patterns before storage.
- Invalid UTF-8 text is accepted with replacement characters and a warning.
- Binary-looking input is rejected with a structured error.
- Bulk processing should still use `prog expand --out <file>` rather than
  putting large slices into model context.
