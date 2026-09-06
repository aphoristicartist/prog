# Terminal-Bench 2.0 paired pilot

This is the preregistered public-benchmark slice of the actual-agent evaluation
([#236](https://github.com/aphoristicartist/prog/issues/236)). No live trial has
run yet, and nothing in this directory supports a performance claim.

The machine-readable preregistration is
[`fixtures/agent-eval/terminal-bench-2/preregistration.json`](../fixtures/agent-eval/terminal-bench-2/preregistration.json).
The git commit that first contains that file is the preregistration proof. The
benchmark source, Harbor, Claude Code, Claude model, `prog` release, seed,
resource-bounded subset, arm order, settings, stopping rule, analysis, and
falsification conditions are all fixed there before credentialed execution.
Its `amendments` list records installation corrections made before model
execution; git history retains the original registration.

## Design

The pilot uses ten tasks selected from the 89-task source commit. Eligibility is
limited to CPU tasks whose agent and verifier timeouts are at most 1,200 seconds
and whose declared memory is at most 4 GiB. The 54 eligible task IDs are sorted
by `SHA-256(seed + NUL + task_id)` and the first ten are selected. This is a
cost-control rule fixed without looking at model outcomes.

Each task runs once in each arm. Five tasks use raw-first order and five use
`prog`-first order. Harbor runs one trial at a time, so arms stay interleaved
instead of running one complete arm and then the other. Both arms use the exact
Terminal-Bench instruction, Claude Code 2.1.241, `claude-fable-5`, high effort,
100 maximum turns, the task-native timeout, and a USD 10 per-trial ceiling.
Harbor and Claude Code do not expose an independent total-token stop, so that
limit is unavailable rather than fabricated; provider token usage is reported
afterward when present in both arms.

The `prog` arm subclasses Harbor's Claude Code integration only to upload the
verified v0.1.1 Linux release and run its shipped
`prog init --agent claude-code --project --root /app` command. Setup checks that
the installed skill equals that release's `init --print-skill` output and that
its explicit wrapper is executable. It does not alter the task instruction,
agent run method, benchmark environment, grader, or retry policy.

The original registration named `harness install`, which v0.1.1 does not expose.
The [first native Linux installation preflight](https://github.com/aphoristicartist/prog/actions/runs/34005198184/job/101410978802)
installed stock Claude Code successfully and failed in the `prog` arm with
`unrecognized subcommand 'harness'`. The recorded amendment uses v0.1.1's
existing Claude Code integration instead. This measures the released skill and
explicit wrapper, not a newer harness integration. No model or grader ran
before this correction, and no release, model, task, or budget pin changed.

## Preflight without model credentials

Install Harbor 0.22.0 in an isolated environment and ask it to resolve each
configuration without running trials. Configuration resolution alone does not
test container creation or installation:

```sh
export PYTHONPATH="$PWD/fixtures/agent-eval/terminal-bench-2"
uvx --from 'harbor==0.22.0' harbor run \
  --config fixtures/agent-eval/terminal-bench-2/pilot-raw-first.yaml \
  --print-config
uvx --from 'harbor==0.22.0' harbor run \
  --config fixtures/agent-eval/terminal-bench-2/pilot-prog-first.yaml \
  --print-config
```

Before the credentialed run, prepare the exact Linux release binary with the
repository's verified installer. This downloads the release checksum and
requires GitHub build-provenance attestation verification:

```sh
pilot_bin_dir="$(mktemp -d)/bin"
PROG_VERSION=v0.1.1 \
PROG_TARGET=x86_64-unknown-linux-gnu \
PROG_INSTALL_DIR="$pilot_bin_dir" \
PROG_MODIFY_PATH=0 \
sh install.sh
export PROG_PILOT_BINARY="$pilot_bin_dir/prog"
```

On native x86_64 Linux with Docker and the Docker Compose plugin, exercise the
official runner's `--install-only` mode before requesting a paid pilot. The
[`installation preflight workflow`](../.github/workflows/terminal-bench-preflight.yml)
does this without model credentials. It installs both preregistered arms on
the first selected task, with agent execution and verification disabled:

```sh
preflight_dir="$(mktemp -d)/installation"
uvx --python 3.13.14 --from 'harbor==0.22.0' \
  python "$PYTHONPATH/install_preflight.py" prepare \
  --binary "$PROG_PILOT_BINARY" --output "$preflight_dir"
env -i PATH="$PATH" HOME="$HOME" PYTHONPATH="$PYTHONPATH" \
  uvx --python 3.13.14 --from 'harbor==0.22.0' harbor run \
  --config "$preflight_dir/config.json" --install-only
uvx --python 3.13.14 --from 'harbor==0.22.0' \
  python "$PYTHONPATH/install_preflight.py" check \
  --config "$preflight_dir/config.json"
```

The helper only derives an official configuration and checks Harbor's setup
receipts; Harbor still owns installation, scheduling, and cleanup. It preserves
the benchmark commit, harness and release pins, arm settings, and retry policy.
Use a new output directory for each preflight. Harbor 0.22.0 can exit zero when
installations fail, so a successful shell exit is insufficient: the receipt
check must also pass. CI preserves the official job directory and check report
as artifacts, including failures.

These setup-only receipts are not pilot attempts, dropouts, task grades, or
evidence of model performance. They establish installation for one task image,
not readiness of all ten images or availability of the pinned model. AMD64
images running under emulation on an ARM Docker host are outside this native
Linux preflight; local setup failures there do not establish a native Linux
failure. The paid pilot outcome file remains unchanged.

Do not run the credentialed commands until a concrete pilot budget is approved.
When it is approved, export `ANTHROPIC_API_KEY` and run the same two configs
without `--print-config`. Preserve both complete Harbor job directories. Raw
and `prog` outcomes, dropouts, final claims, grader results, and comparable
provider usage feed the existing claim gate through #238; they are never
hand-copied into prose.

## Stopping and reporting

The pilot attempts the 20 scheduled task-arm trials exactly once. Infrastructure
failures, safety refusals, timeouts, and usage failures remain explicit
dropouts; they do not authorize replacement tasks or outcome-driven reruns. A
powered run is not designed or started until the pilot's actual cost, runtime,
and dropout rate are published. Null and negative outcomes remain publishable.
