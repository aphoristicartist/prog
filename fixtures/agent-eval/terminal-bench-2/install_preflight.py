"""Prepare official Harbor install-only config and check its setup receipts.

This does not run trials or grade benchmark outcomes. Execute it in the pinned
Harbor environment so receipt normalization uses Harbor's own config models.
"""

import argparse
import json
from datetime import datetime
from pathlib import Path

import yaml
from harbor.models.job.config import JobConfig
from harbor.models.trial.config import AgentConfig


FIXTURES = Path(__file__).resolve().parent


def require(condition, message):
    if not condition:
        raise ValueError(message)


def prepare(binary, output):
    binary = binary.resolve(strict=True)
    require(binary.is_file(), "prog binary must be a file")
    config = yaml.safe_load((FIXTURES / "pilot-raw-first.yaml").read_text())
    config["job_name"] = "install-only"
    config["jobs_dir"] = str(output.resolve() / "jobs")
    config["install_only"] = True
    config["verifier"] = {"disable": True}
    config["environment"].pop("env", None)
    config["datasets"][0]["task_names"] = config["datasets"][0]["task_names"][:1]
    config["agents"][1]["kwargs"]["prog_binary_path"] = str(binary)
    # Resolve defaults and legacy orchestrator fields through the official model.
    resolved = JobConfig.model_validate(config)
    output.mkdir(parents=True, exist_ok=False)
    path = output / "config.json"
    path.write_text(resolved.model_dump_json(indent=2))
    return {"config": str(path.resolve()), "claim_eligible": False}


def completed(phase):
    require(isinstance(phase, dict), "missing setup phase")
    start = datetime.fromisoformat(phase["started_at"])
    finish = datetime.fromisoformat(phase["finished_at"])
    require(finish >= start, "unfinished or invalid setup phase")


def check(config_path):
    expected = JobConfig.model_validate_json(config_path.read_text())
    require(expected.install_only is True, "configuration permits model execution")
    require(expected.verifier.disable is True, "configuration permits grading")
    require(len(expected.agents) == 2, "expected exactly two installation arms")
    require(len(expected.datasets) == 1, "expected one official dataset")
    dataset = expected.datasets[0]
    require(len(dataset.task_names) == 1, "expected one installation task")
    task = dataset.task_names[0]
    job_dir = expected.jobs_dir / expected.job_name
    actual = JobConfig.model_validate_json((job_dir / "config.json").read_text())
    require(actual == expected, "Harbor job configuration differs from preflight")
    job = json.loads((job_dir / "result.json").read_text())
    completed(job)
    require(job["n_total_trials"] == 2, "expected exactly two setup receipts")
    stats = job["stats"]
    require(stats["n_completed_trials"] == 2, "installations did not complete")
    for key in (
        "n_errored_trials", "n_running_trials", "n_pending_trials",
        "n_cancelled_trials", "n_retries",
    ):
        require(stats[key] == 0, f"nonzero {key}")
    for key in ("n_input_tokens", "n_cache_tokens", "n_output_tokens", "cost_usd"):
        require(stats[key] is None, f"unexpected provider usage: {key}")
    paths = sorted(job_dir.glob("*/result.json"))
    require(len(paths) == 2, "missing or additional setup receipts")
    remaining = list(expected.agents)
    arms = []
    for path in paths:
        trial = json.loads(path.read_text())
        config = trial["config"]
        require(config["install_only"] is True, "trial permits model execution")
        require(config["verifier"]["disable"] is True, "trial permits grading")
        require(config["environment"] == expected.environment.model_dump(mode="json"), "unexpected environment settings")
        for key in (
            "exception_info", "agent_execution", "verifier", "agent_result",
            "verifier_result", "step_results",
        ):
            # A missing field is not evidence that execution was skipped.
            require(trial[key] is None, f"unexpected trial activity or error: {key}")
        completed(trial)
        completed(trial["environment_setup"])
        completed(trial["agent_setup"])
        source = config["task"]
        repo, commit = dataset.repo.split("@")
        require(source["path"] == task, "unexpected task")
        require(source["git_url"] == f"https://github.com/{repo}.git", "unexpected repository")
        require(source["git_commit_id"] == commit, "unexpected benchmark commit")
        agent = AgentConfig.model_validate(config["agent"])
        require(agent in remaining, "duplicate or unexpected installation arm/settings")
        remaining.remove(agent)
        arms.append({"agent": trial["agent_info"]["name"], "receipt": str(path)})
    require(not remaining, "missing installation arm")
    return {
        "status": "installation_preflight_passed",
        "claim_eligible": False,
        "task_id": task,
        "model_execution": "disabled",
        "verification": "disabled",
        "arms": arms,
        "scope": "Harbor reports successful setup; no benchmark outcome or usage claim",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prep = commands.add_parser("prepare", help="derive one-task install-only configuration")
    prep.add_argument("--binary", required=True, type=Path)
    prep.add_argument("--output", required=True, type=Path)
    verify = commands.add_parser("check", help="check official setup receipts, not task grades")
    verify.add_argument("--config", required=True, type=Path)
    args = parser.parse_args()
    try:
        result = prepare(args.binary, args.output) if args.command == "prepare" else check(args.config)
    except (KeyError, TypeError, ValueError, OSError) as error:
        print(json.dumps({"status": "preflight_failed", "claim_eligible": False, "error": str(error)}))
        raise SystemExit(1) from error
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
