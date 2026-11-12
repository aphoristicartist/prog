# Done gates side by side

`compare.py` puts prog's stop gate and four done gates from the Claude Code
plugin directory through the same eleven short sessions. Each session uses a
tiny Python project with a real bug and real `pytest`. The four rivals are
[done-needs-proof](https://github.com/sdvsignal/done-needs-proof),
[show-me-the-work](https://github.com/pinetreeB/show-me-the-work),
[llm-dark-patterns](https://github.com/waitdeadai/llm-dark-patterns) and
[dod-guard](https://github.com/atoslins/dod-guard).

A session is either a real fix, an honest report, or one of the ways to make
tests look green without fixing anything. The cheats are deleting, skipping
or swapping the failing test, running fewer tests, editing after the green
run, and claiming without running anything. A small emulator plays Claude
Code's side of the hook protocol from each plugin's own hook file:
- `SessionStart` and `UserPromptSubmit`;
- `PreToolUse` and `PostToolUse` around every edit and shell command, which
  really run;
- a Claude Code-format transcript at `transcript_path`;
- a final `Stop` carrying the last message.

A plugin **blocks** when a Stop hook exits 2 or prints
`{"decision": "block"}`. Nothing calls a model.

## Reproduce

Run it on Linux. dod-guard needs GNU `find` and bash 4 or newer, and
show-me-the-work needs Python 3.12 or newer. On macOS, dod-guard's detectors
fail and it blocks every stop.

```sh
mkdir plugins && cd plugins
git clone https://github.com/sdvsignal/done-needs-proof && git -C done-needs-proof checkout 8b8f2bd
git clone https://github.com/pinetreeB/show-me-the-work && git -C show-me-the-work checkout cbfe565
git clone https://github.com/waitdeadai/llm-dark-patterns && git -C llm-dark-patterns checkout e45815b
git clone https://github.com/atoslins/dod-guard && git -C dod-guard checkout be038f7
cd ..
python3 -m venv venv && venv/bin/pip install pytest==8.4.2
python3 -I path/to/prog/scripts/done-gate-comparison/compare.py \
  --plugins plugins --prog-plugin path/to/prog/plugins/prog-claude \
  --prog "$(command -v prog)" --python venv/bin/python --out report.md
```

The checked-in [report](report.md) was run in `python:3.13-slim-bookworm`
(arm64) with Node 18, jq 1.6, git and pytest 8.4.2. It uses prog built from
this branch, which includes the virtualenv shim fix and both stop-gate rules
in the CHANGELOG's Unreleased section. [report-0.2.0.md](report-0.2.0.md) is
the same run with the released 0.2.0 Linux binary (amd64), and its rival
columns are identical.

## Reading the result

- **prog** is the only gate that refuses every test cheat: deleted, skipped
  and swapped tests, and narrower runs. It never blocks on missing evidence,
  so a claim with no test run passes, as the `stop_gate` setting's description
  says. It still blocks a README edit made after the green run, by design,
  because tests can read docs. Released 0.2.0 on Linux also blocks the real
  fix. Its `python` shim broke the venv, so pytest could not be imported,
  which is the bug this branch fixes. Its "run fewer tests" block in that run
  is an artifact of the same bug, because every run failed to import pytest.
- **done-needs-proof** matches claim words to a passing check run in the same
  turn after the last edit. It never blocks correct work. It cannot see what
  the test run left out.
- **show-me-the-work** blocks every success claim until the message carries
  its "Hypothesis / Evidence / Rejected" markers. It blocks cheats and correct
  work alike, for that reason.
- **llm-dark-patterns** asks for concrete evidence in the closing message
  itself. It blocks correct work that does not quote any.
- **dod-guard** reruns the suite at Stop. In this setup its suite runner
  reported `exit_code: 0` for a failing pytest run, so its pipeline passed
  the wrong fix. It blocked the skip only because its test-tautology detector
  flagged the edited test file (`test_tautology=1`).
