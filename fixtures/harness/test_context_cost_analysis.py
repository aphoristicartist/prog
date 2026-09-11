import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import analyze_context_cost as analysis


ROOT = Path(__file__).resolve().parents[2]


def evidence():
    return {"schema": "prog.evidence", "excerpt": "private diagnostic é\n",
            "citations": [{"excerpt": "private diagnostic é\n"}],
            "evidence_ref": {"cursor": "pc1_private", "path": "/private",
                             "availability": "recoverable"}}


def step(body, code=0):
    return {"command": ["prog", "evidence", "pc1_private", "--path", "/private"],
            "stdout": json.dumps(body, ensure_ascii=False, indent=3), "exit_code": code,
            "stderr": ""}


class ContextCostAnalysis(unittest.TestCase):
    def test_repeated_presentations_are_charged_and_refs_are_unique(self):
        first = step(evidence())
        report = analysis.summarize([first, first], outcome="insufficient")
        n = len(first["stdout"].encode())
        self.assertEqual(report["recorded_response_bytes"], 2 * n)
        self.assertEqual(report["repeated_response_bytes"], n)
        self.assertEqual(sum(report["known_bytes_by_field"].values()), 2 * n)
        self.assertEqual(report["retrieval_requests"], 2)
        self.assertEqual(report["unique_consulted_refs"], 1)
        self.assertEqual(report["completed_retrievals"], 2)
        self.assertEqual(report["repeated_retrievals"], 1)
        self.assertEqual(report["outcome"], "insufficient")
        excerpt_size = len(json.dumps(evidence()["excerpt"], ensure_ascii=False).encode())
        self.assertEqual(report["repeated_excerpt_value_bytes"], 3 * excerpt_size)
        for private in ("private diagnostic", "pc1_private", "/private"):
            self.assertNotIn(private, json.dumps(report))

    def test_failed_and_denied_reads_are_not_consulted(self):
        denied = {"error": {"kind": "policy_denied", "message": "private reason"}}
        report = analysis.summarize([step(evidence()), step(denied, 1), step(denied)])
        self.assertEqual(report["retrieval_requests"], 3)
        self.assertEqual(report["failed_retrievals"], 2)
        self.assertEqual(report["completed_retrievals"], 1)
        self.assertEqual(report["repeated_retrievals"], 0)
        self.assertEqual(report["unique_consulted_refs"], 1)
        self.assertGreater(report["known_bytes_by_field"]["notices_and_errors"], 0)

    def test_repeated_finding_records_remain_in_the_full_cost(self):
        finding = {"fingerprint": "private-fingerprint", "path": "/private", "reason": "private reason"}
        body = {"findings": [finding, finding]}
        text = json.dumps(body, ensure_ascii=False, separators=(",", ":"))
        row = analysis.summarize([{"stdout": text}])
        self.assertEqual(row["repeated_finding_record_bytes"], len(analysis.encoded(finding)))
        self.assertEqual(sum(row["known_bytes_by_field"].values()), len(text.encode()))
        self.assertNotIn("private reason", json.dumps(row))

    def test_missing_body_does_not_invent_field_cost_or_success(self):
        report = analysis.summarize([{"command": ["prog", "expand", "$CURSOR"],
                                      "stdout_bytes": 101, "exit_code": 0}])
        self.assertEqual(report["recorded_response_bytes"], 101)
        self.assertEqual(report["unprofiled_response_bytes"], 101)
        self.assertEqual(report["known_bytes_by_field"], {})
        self.assertEqual(report["unclassified_retrievals"], 1)
        self.assertIsNone(report["unique_consulted_refs"])
        self.assertIsNone(report["completed_retrievals"])
        self.assertIsNone(report["repeated_retrievals"])
        self.assertEqual(report["known_completed_retrievals"], 0)
        self.assertIsNone(report["recorded_stderr_bytes"])

    def test_incomplete_and_conflicting_ledgers_preserve_unknowns(self):
        row = analysis.summarize([{"command": ["prog", "run"]}], declared_bytes=99)
        self.assertIsNone(row["recorded_response_bytes"])
        self.assertEqual(row["declared_response_bytes"], 99)
        row = analysis.summarize([step(evidence())], declared_bytes=1)
        self.assertTrue(row["inconsistent_byte_counts"])
        self.assertGreater(row["recorded_response_bytes"], row["declared_response_bytes"])

    def test_zero_calls_do_not_become_a_success_claim(self):
        row = analysis.summarize([], outcome="not_attempted", declared_bytes=0, declared_calls=0)
        self.assertEqual(row["recorded_response_bytes"], 0)
        self.assertEqual(row["outcome"], "not_attempted")
        self.assertEqual(row["unique_consulted_refs"], 0)
        self.assertEqual(row["completed_retrievals"], 0)
        self.assertEqual(row["repeated_retrievals"], 0)
        self.assertNotIn("unused", json.dumps(row))

    def test_host_duplicate_internal_value_is_not_delivered_twice(self):
        body = evidence()
        result = {"isError": False, "content": [{"type": "text", "text": json.dumps(body)}],
                  "value": body, "additionalContexts": [{"text": "private notice"}]}
        delivery = {"request": {"name": "prog_evidence", "arguments": {"mode": "exact"}},
                    "result": result}
        report = {"schema": "prog.registered_coding_loop_smoke", "passed": True,
                  "commands": [], "stdout_bytes": 0, "deliveries": [delivery, delivery]}
        rows = analysis.analyze(report)
        presented = rows[1]
        expected = {key: result[key] for key in ("isError", "content", "additionalContexts")}
        self.assertEqual(presented["presentation_json_bytes"], 2 * len(analysis.encoded(expected)))
        self.assertEqual(presented["unique_consulted_refs"], 1)
        self.assertGreater(presented["presentation_wrapper_bytes"], 0)
        self.assertEqual(presented["additional_context_json_bytes"], 2 * len(analysis.encoded(result["additionalContexts"])))
        self.assertNotIn("private notice", json.dumps(rows))

    def test_operation_detection_does_not_read_nested_child_argv(self):
        self.assertEqual(analysis.operation(["/tmp/bin/prog", "--dir", "expand", "run", "--", "prog", "search"]), "run")
        self.assertEqual(analysis.operation(["python3", "-c", "private code", "inspect"]), "other")
        self.assertEqual(analysis.operation(["/tmp/.agents/prog-hooks/prog-run.sh", "cargo", "test"]), "capture")
        self.assertEqual(analysis.operation(["prog", "--pretty", "--lens-dir", "search", "meta"]), "meta")
        self.assertEqual(analysis.operation(["prog", "--help"]), "help")

    def test_non_json_and_deep_bodies_are_counted_without_fabricating_fields(self):
        for text in ("plain private output\n", "[" * 70 + "0" + "]" * 70):
            row = analysis.summarize([{"stdout": text}])
            self.assertEqual(sum(row["known_bytes_by_field"].values()), len(text.encode()))
            self.assertEqual(row["profiled_response_bytes"], len(text.encode()))

    def test_actual_fixture_families_have_no_dropped_rows_or_inverted_tokens(self):
        for filename in ("competitive-baseline-metrics.json", "evidence-cli-metrics.json"):
            document = json.loads((ROOT / "fixtures/evals" / filename).read_text())
            source_rows = document if isinstance(document, list) else document["rows"]
            rows = analysis.analyze(document)
            self.assertEqual(len(rows), len(source_rows))
            self.assertEqual(sum(r["recorded_response_bytes"] for r in rows), sum(r["response_bytes"] for r in source_rows))
            self.assertFalse(any(r["inconsistent_byte_counts"] for r in rows))
            self.assertTrue(all(r["profiled_response_bytes"] == 0 for r in rows))
            for index, (source, row) in enumerate(zip(source_rows, rows)):
                self.assertEqual(row["source_row_index"], index)
                if "counterexample" in source:
                    self.assertEqual(row["graded_counterexample"], source["counterexample"])
                self.assertEqual(row["reported_artifact_bytes"], source.get("artifact_bytes"))
                self.assertEqual(row["reported_expansion_count"], source.get("expansion_count"))
        document = json.loads((ROOT / "fixtures/evals/evidence-acquisition-metrics.json").read_text())
        rows = analysis.analyze(document)
        self.assertEqual(len(rows), 3 * len(document["scenarios"]))
        self.assertTrue(all(r["recorded_response_bytes"] is None for r in rows))
        self.assertTrue(all(r["completed_retrievals"] is None for r in rows))
        self.assertTrue(all(r["repeated_retrievals"] is None for r in rows))

    def test_cli_is_deterministic_and_errors_are_json_without_input_echo(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "input.json"
            source.write_text(json.dumps([{"steps": [step(evidence())], "outcome": "insufficient"}]))
            command = [sys.executable, str(Path(analysis.__file__)), str(source)]
            first = subprocess.run(command, capture_output=True, check=True).stdout
            self.assertEqual(first, subprocess.run(command, capture_output=True, check=True).stdout)
            self.assertNotIn(b"private diagnostic", first)
            source.write_text('{"schema":"private unknown schema"}')
            failed = subprocess.run(command, capture_output=True)
            self.assertEqual(failed.returncode, 1)
            self.assertEqual(failed.stderr, b"")
            self.assertIn("error", json.loads(failed.stdout))
            self.assertNotIn(b"private unknown", failed.stdout)


if __name__ == "__main__":
    unittest.main()
