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
| `completed_retrievals`, `repeated_retrievals` | Successful reads with recoverable returned identities, including repeated reads of an already consulted reference. Unknown retrievals make these totals unavailable; `known_*` fields retain proven subtotals. |
| `unique_consulted_refs` | Deduplicated successful returned recoverable references. It is unavailable if any retrieval cannot be classified; the known subtotal is reported separately. |
| `repeated_response_bytes`, `repeated_excerpt_value_bytes`, `repeated_finding_record_bytes` | Repeated presentations are charged every time. These overlapping diagnostics locate duplication; they are not additional costs or measured potential savings. |
| Graded outcome and counterexample fields | Join existing grader results without treating a lack of retrieval as success or evidence that omitted content was unnecessary. |
| `reported_artifact_bytes`, `reported_expansion_count` | Preserve available source-size and expansion counters for small-output losses and retrieval comparisons. They do not establish whether an additional read was necessary. |

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

### Ranked decisions from the checked-in ledgers

These are decisions about the next experiment. The command above reproduces the
source totals; source-row indices below are zero-based. Input hashes identify the
exact ledger revision. The examples retain their existing fixture graders and do
not represent observed agent performance.

1. **Test capture plus initial findings plus evidence in #116.** In
   `evidence-cli-metrics.json`, findings rows 1, 4, 7, 10 and 13 total 39,761
   response bytes across 11 calls; inspect rows 2, 5, 8, 11 and 14 total 63,112
   bytes across 16 calls. All ten rows expose the required evidence. This
   supports the existing composition experiment and its whole-sequence grading.
   It does not identify a safe field to delete: those ledgers omit bodies.
2. **Keep the tiny-output routing control in #120.** Competitive rows 90 and
   97 expose the same 57-byte fixture answer, but the first envelope costs 3,218
   bytes. Row 98 adds an expansion and reaches 7,401 bytes. Test existing route
   guidance and compact presentation against that loss before changing host
   routing in #120. A size measured after capture cannot retroactively justify
   a pre-execution bypass.
3. **Prioritize wrong-selection controls in #118/#139.**
   Competitive row 128 spends 20,627 bytes and one expansion on the unranked
   target yet still has insufficient evidence. Its source records selection of
   `/head/0` instead of the required `/lines/1200/text`. Widening the selected
   excerpt has no demonstrated benefit here. Preserve the failure and test
   bounded discovery before changing #116; do not convert the nonzero recall
   into success.
4. **Profile repeated metadata and excerpts before deleting either in #116.**
   The size ledger for the pytest finding workflow (evidence CLI row 10)
   establishes a 7,617-byte, two-call cost, but has no bodies to attribute it.
   Generate and analyze the installed-loop report above to expose
   `metadata_and_other`, `repeated_excerpt_value_bytes`, and
   `repeated_finding_record_bytes`. Use those measured candidates in the
   composition experiment. No specific deletion or universal excerpt-width
   change is justified by the versioned size-only ledgers.
5. **Do not add runtime telemetry in #126 from these results.** Competitive
   rows 127 and 128 both fail to expose the unranked answer even though their
   expansion counts differ. Existing ledgers already answer that recall alone
   is insufficient. Per-read required-evidence contribution, abandoned live
   tasks, host fallback causes, and actual agent outcomes need explicit records
   and graders from #279/#282; they remain unavailable here.

The standard workspace tests execute the Python regression suite through
[`context_cost_analysis.rs`](../crates/prog-cli/tests/context_cost_analysis.rs).
The suite covers repeated presentations, unique references, denied/failed reads,
partial ledgers, inconsistent totals, missing bodies, empty denominators, host
wrappers, private input text, deterministic output, and the real checked-in
artifact families.
