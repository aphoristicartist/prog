# Offline context-cost analysis

[#280](https://github.com/aphoristicartist/prog/issues/280) is implemented by
[`analyze_context_cost.py`](../fixtures/harness/analyze_context_cost.py). It reads
existing evaluation artifacts, performs no source execution or network requests,
and writes an aggregate JSON report without copying raw text, commands, file
paths, cursors, or excerpts into the output.

Run it against the checked-in comparison and evidence ledgers:

```sh
python3 fixtures/harness/analyze_context_cost.py \
  fixtures/evals/competitive-baseline-metrics.json \
  fixtures/evals/evidence-cli-metrics.json \
  fixtures/evals/evidence-acquisition-metrics.json \
  --output /tmp/prog-context-cost.json
```

The same command accepts the existing installed coding-loop report, registered
coding-loop report, or combined installed-facade comparison. Those reports have
response bodies and can support field attribution. Generate an installed report
with the existing driver, then analyze it offline:

```sh
python3 fixtures/harness/installed_coding_loop.py \
  --prog "$PWD/target/debug/prog" --output /tmp/prog-installed-loop.json
python3 fixtures/harness/analyze_context_cost.py \
  /tmp/prog-installed-loop.json --output /tmp/prog-installed-cost.json
```

Input order and source-row indices are retained, and each source has a content
hash. Given the same input bytes, the output is deterministic. Reports from
failed fixture runs remain analyzable and keep their failed outcome. Unsupported
schemas produce a JSON error; a future live-evaluation ledger needs an explicit
adapter and tests before it can be treated as a supported format.

## What the report establishes

| Report field | Meaning |
|---|---|
| `recorded_response_bytes` / `known_response_bytes` | Full recorded responses, including repeated and unsuccessful requests. An incomplete size ledger has an unavailable total and a separate known subtotal. |
| `declared_response_bytes` / `inconsistent_byte_counts` | Preserve the source's total and expose conflicts with the recorded steps. |
| `known_response_bytes_by_operation` | Capture, findings, inspection, search, evidence, expansion, status and other command costs. Child argv is never mistaken for the leading command. |
| `known_bytes_by_field` | Actual JSON value spans for findings, previews, excerpts, citations, references, instructions, notices/errors and other metadata. JSON keys, punctuation and whitespace have a separate framing bucket. |
| `profiled_response_bytes` / `unprofiled_response_bytes` | How much of the recorded response total has bodies available for attribution. |
| `retrieval_requests`, `failed_retrievals`, `unclassified_retrievals` | Requests, explicit failures, and requests whose outcome cannot be established from the retained ledger. |
| `unique_consulted_refs` | Deduplicated successful returned recoverable references. It is unavailable if any retrieval cannot be classified; the known subtotal is reported separately. |
| `repeated_response_bytes`, `repeated_excerpt_value_bytes`, `repeated_finding_record_bytes` | Repeated presentations are charged every time. These overlapping diagnostics locate duplication; they are not additional costs or measured potential savings. |
| Graded outcome and counterexample fields | Join existing grader results without treating a lack of retrieval as success or evidence that omitted content was unnecessary. |

The competitive and CLI-evidence artifacts currently retain per-step sizes but
omit response bodies. Their field-level costs and successful unique-reference
counts therefore remain unavailable. The aggregate evidence-acquisition report
has approximate token totals without response bodies or exact byte counts;
the analyzer does not reverse the token estimate into invented byte measurements.

Installed reports include bodies, exact exported-file read costs, and the
existing outcome checks. Registered reports additionally distinguish canonical
CLI calls from caller-visible tool presentations, request serialization, and
presentation wrappers. The internal duplicate `value` field is excluded from
the delivered presentation. These are separate accounting views, not totals
that should be added together. Available instruction-file size is kept separate
from context the fixture explicitly records as delivered.

## Using the result for the next change

Use the body-level breakdown to choose a representation experiment for #116:
excerpts duplicated in citations, repeated metadata, or a predictable evidence
read after capture. Compare the resulting whole response sequence and its
graded evidence outcome against the original. A reduction in one field alone
does not demonstrate a cheaper successful workflow. Retain tiny, no-answer,
failed, unavailable and contradictory cases.

No runtime telemetry, persistent usage database, suppression of previously seen
content, pricing assumptions, provider token usage, or model-success claim is
introduced. Actual agent behavior remains the separate #282 experiment.

The standard workspace tests execute the Python regression suite through
[`context_cost_analysis.rs`](../crates/prog-cli/tests/context_cost_analysis.rs).
The suite covers repeated presentations, unique references, denied/failed reads,
partial ledgers, inconsistent totals, missing bodies, empty denominators, host
wrappers, private input text, deterministic output, and the real checked-in
artifact families.
