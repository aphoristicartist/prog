#!/usr/bin/env python3
"""Exercise an installed skill/CLI coding loop, without an agent or provider."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile


BUDGET_BYTES = 16 * 1024
SUITE_ARGV = ["cargo", "test", "-p", "installed-loop", "--lib", "--color", "never"]
ENV_MARKER = "installed environment with spaces and 'quotes'"
SOURCE = r'''pub fn add(a: i64, b: i64) -> i64 { a + b + 1 }
#[cfg(test)] mod tests {
    fn record(name: &str) {
        use std::io::Write;
        let cwd = std::env::current_dir().unwrap();
        let marker = std::env::var("PROG_LOOP_ENV").unwrap();
        assert_eq!(marker, "installed environment with spaces and 'quotes'");
        let mut log = std::fs::OpenOptions::new().create(true).append(true)
            .open(std::env::var("PROG_LOOP_LOG").unwrap()).unwrap();
        let line = format!("{name}\t{}\t{marker}\n", cwd.display());
        log.write_all(line.as_bytes()).unwrap();
    }
    #[test] fn computes_total() {
        record("computes_total");
        for i in 0..200 { println!("diagnostic {i}: {}", "x".repeat(160)); }
        assert_eq!(super::add(2, 3), 5);
    }
    #[test] fn independent_check() {
        record("independent_check");
        assert_eq!(2 * 3, 6);
    }
}
'''


def digest(value):
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode()).hexdigest()


class Loop:
    def __init__(self, root, binary):
        self.root = root
        self.project = root / "coding project 'with spaces'"
        self.project.mkdir()
        (root / "bin").mkdir()
        self.prog = root / "bin" / "prog"
        shutil.copy2(binary, self.prog)
        self.original_binary = binary
        self.env = dict(os.environ)
        # Ambient integration configuration must not reach the contributor's
        # store, change the response budget, or supply checkout-only lenses.
        for key in list(self.env):
            if key.startswith(("PROG_", "GIT_")):
                del self.env[key]
        self.env.update(
            PATH=str(root / "bin") + os.pathsep + os.environ["PATH"],
            PROG_DIR=str(root / "store"),
            PROG_BUDGET_BYTES=str(BUDGET_BYTES),
            PROG_LOOP_ENV=ENV_MARKER,
            PROG_LOOP_LOG=str(root / "executions.tsv"),
            CARGO_TARGET_DIR=str(root / "target"),
            CARGO_TERM_COLOR="never",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
        )
        self.env.pop("CARGO_BUILD_TARGET", None)
        self.calls = []
        self.checks = []
        self.edits = []
        self.file_reads = []

    def check(self, name, condition):
        if not condition:
            raise AssertionError(name)
        self.checks.append(name)

    def run(self, argv, role, code=0, structured=True):
        # No shell, and exactly one spawn per ledger entry. Give a hung command
        # a termination signal first so prog can clean up its own child group.
        child = subprocess.Popen(
            [str(arg) for arg in argv], cwd=self.project, env=self.env,
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            start_new_session=True,
        )
        try:
            stdout, stderr = child.communicate(timeout=90)
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGTERM)
            try:
                child.communicate(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.communicate()
            raise AssertionError("independent command deadline: " + repr(argv))
        row = {
            "argv": [str(arg) for arg in argv], "role": role,
            "exit_code": child.returncode,
            "stdout_bytes": len(stdout), "stderr_bytes": len(stderr),
            "stdout": stdout.decode("utf-8"), "stderr": stderr.decode("utf-8"),
        }
        self.calls.append(row)
        if child.returncode != code:
            raise AssertionError("unexpected command exit: " + json.dumps(row))
        if structured:
            self.check("bounded response " + str(len(self.calls)), len(stdout) <= BUDGET_BYTES)
            return json.loads(stdout)
        return stdout

    def cli(self, *args, role="navigation"):
        return self.run([self.prog, *args], role)

    def executions(self):
        path = self.root / "executions.tsv"
        return path.read_text().splitlines() if path.exists() else []

    def exact(self, cursor, path):
        evidence = self.cli("evidence", cursor, "--path", path)
        value = evidence["excerpt"]
        if evidence.get("omitted"):
            # An evidence excerpt can itself be bounded. Export the exact
            # cached slice and account for the subsequent local file read.
            exported = self.root / ("evidence-" + str(len(self.calls)) + ".json")
            receipt = self.cli("expand", cursor, "--path", path, "--out", exported)
            contents = exported.read_bytes()
            self.file_reads.append({"path": str(exported), "bytes": len(contents),
                                    "content": contents.decode("utf-8")})
            self.check("export receipt matches bytes " + str(len(self.calls)),
                       hashlib.sha256(contents).hexdigest() == receipt["data_preview"]["sha256"])
            value = json.loads(contents)
        self.check("exact cached slice matches reference " + str(len(self.calls)),
                   digest(value) == evidence["evidence_ref"]["redacted_slice_sha256"])
        return value, evidence

    def capture(self, extra=(), direct=False):
        before = self.executions()
        if direct:
            command = [self.prog, "run", "--preserve-exit-code", "--max-stdout-bytes", "128", "--"]
        else:
            command = [self.project / ".agents/prog-hooks/prog-run.sh"]
        expected_code = 0 if extra or self.fixed else 101
        value = self.run([*command, *SUITE_ARGV, *extra], "capture", expected_code)
        new = self.executions()[len(before):]
        names = [line.split("\t")[0] for line in new]
        expected = ["independent_check"] if extra else ["computes_total", "independent_check"]
        self.check("upstream executed once " + str(len(self.calls)), sorted(names) == sorted(expected))
        self.check("cwd and environment preserved " + str(len(self.calls)), all(
            line.split("\t")[1:] == [str(self.project.resolve()), ENV_MARKER] for line in new
        ))
        metadata, _ = self.exact(value["cursor"], "/command")
        self.check("argv preserved " + str(len(self.calls)), metadata["argv"] == [*SUITE_ARGV, *extra])
        self.check("exit status preserved " + str(len(self.calls)), metadata["exit_code"] == expected_code)
        return value

    def declare(self, name, observation, origin, fingerprint):
        session = self.cli("session", "start", "--goal", name, role="readiness")
        self.cli(
            "session", "obligation-add", "addition-fixed",
            "--check", "the original failure is absent in the complete library suite",
            "--scope", "library", "--origin-observation", origin,
            "--expected-absent-fingerprint", fingerprint,
            "--evidence-observation", observation,
            *["--expected-argv=" + arg for arg in SUITE_ARGV], role="readiness",
        )
        return session["session_id"]

    def edit(self, content):
        path = self.project / "src/lib.rs"
        before = path.read_bytes()
        path.write_text(content)
        self.edits.append({
            "actor": "fixture_driver", "path": "src/lib.rs",
            "before_sha256": hashlib.sha256(before).hexdigest(),
            "after_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        })

    def exercise(self):
        self.fixed = False
        self.check("installed executable is a separate file", not os.path.samefile(self.prog, self.original_binary))
        self.run(["git", "init", "-q"], "setup", structured=False)
        (self.project / "src").mkdir()
        (self.project / "src/lib.rs").write_text(SOURCE)
        (self.project / "Cargo.toml").write_text(
            '[package]\nname="installed-loop"\nversion="0.0.0"\nedition="2021"\n'
        )
        installed = self.cli("harness", "install", "--host", "agent-skills", "--root", self.project, role="setup")
        self.check("portable host installed", installed["hosts"] == ["agent-skills"])
        self.run(["cargo", "generate-lockfile", "--offline"], "setup", structured=False)
        self.run(["git", "add", "."], "setup", structured=False)
        self.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                  "commit", "-qm", "initial fixture"], "setup", structured=False)
        doctor = self.cli("harness", "doctor", "--host", "agent-skills", "--root", self.project, role="setup")
        self.check("installed files verified", doctor["ready"] is True)

        baseline = self.capture()
        origin = baseline["observation"]["observation_id"]
        cursor = baseline["cursor"]
        self.check("failure needs progressive disclosure", bool(baseline["omitted"]))
        inspected = self.cli("inspect", cursor, "--goal", "why computes_total failed")
        # The retrieval strategy gets its path from the actual inspection.
        # The fixture's expected answer is only used after evidence arrives.
        finding = next(item for item in inspected["findings"] if item["kind"] == "rust_panic")
        exact, evidence = self.exact(cursor, finding["evidence_ref"]["path"])
        self.check("exact evidence matches inspection reference", digest(exact) ==
                   finding["evidence_ref"]["redacted_slice_sha256"] ==
                   evidence["evidence_ref"]["redacted_slice_sha256"])
        self.check("actual failing assertion retrieved", "computes_total" in json.dumps(exact)
                   and "left: 6" in json.dumps(exact) and "right: 5" in json.dumps(exact))

        narrow = self.capture(["independent_check"])
        narrowed = self.cli("delta", origin, narrow["observation"]["observation_id"])
        self.check("narrow success cannot resolve the failure", narrowed["assessment"]["can_prove_absence"] is False
                   and all(item["status"] != "resolved" for item in narrowed["findings"]))
        self.declare("negative control: narrower test", narrow["observation"]["observation_id"], origin, finding["fingerprint"])
        narrow_status = self.cli("status", role="readiness")
        self.check("narrow readiness blocks", narrow_status["readiness"]["ready"] is False)

        self.edit(SOURCE.replace("a + b + 1", "a + b"))
        self.fixed = True
        fresh = self.capture()
        subject = fresh["observation"]["observation_id"]
        self.check("verification is a fresh observation", subject != origin)
        verified_session = self.declare("installed coding loop", subject, origin, finding["fingerprint"])
        status = self.cli("status", "--baseline", origin, "--subject", subject, role="readiness")
        direct_readiness = self.cli("session", "show", verified_session, "--readiness", role="readiness")
        direct_delta = self.cli("delta", origin, subject)
        self.check("fresh full suite proves the declared fix", status["readiness"]["ready"] is True
                   and status["readiness"]["evaluations"][0]["status"] == "passed"
                   and status["delta"]["assessment"]["can_prove_absence"] is True)
        self.check("status uses canonical readiness", status["readiness"]["evaluations"] == direct_readiness["evaluations"]
                   and status["readiness"]["blockers"] == direct_readiness["blockers"])
        self.check("status uses canonical delta", status["delta"]["assessment"] == direct_delta["assessment"]
                   and status["delta"]["counts"] == direct_delta["counts"])
        historical, _ = self.exact(cursor, finding["evidence_ref"]["path"])
        self.check("old cursor still returns original evidence", historical == exact)
        self.check("navigation and verification never rerun tests", len(self.executions()) == 5)

        self.edit(SOURCE.replace("a + b + 1", "a + b + 2"))
        stale = self.cli("status", "--session-id", verified_session, role="readiness")
        self.check("workspace mutation invalidates prior verification", stale["readiness"]["ready"] is False
                   and stale["readiness"]["evaluations"][0]["status"] == "stale")
        self.edit(SOURCE.replace("a + b + 1", "a + b"))
        truncated = self.capture(direct=True)
        self.check("successful truncated capture cannot prove absence", truncated["observation"]["capture"]["can_prove_absence"] is False)
        self.declare("negative control: truncated output", truncated["observation"]["observation_id"], origin, finding["fingerprint"])
        incomplete = self.cli("status", role="readiness")
        self.check("incomplete readiness blocks", incomplete["readiness"]["ready"] is False
                   and incomplete["readiness"]["evaluations"][0]["status"] == "unverifiable")
        self.check("all captures and no implicit reruns accounted for", len(self.executions()) == 7)

        restored = self.cli("status", "--session-id", verified_session, role="readiness")
        self.check("restoring identical fixed content retains full-suite verification", restored["readiness"]["ready"] is True)
        self.cli("cache", "purge", "--payload-budget-bytes", "0", role="setup")
        evicted = self.cli("status", "--session-id", verified_session, role="readiness")
        self.check("evicted verification evidence blocks readiness", evicted["readiness"]["ready"] is False
                   and evicted["readiness"]["evaluations"][0]["status"] == "unverifiable")
        self.check("eviction checks never rerun tests", len(self.executions()) == 7)

        return {
            "schema": "prog.installed_coding_loop_smoke", "passed": True,
            "scope": "deterministic installed skill/CLI fixture; no agent, provider, or three-tool facade trial",
            "temporary_store_retained": False,
            "checks": self.checks, "commands": self.calls, "external_edits": self.edits,
            "file_reads": self.file_reads,
            "file_read_bytes": sum(row["bytes"] for row in self.file_reads),
            "stdout_bytes": sum(row["stdout_bytes"] for row in self.calls),
            "stderr_bytes": sum(row["stderr_bytes"] for row in self.calls),
            "instruction_file_bytes": (self.project / ".agents/skills/prog/SKILL.md").stat().st_size,
            "instruction_accounting": "available installed file; not claimed as delivered model context",
            "baseline_observation_id": origin, "verification_observation_id": subject,
            "failure_evidence": evidence, "exact_failure_slice": exact, "verified_status": status,
            "negative_controls": {"narrow": narrow_status, "stale": stale, "incomplete": incomplete, "evicted": evicted},
            "test_executions": self.executions(),
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prog", required=True, type=Path, help="built or installed prog executable")
    parser.add_argument("--output", type=Path, help="optional JSON report; stdout always receives the report")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="prog-installed-loop-") as directory:
        loop = Loop(Path(directory), args.prog.resolve(strict=True))
        try:
            report = loop.exercise()
        except Exception as error:
            report = {"schema": "prog.installed_coding_loop_smoke", "passed": False,
                      "error": str(error), "checks": loop.checks, "commands": loop.calls}
    encoded = json.dumps(report, indent=2) + "\n"
    if args.output:
        args.output.write_text(encoded)
    print(encoded, end="")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
