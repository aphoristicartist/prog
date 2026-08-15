# prog documentation

Complete reference set. Start with the [project README](../README.md) for the
overview and quickstart; contributors and coding agents should read
[`AGENTS.md`](../AGENTS.md).

## Using prog

| Doc | Covers |
|---|---|
| [walkthroughs.md](walkthroughs.md) | End-to-end task walkthroughs. |
| [run.md](run.md) | Capturing local commands with `prog run`. |
| [observe.md](observe.md) | Capturing files and stdin; supported artifact formats. |
| [source-setup.md](source-setup.md) | Registering HTTP and CLI source profiles. |
| [evidence-navigation.md](evidence-navigation.md) | `inspect`, `search`, `find`, `evidence`, `expand`. |
| [paths.md](paths.md) | Listing addressable JSON Pointer paths. |
| [findings.md](findings.md) | How deterministic findings are produced and ranked. |
| [coding-providers.md](coding-providers.md) | Bounded pytest and Cargo/rustc normalization. |
| [integrations.md](integrations.md) | Agent skills, hooks, and the MCP stance. |

## Verification

| Doc | Covers |
|---|---|
| [delta.md](delta.md) | Conservative comparison of two observations; when absence is provable. |
| [verification.md](verification.md) | Verification obligations, independent mutation read-back, and readiness gating. |
| [mcp-tasks.md](mcp-tasks.md) | Long-running MCP task lifecycle. |
| [evidence.md](evidence.md) | Observation records, lineage, and evidence references. |
| [source-state.md](source-state.md) | Native validators, declared change tokens, MCP modification annotations, and revalidation. |
| [evidence-acquisition.md](evidence-acquisition.md) | Evidence-acquisition eval and its baseline. |

## Contracts and safety

| Doc | Covers |
|---|---|
| [contracts.md](contracts.md) | The disclosure envelope and other public contracts. |
| [safety.md](safety.md) | Redaction, effect policy, trust, and fail-closed gates. |
| [cache.md](cache.md) | Cache lifecycle, retention, and purge. |
| [metadata.md](metadata.md) | Observation metadata fields. |
| [lenses.md](lenses.md) | Lens manifests. Lenses are data and cannot execute code. |
| [lens-packs.md](lens-packs.md) | Distributing lenses as packs. |
| [../INVARIANTS.md](../INVARIANTS.md) | The fourteen invariants and their executable tests. |

## Evaluation and positioning

| Doc | Covers |
|---|---|
| [token-economics.md](token-economics.md) | Raw-vs-prog token ratios across fixtures. |
| [task-success-eval.md](task-success-eval.md) | Task-success evaluation. |
| [replay-eval.md](replay-eval.md) | Multi-iteration replay oracle for delta and readiness correctness. |
| [agent-eval.md](agent-eval.md) | Actual-agent claim gate status and false-completion grader replay. |
| [competitive-baselines.md](competitive-baselines.md) | Comparisons, including cases prog loses. |
| [real-world-demos.md](real-world-demos.md) | Real-world-shaped local demos. |
| [positioning.md](positioning.md) | When to use prog and when not to. |
| [cost.md](cost.md) | Storage and disclosure economics. |
| [token-economics.md](token-economics.md) | Regeneration command for measured tables. |
| [release-notes.md](release-notes.md) | Per-release reference. |

## Design records

- [rfcs/0001-progressive-disclosure-gateway.md](rfcs/0001-progressive-disclosure-gateway.md)
- [rfcs/0002-type-theory-formal-methods-and-reflexivity.md](rfcs/0002-type-theory-formal-methods-and-reflexivity.md)
- [rfcs/0003-observation-lenses.md](rfcs/0003-observation-lenses.md)

## A note on the numbers

Every measured figure in these docs is generated from checked-in fixtures, not
written by hand. Regenerate with:

```sh
PROG_TOKEN_EVAL_UPDATE=1 cargo test -p prog-cli --test eval -- --nocapture
```

Do not hand-edit figures in `token-economics.md`, `evidence-acquisition.md`, or
`fixtures/evals/*.json`.
