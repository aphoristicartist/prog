#!/usr/bin/env python3
"""Example harness consumer for `prog session show --readiness` JSON.

This script owns the gate decision. `prog` only reports evidence-backed
readiness data. Malformed or internally inconsistent input fails closed.
"""

import json
import sys

MAX_BLOCKERS = 8
MAX_BLOCKER_CHARS = 512


def emit(decision, reason, blockers, omitted=0):
    json.dump(
        {
            "schema": "prog.example.readiness-consumer.v1",
            "decision": decision,
            "reason": reason,
            "blockers": blockers,
            "omitted_blockers": omitted,
        },
        sys.stdout,
        sort_keys=True,
        separators=(",", ":"),
    )
    sys.stdout.write("\n")


try:
    report = json.load(sys.stdin)
except (json.JSONDecodeError, UnicodeDecodeError):
    emit("block", "invalid_json", [])
    raise SystemExit(2)

if not isinstance(report, dict) or report.get("schema") != "prog.verification":
    emit("block", "unexpected_schema", [])
    raise SystemExit(2)

configured = report.get("configured")
ready = report.get("ready")
blockers = report.get("blockers")
if (
    not isinstance(configured, bool)
    or not isinstance(ready, bool)
    or not isinstance(blockers, list)
    or any(not isinstance(blocker, str) for blocker in blockers)
):
    emit("block", "invalid_contract", [])
    raise SystemExit(2)

bounded = [blocker[:MAX_BLOCKER_CHARS] for blocker in blockers[:MAX_BLOCKERS]]
omitted = max(0, len(blockers) - len(bounded))
if configured and ready and not blockers:
    emit("pass", "all_required_obligations_satisfied", [])
    raise SystemExit(0)

if ready and (not configured or blockers):
    emit("block", "inconsistent_readiness_report", bounded, omitted)
    raise SystemExit(2)

reason = "not_configured" if not configured else "required_obligations_unsatisfied"
emit("block", reason, bounded, omitted)
raise SystemExit(1)
