# First-party lens pack

This directory contains first-party `prog.lens_manifest.v1` manifests. `prog`
loads only top-level `.json`, `.yaml`, and `.yml` files from the lens directory;
fixtures live under `fixtures/` and are ignored by the runtime loader.

Included lenses:

- `run.failures`: compact failure-first view for `prog run` captures.
- `run.streams`: bounded stdout/stderr head-tail view for command captures.
- `observe.text.logs`: head-tail triage for profile-free text logs.
- `observe.ndjson.records`: event-row triage for NDJSON observations.
- `json.items.triage`: generic `/items` JSON collection triage.
- `github.issues.triage`: issue list triage for profiled `list_issues` calls.

Each manifest includes positive fixtures, counterexample fixtures, explicit
omitted paths, expansion actions, and invariants. CI validates every manifest
and checks that positive fixture projections are smaller than raw payloads and
beat a simple 2 KiB truncation baseline.
