# Observation lenses

Lens manifests are small, declarative view contracts for noisy artifacts. They
let a project teach `prog` how to show a better bounded first view without
making the raw payload unrecoverable.

Use lenses when the agent knows the artifact family but does not yet know the
exact slice it will need. Prefer native API filters, `jq`, or a domain-specific
command when the exact query is already known.

## Layout

By default, `prog call --lens <id>`, `prog observe --lens <id>`, and
`prog run --lens <id>` load manifests from `./lenses`. Override that with
`--lens-dir` or `PROG_LENS_DIR`.

```bash
prog --lens-dir ./lenses call github list_issues --args '{}' --lens github.issues.triage
prog --lens-dir ./lenses observe --file service.log --mime text/plain --lens observe.text.logs
prog --lens-dir ./lenses run --lens run.failures -- cargo test
```

Manifest files may be JSON, YAML, or YML. They are loaded from the top level of
the lens directory. Every loaded manifest is validated before the requested
lens is applied, so duplicate ids and invalid manifests fail early.

## Contract

The public contract is exposed through `prog meta LensManifest`.

```json
{
  "schema": "prog.lens_manifest",
  "id": "github.issues.triage",
  "match": {
    "source_kind": "http",
    "operation": "list_issues"
  },
  "view": {
    "root": "/items",
    "limit": 20,
    "fields": {
      "number": "/number",
      "title": "/title",
      "state": "/state",
      "labels": "/labels/*/name"
    }
  },
  "omit": [
    {
      "path": "/items/*/body",
      "reason": "large_string",
      "detail": "issue body is expandable on demand",
      "expandable": true
    }
  ],
  "next_actions": [
    {
      "kind": "expand",
      "path": "/items/{index}/body",
      "reason": "inspect issue body only when the preview looks relevant"
    }
  ],
  "findings": [
    {
      "kind": "issue_candidate",
      "path": "/items/*",
      "confidence": 0.8,
      "reason": "issue row is available for triage",
      "title": "issue candidate"
    }
  ],
  "invariants": [
    "envelope_under_budget",
    "no_fabricated_values",
    "redaction_dominates_expansion"
  ]
}
```

## Semantics

- `match` is enforced whenever a lens is selected. A lens can pin source kind,
  source id, operation, MIME type, and artifact kind.
- `view.root` selects the cursor root and first-view root.
- `view.limit` and `view.depth` override default preview policy for the lens.
- `view.fields` maps output field names to JSON Pointer selectors relative to
  each item under the root. A `*` segment collects values from arrays or
  objects.
- `omit` adds explicit omitted regions and reasons to the envelope.
- `next_actions` adds planner-facing actions before generated omission actions.
- `findings` declares data-only finding providers. Paths may use `*`; rules
  emit only for existing redacted payload paths. `contains_any` can
  conservatively restrict a rule by case-insensitive terms.
- Expansion still uses the original redacted cached payload, not the synthetic
  preview.
- The first-party pack lives in `lenses/`; see [First-party lens
  packs](lens-packs.md).

## Safety Rules

- Manifests are declarative. They cannot execute code.
- Paths must be JSON Pointers. Wildcards are allowed only where the compiler can
  keep them as bounded selectors or display paths.
- Omission and finding paths outside `view.root` are rejected. Invalid finding
  confidence, empty terms, and path escapes fail closed.
- Redaction happens before lens projection. A lens cannot recover redacted
  content.
- Envelope budgets still apply.

## Counterexamples

Do not use a lens when:

- the payload is already tiny
- the upstream API can return exactly the needed fields
- a one-line `jq` query is already known
- the lens would hide fields needed for a safety review
- the workflow needs live streaming output instead of cached expansion
