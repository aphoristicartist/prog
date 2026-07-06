# Cost planner

`prog cost` compares raw-context and `prog`-mediated observation flows for an
expensive model profile.

```bash
prog cost \
  --model-profile models/fable-class-2026-07.json \
  --raw-file fixtures/evals/payload.json \
  --expand-path /items/42/body \
  --estimated-output-tokens 800
```

Reports include:

- input tokens
- estimated output tokens
- total estimated cost
- savings ratio against the raw baseline
- context-window fit
- warnings and counterexamples

The model profile is the source of pricing truth. Pricing and model names change
frequently, so refresh `input_price_per_million_tokens`,
`output_price_per_million_tokens`, context window, and metadata before using a
report for budget decisions.

## Architecture

The intended workflow for expensive long-context models is:

```text
cheap/local collection -> prog observation -> expensive model reasoning -> targeted expansion
```

The expensive model should receive bounded envelopes, path listings, and exact
expanded evidence on demand. It should not receive raw full API dumps, complete
logs, or repeated cache-hit payloads by default.

## Scenarios

`prog cost` reports:

- `raw_payload`: full raw artifact in context
- `simple_truncation`: clipping to the model context window
- `prog_observe_only`: bounded first observation only
- `prog_observe_paths_expand`: observation plus path listing and requested
  expansions
- `repeated_cache_hits`: one capture plus repeated cached paths/expansions

## Counterexamples

Cost savings are not meaningful when:

- the payload is tiny
- one expansion reveals nearly the entire artifact
- model output dominates total cost
- a low-cost local model makes latency more important than input-token spend
