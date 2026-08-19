# First-party lens pack

This directory contains first-party `prog.lens_manifest` manifests. `prog`
loads only top-level `.json`, `.yaml`, and `.yml` files from the lens directory;
fixtures live under `fixtures/` and are ignored by the runtime loader.

Included lenses:

- `run.failures`: compact failure-first view for `prog run` captures.
- `run.streams`: bounded stdout/stderr head-tail view for command captures.
- `cargo-test`: Cargo test failure triage.
- `go-test`: Go test failure triage.
- `npm-test`: npm test failure triage.
- `pytest`: pytest failure triage.
- `junit`: JUnit XML failure triage for any runner that emits JUnit.
- `logs`: generic head-tail log triage with deterministic log findings.
- `observe.text.logs`: head-tail triage for profile-free text observations.
- `observe.ndjson.records`: event-row triage for NDJSON observations.
- `json.items.triage`: generic `/items` JSON collection triage.
- `github-issues`: `gh` issue-list run capture triage.
- `github.issues.triage`: issue list triage for profiled `list_issues` calls.
- `gh-actions-log`: GitHub Actions workflow-log triage with annotation findings.
- `kubectl-json`: Kubernetes JSON object status triage.
- `terraform-plan`: Terraform/OpenTofu plan `resource_changes` triage with
  destructive-change findings.
- `trivy`: Trivy vulnerability-report triage with provider-severity findings.
- `sarif`: SARIF diagnostic triage for any linter that emits SARIF.
- `unified-diff`: unified-diff review with risky-hunk findings.
- `mcp-jsonrpc-error`: MCP JSON-RPC error-envelope triage.
- `llm-api-error`: LLM provider API error-envelope triage.
- `otel-ndjson`: OpenTelemetry NDJSON log-record triage.

Each manifest includes positive fixtures, counterexample fixtures, explicit
omitted paths, expansion actions, and invariants. CI validates every manifest
and checks that positive fixture projections are smaller than raw payloads and
beat a simple 2 KiB truncation baseline. A docs test also fails when the list
above drifts from the manifests on disk.
