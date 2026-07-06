# Profile-free observations

`prog observe` captures stdin or a file without requiring a source profile. It
uses the same redaction, cache, cursor, envelope, and expansion contracts as
profile-backed `prog call`.

Use `observe` for one-off artifacts such as JSON payloads, NDJSON logs, plain
text logs, test output saved to a file, or pasted command output.

See [Path discovery](paths.md) for filtering omitted regions and using ranked
`next_actions`.

## JSON

```bash
prog observe --file ./large.json --mime application/json --name large-json
prog --lens-dir ./lenses observe --file ./large.json --mime application/json --lens json.items.triage
```

Observation cache entries default to a 24-hour TTL. Override it when a fixture
or workflow needs shorter-lived evidence:

```bash
prog observe --file ./large.json --ttl-seconds 300
```

JSON observations keep the original JSON shape, so expansion uses normal JSON
Pointer paths. A typical agent loop is observe, list candidate paths, then
expand the specific evidence needed:

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

## Safety

- JSON and NDJSON are redacted through the normal persistence redaction policy.
- Text observations redact obvious `token=...`, `password=...`, `secret=...`,
  `api_key=...`, and `authorization: ...` patterns before storage.
- Invalid UTF-8 text is accepted with replacement characters and a warning.
- Binary-looking input is rejected with a structured error.
- Bulk processing should still use `prog expand --out <file>` rather than
  putting large slices into model context.
