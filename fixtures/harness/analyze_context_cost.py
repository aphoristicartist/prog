#!/usr/bin/env python3
"""Offline accounting over existing evaluation ledgers; never executes a source."""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import sys


SCHEMA = "prog.context_cost_analysis.v1"
MAX_INPUT_BYTES = 64 * 1024 * 1024
OPERATIONS = {"run", "observe", "call", "capture", "inspect", "search", "find",
              "findings", "evidence", "expand", "paths", "status", "session", "file_read",
              "meta", "help", "init"}
RETRIEVAL = {"evidence", "expand"}
STRATEGIES = {"raw_context", "head_tail_truncation", "native_field_selection",
              "rtk_grep_filter", "broad_log_search", "file_capture_search",
              "caveman_terse_output", "prog_envelope_only", "prog_retrieve",
              "prog_repeated_cache", "paths", "findings", "inspect", "baseline",
              "cli", "registered"}
OUTCOMES = {"evidence_available", "insufficient_evidence", "insufficient",
            "not_attempted", "passed", "failed"}
DECODER = json.JSONDecoder()


def encoded(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode()


def size(value):
    return len(value.encode("utf-8"))


def number(value):
    return value if type(value) is int and value >= 0 else None


def skip_space(text, position):
    while position < len(text) and text[position].isspace():
        position += 1
    return position


def spans(text, position=0, key=None, depth=0):
    """Retain actual JSON value byte spans, including whitespace and escapes."""
    if depth > 64:
        raise ValueError("body depth exceeds analysis bound")
    position = skip_space(text, position)
    value, end = DECODER.raw_decode(text, position)
    yield key, value, position, end, depth
    if isinstance(value, dict):
        cursor = skip_space(text, position + 1)
        while cursor < end - 1:
            child_key, cursor = DECODER.raw_decode(text, cursor)
            cursor = skip_space(text, cursor)
            if text[cursor] != ":":
                raise ValueError("invalid object separator")
            cursor = skip_space(text, cursor + 1)
            yield from spans(text, cursor, child_key, depth + 1)
            _, cursor = DECODER.raw_decode(text, cursor)
            cursor = skip_space(text, cursor)
            if text[cursor] == ",":
                cursor = skip_space(text, cursor + 1)
    elif isinstance(value, list):
        cursor = skip_space(text, position + 1)
        while cursor < end - 1:
            child_key = "finding_record" if key == "findings" else None
            yield from spans(text, cursor, child_key, depth + 1)
            _, cursor = DECODER.raw_decode(text, cursor)
            cursor = skip_space(text, cursor)
            if text[cursor] == ",":
                cursor = skip_space(text, cursor + 1)


def operation(argv):
    # Parse only the known leading prog surface; never inspect child argv.
    if argv and Path(str(argv[0])).name == "prog-run.sh":
        return "capture"
    if not argv or Path(str(argv[0])).name != "prog":
        return "other"
    takes_value = {"--dir", "--budget-bytes", "--budget-tokens", "--lens-dir"}
    index = 1
    while index < len(argv):
        token = argv[index]
        if token in takes_value:
            index += 2
        elif token == "--pretty":
            index += 1
        elif token in {"--help", "-h"}:
            return "help"
        elif token.startswith("--") and "=" in token:
            index += 1
        else:
            return token if token in OPERATIONS else "other"
    return "other"


def field_group(key):
    if key in {"findings", "excerpt", "citations", "data_preview", "evidence_ref"}:
        return key
    if key in {"warnings", "error", "errors"}:
        return "notices_and_errors"
    if key in {"instructions", "commands", "next_actions", "guidance"}:
        return "instructions_and_navigation"
    return "metadata_and_other"


def evidence_identity(body):
    if not isinstance(body, dict) or body.get("error"):
        return None
    ref = body.get("evidence_ref")
    if not isinstance(ref, dict) and isinstance(body.get("data_preview"), dict):
        ref = body["data_preview"].get("evidence_ref")
    if not isinstance(ref, dict) or ref.get("availability") != "recoverable":
        return None
    identity = ref.get("observation_id") or ref.get("cursor")
    if not isinstance(identity, str) or not isinstance(ref.get("path"), str):
        return None
    return encoded([identity, ref["path"], body.get("line_range"), body.get("byte_range")])


def summarize(steps, *, strategy="other", outcome="unavailable", declared_bytes=None,
              declared_calls=None, context=None):
    fields = Counter()
    operations = Counter()
    seen_bodies, seen_excerpts, seen_findings, consulted = set(), set(), set(), set()
    profiled = repeated_bodies = repeated_excerpts = repeated_findings = 0
    requests = failed_reads = unknown_reads = completed_reads = repeated_reads = missing_bodies = 0
    recorded = 0
    missing_sizes = 0
    stderr_bytes = 0
    missing_stderr = 0
    inconsistent = False
    for step in steps:
        op = step.get("operation") or operation(step.get("argv", step.get("command", [])))
        if op not in OPERATIONS:
            op = "other"
        body_text = step.get("stdout")
        body_text = body_text if isinstance(body_text, str) else None
        n = number(step.get("stdout_bytes", step.get("response_bytes")))
        if body_text is not None:
            inconsistent |= n is not None and n != size(body_text)
            n = size(body_text)
        if n is None:
            missing_sizes += 1
        else:
            recorded += n
            operations[op] += n
        err = step.get("stderr")
        err_n = size(err) if isinstance(err, str) else number(step.get("stderr_bytes"))
        if err_n is None:
            missing_stderr += 1
        else:
            stderr_bytes += err_n
        body = None
        if body_text is None:
            missing_bodies += 1
        else:
            profiled += size(body_text)
            digest = hashlib.sha256(body_text.encode()).digest()
            if digest in seen_bodies:
                repeated_bodies += size(body_text)
            seen_bodies.add(digest)
            try:
                body = json.loads(body_text)
                nodes = list(spans(body_text))
            except (ValueError, RecursionError):
                nodes = []
                body = None
            top_bytes = 0
            for key, value, start, end, depth in nodes:
                measured = size(body_text[start:end])
                if depth == 1 and isinstance(body, dict):
                    fields[field_group(key)] += measured
                    top_bytes += measured
                if key == "excerpt" and value not in (None, "", [], {}):
                    fingerprint = hashlib.sha256(encoded(value)).digest()
                    if fingerprint in seen_excerpts:
                        repeated_excerpts += measured
                    seen_excerpts.add(fingerprint)
                if key == "finding_record":
                    fingerprint = hashlib.sha256(encoded(value)).digest()
                    if fingerprint in seen_findings:
                        repeated_findings += measured
                    seen_findings.add(fingerprint)
            fields["json_framing" if isinstance(body, dict) and nodes else "non_object_or_unparsed"] += size(body_text) - top_bytes
        if op in RETRIEVAL:
            requests += 1
            code = step.get("exit_code")
            code = code if type(code) is int else None
            has_error = isinstance(body, dict) and bool(body.get("error"))
            if step.get("is_error") is True or code is not None and code != 0 or has_error:
                failed_reads += 1
            elif (code == 0 or step.get("is_error") is False) and evidence_identity(body):
                identity = evidence_identity(body)
                completed_reads += 1
                repeated_reads += identity in consulted
                consulted.add(identity)
            else:
                unknown_reads += 1
    declared_bytes = number(declared_bytes)
    if declared_bytes is not None and not missing_sizes:
        inconsistent |= declared_bytes != recorded
    total = None if missing_sizes else recorded
    return {
        "strategy": strategy if strategy in STRATEGIES else "other",
        "outcome": outcome if outcome in OUTCOMES else "unavailable",
        "recorded_response_bytes": total,
        "declared_response_bytes": declared_bytes,
        "known_response_bytes": recorded,
        "recorded_stderr_bytes": None if missing_stderr else stderr_bytes,
        "declared_tool_calls": number(declared_calls),
        "ledger_steps": len(steps),
        "known_response_bytes_by_operation": dict(sorted(operations.items())),
        "profiled_response_bytes": profiled,
        "unprofiled_response_bytes": total - profiled if total is not None and total >= profiled else None,
        "known_bytes_by_field": dict(sorted(fields.items())),
        "missing_body_steps": missing_bodies,
        "missing_size_steps": missing_sizes,
        "inconsistent_byte_counts": inconsistent,
        "retrieval_requests": requests,
        "failed_retrievals": failed_reads,
        "unclassified_retrievals": unknown_reads,
        "completed_retrievals": None if unknown_reads else completed_reads,
        "known_completed_retrievals": completed_reads,
        "repeated_retrievals": None if unknown_reads else repeated_reads,
        "known_repeated_retrievals": repeated_reads,
        "unique_consulted_refs": None if unknown_reads else len(consulted),
        "known_unique_consulted_refs": len(consulted),
        "repeated_response_bytes": repeated_bodies,
        "repeated_excerpt_value_bytes": repeated_excerpts,
        "repeated_finding_record_bytes": repeated_findings,
        "declared_context_components": context or {},
    }


CONTEXT_KEYS = {"instructions", "assembled_sections", "assembled_tool_schemas_json",
                "tool_requests_json", "tool_presentations_json", "cli_stdout", "cli_stderr",
                "exported_file_reads", "cli_argv_json"}


def installed(report, strategy):
    context = {key: value for key, value in report.get("context_bytes", {}).items()
               if key in CONTEXT_KEYS and number(value) is not None}
    rows = [summarize(report.get("commands", []), strategy=strategy,
                      outcome="passed" if report.get("passed") is True else "failed",
                      declared_bytes=report.get("stdout_bytes"), context=context)]
    if "deliveries" in report:
        steps = []
        wrapper_bytes = 0
        request_bytes = 0
        notice_bytes = 0
        for delivery in report["deliveries"]:
            request, result = delivery["request"], delivery["result"]
            presentation = {key: result[key] for key in ("isError", "content", "additionalContexts") if key in result}
            wrapper_bytes += len(encoded(presentation))
            request_bytes += len(encoded(request))
            if "additionalContexts" in result:
                notice_bytes += len(encoded(result["additionalContexts"]))
            texts = [block["text"] for block in result.get("content", []) if isinstance(block.get("text"), str)]
            name = request.get("name")
            op = {"prog_observe": "capture", "prog_status": "status"}.get(name, "other")
            if name == "prog_evidence":
                op = request.get("arguments", {}).get("mode", "evidence")
                op = "evidence" if op == "exact" else op
            steps.append({"operation": op, "stdout": texts[0] if len(texts) == 1 else None,
                          "stdout_bytes": sum(map(size, texts)), "is_error": result.get("isError")})
        presentations = summarize(steps, strategy="registered", outcome=rows[0]["outcome"])
        presentations["presentation_json_bytes"] = wrapper_bytes
        presentations["request_json_bytes"] = request_bytes
        presentations["additional_context_json_bytes"] = notice_bytes
        presentations["presentation_wrapper_bytes"] = wrapper_bytes - presentations["known_response_bytes"]
        presentations["scope"] = "registered_presentations"
        rows.append(presentations)
    rows[0]["scope"] = "cli_ledger"
    rows[0]["available_instruction_file_bytes"] = number(report.get("instruction_file_bytes"))
    if "file_reads" in report:
        reads = [{"operation": "file_read", "stdout": row.get("content"),
                  "stdout_bytes": row.get("bytes"), "stderr": ""} for row in report["file_reads"]]
        row = summarize(reads, strategy=strategy, outcome=rows[0]["outcome"],
                        declared_bytes=report.get("file_read_bytes"), declared_calls=0)
        row["scope"] = "exported_file_reads"
        rows.append(row)
    return rows


def analyze(document):
    if isinstance(document, list):
        rows = []
        for index, source in enumerate(document):
            row = summarize(source.get("steps", []), strategy=source.get("strategy"),
                            outcome=source.get("outcome"), declared_bytes=source.get("response_bytes"),
                            declared_calls=source.get("tool_calls"))
            row["source_row_index"] = index
            row["task_mode"] = source.get("task_mode") if source.get("task_mode") in {
                "known_path_recoverability", "deterministic_discovery"} else None
            for field in ("counterexample", "correct", "evidence_available", "available"):
                row["graded_" + field] = source.get(field) if type(source.get(field)) is bool else None
            row["reported_artifact_bytes"] = number(source.get("artifact_bytes"))
            row["reported_expansion_count"] = number(source.get("expansion_count"))
            rows.append(row)
        return rows
    schema = document.get("schema")
    if schema == "prog.evidence_cli_eval.v1":
        return analyze(document["rows"])
    if schema == "prog.evidence_acquisition_eval":
        rows = []
        for index, scenario in enumerate(document["scenarios"]):
            for strategy in ("baseline", "findings", "inspect"):
                row = summarize([], strategy=strategy,
                                outcome="evidence_available" if scenario.get("correct") is True else "insufficient",
                                declared_calls=scenario.get(strategy + "_tool_calls"))
                row.update(recorded_response_bytes=None, unprofiled_response_bytes=None,
                           unique_consulted_refs=None, retrieval_requests=None, failed_retrievals=None,
                           completed_retrievals=None, known_completed_retrievals=None,
                           repeated_retrievals=None, known_repeated_retrievals=None,
                           known_unique_consulted_refs=None, unclassified_retrievals=None,
                           scope="aggregate_only", reported_approximate_tokens=number(scenario.get(strategy + "_output_tokens")))
                row["source_scenario_index"] = index
                rows.append(row)
        return rows
    if schema in {"prog.installed_coding_loop_smoke", "prog.registered_coding_loop_smoke"}:
        return installed(document, "cli" if schema == "prog.installed_coding_loop_smoke" else "registered")
    if schema == "prog.installed_facade_comparison":
        return installed(document["cli"], "cli") + installed(document["registered"], "registered")
    raise ValueError("unsupported ledger schema")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ledgers", nargs="+", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    sources = []
    try:
        for path in args.ledgers:
            with path.open("rb") as source:
                raw = source.read(MAX_INPUT_BYTES + 1)
            if len(raw) > MAX_INPUT_BYTES:
                raise ValueError("ledger exceeds analysis bound")
            sources.append({"sha256": hashlib.sha256(raw).hexdigest(), "rows": analyze(json.loads(raw))})
        report = {"schema": SCHEMA, "sources": sources, "limits": [
            "Offline recorded disclosure costs, not provider usage or task-success causation.",
            "Missing bodies, reference identities and sizes remain unavailable; approximate tokens are not inverted into bytes.",
            "Repeated-byte diagnostics overlap field totals and each other; they are not measured savings or evidence that content is unnecessary.",
            "CLI ledgers and registered presentations are separate views; do not add nested transport costs twice.",
            "No raw prose, argv, file paths, cursors or excerpts are exported; outcomes are joined from the existing grader.",
        ]}
        rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
        if args.output:
            args.output.write_text(rendered)
        else:
            sys.stdout.write(rendered)
    except (OSError, ValueError, TypeError, KeyError, AttributeError, RecursionError):
        print(json.dumps({"error": "invalid, unsupported, or unreadable evaluation ledger"}))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
