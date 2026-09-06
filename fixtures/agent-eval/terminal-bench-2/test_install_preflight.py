"""Negative controls for setup claims; these are not benchmark task graders."""

import copy
import json
import tempfile
import unittest
from pathlib import Path

from harbor.models.job.config import JobConfig

from install_preflight import check, prepare


class InstallPreflightTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        binary = root / "prog"
        binary.write_bytes(b"test fixture, never executed")
        self.config = Path(prepare(binary, root / "preflight")["config"])
        expected = JobConfig.model_validate_json(self.config.read_text())
        self.job = expected.jobs_dir / expected.job_name
        self.job.mkdir(parents=True)
        (self.job / "config.json").write_text(self.config.read_text())
        # Minimal successful Harbor receipt. No process/model is launched here.
        phase = {"started_at": "2026-09-06T00:00:00Z", "finished_at": "2026-09-06T00:00:01Z"}
        result = dict(phase, n_total_trials=2, stats={
            "n_completed_trials": 2, "n_errored_trials": 0,
            "n_running_trials": 0, "n_pending_trials": 0,
            "n_cancelled_trials": 0, "n_retries": 0,
            "n_input_tokens": None, "n_cache_tokens": None,
            "n_output_tokens": None, "cost_usd": None,
        })
        self.write(self.job / "result.json", result)
        repo, commit = expected.datasets[0].repo.split("@")
        for index, agent in enumerate(expected.agents):
            trial = dict(phase, config={
                "install_only": True,
                "verifier": {"disable": True},
                "environment": expected.environment.model_dump(mode="json"),
                "agent": agent.model_dump(mode="json"),
                "task": {"path": expected.datasets[0].task_names[0],
                         "git_url": f"https://github.com/{repo}.git",
                         "git_commit_id": commit},
            }, agent_info={"name": ["claude-code", "claude-code-prog"][index]},
                environment_setup=phase, agent_setup=phase,
                exception_info=None, agent_execution=None, verifier=None,
                agent_result=None, verifier_result=None, step_results=None)
            self.write(self.job / str(index) / "result.json", trial)

    @staticmethod
    def write(path, value):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(value))

    def test_success_is_only_an_installation_claim(self):
        report = check(self.config)
        self.assertEqual(report["status"], "installation_preflight_passed")
        self.assertFalse(report["claim_eligible"])
        self.assertEqual(report["model_execution"], "disabled")
        self.assertEqual(report["verification"], "disabled")

    def test_error_or_incomplete_job_cannot_pass(self):
        path = self.job / "result.json"
        original = json.loads(path.read_text())
        for field, value in (("n_errored_trials", 2), ("n_completed_trials", 1),
                             ("n_retries", 1), ("cost_usd", 0.0)):
            with self.subTest(field=field):
                changed = copy.deepcopy(original)
                changed["stats"][field] = value
                self.write(path, changed)
                with self.assertRaises(ValueError):
                    check(self.config)

    def test_missing_receipt_and_duplicate_arm_cannot_pass(self):
        first = self.job / "0/result.json"
        second = self.job / "1/result.json"
        second.write_text(first.read_text())
        with self.assertRaisesRegex(ValueError, "duplicate"):
            check(self.config)
        second.unlink()
        with self.assertRaisesRegex(ValueError, "missing"):
            check(self.config)

    def test_activity_error_and_missing_fields_cannot_pass(self):
        path = self.job / "0/result.json"
        original = json.loads(path.read_text())
        for field in ("agent_execution", "verifier", "agent_result", "verifier_result",
                      "exception_info", "step_results"):
            for missing in (False, True):
                with self.subTest(field=field, missing=missing):
                    changed = copy.deepcopy(original)
                    if missing:
                        del changed[field]
                    else:
                        changed[field] = {}
                    self.write(path, changed)
                    with self.assertRaises((ValueError, KeyError)):
                        check(self.config)

    def test_changed_config_source_pin_and_unfinished_setup_cannot_pass(self):
        path = self.job / "0/result.json"
        original = json.loads(path.read_text())
        mutations = [
            lambda r: r["config"].update(install_only=False),
            lambda r: r["config"]["verifier"].update(disable=False),
            lambda r: r["config"]["task"].update(git_commit_id="wrong-commit"),
            lambda r: r["config"]["agent"]["kwargs"].update(version="wrong-version"),
            lambda r: r["agent_setup"].update(finished_at=None),
        ]
        for mutate in mutations:
            changed = copy.deepcopy(original)
            mutate(changed)
            self.write(path, changed)
            with self.assertRaises((ValueError, TypeError)):
                check(self.config)


if __name__ == "__main__":
    unittest.main()
