#!/usr/bin/env python3
"""Compare the installed CLI fixture with real registered tools, without a model."""

import argparse
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import tempfile
import time

from installed_coding_loop import BUDGET_BYTES, Loop


def encoded(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


class InstructedLoop(Loop):
    def capture(self, *args, **kwargs):
        if not hasattr(self, "delivered_instructions"):
            self.delivered_instructions = (self.project / ".agents/skills/prog/SKILL.md").read_text()
        return super().capture(*args, **kwargs)


class RegisteredLoop(Loop):
    def __init__(self, root, binary, node, driver, artifact):
        super().__init__(root, binary)
        self.deliveries = []
        self.buffer = b""
        self.host_errors = (root / "host-stderr").open("w+b")
        self.host = subprocess.Popen(
            [str(node), str(driver), str(artifact), str(self.prog),
             str(self.project), str(root / "store")],
            cwd=self.project, env=self.env, stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=self.host_errors, start_new_session=True,
        )
        try:
            self.assembly = self.receive()["assembly"]
        except BaseException:
            self.close()
            raise

    def receive(self):
        deadline = time.monotonic() + 90
        with selectors.DefaultSelector() as selector:
            selector.register(self.host.stdout, selectors.EVENT_READ)
            while b"\n" not in self.buffer:
                if not selector.select(max(0, deadline - time.monotonic())):
                    raise AssertionError("independent registered host response deadline")
                chunk = os.read(self.host.stdout.fileno(), 65536)
                if not chunk:
                    self.host_errors.seek(0)
                    raise AssertionError("registered host exited: " + self.host_errors.read().decode())
                self.buffer += chunk
                if len(self.buffer) > 2 * 1024 * 1024:
                    raise AssertionError("fixture host response exceeded independent cap")
        line, self.buffer = self.buffer.split(b"\n", 1)
        return json.loads(line)

    def close(self):
        self.host.stdin.close()
        try:
            self.host.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(self.host.pid, signal.SIGTERM)
            try:
                self.host.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(self.host.pid, signal.SIGKILL)
                self.host.wait()
        self.host.stdout.close()
        self.host_errors.close()

    def tool(self, name, arguments, role):
        request = {"name": name, "arguments": arguments}
        self.host.stdin.write(encoded(request) + b"\n")
        self.host.stdin.flush()
        result = self.receive()
        self.deliveries.append({"request": request, "role": role, "result": result})
        self.check("registered tool succeeds " + str(len(self.deliveries)), result["isError"] is False)
        self.check("canonical value equals delivered content " + str(len(self.deliveries)),
                   json.loads(result["content"][0]["text"]) == result["value"])
        self.check("registered content bounded " + str(len(self.deliveries)),
                   sum(len(block["text"].encode()) for block in result["content"]) <= BUDGET_BYTES)
        return result["value"]

    def run(self, argv, role, code=0, structured=True):
        if str(argv[0]).endswith("/.agents/prog-hooks/prog-run.sh"):
            # This fixture maps its own known argv shape, never reparses shell
            # text. Exit truth is checked from exact /command by Loop.capture.
            return self.tool("prog_observe", {"mode": "run", "argv": argv[1:]}, role)
        return super().run(argv, role, code, structured)

    def cli(self, *args, role="navigation"):
        command, *tail = [str(arg) for arg in args]
        if command not in ("inspect", "evidence", "expand", "status"):
            return super().cli(*args, role=role)
        if command == "status":
            arguments = {tail[i][2:].replace("-", "_"): tail[i + 1] for i in range(0, len(tail), 2)}
            name = "prog_status"
        else:
            cursor, *flags = tail
            arguments = {"mode": {"evidence": "exact"}.get(command, command), "cursor": cursor}
            arguments.update({flags[i][2:]: flags[i + 1] for i in range(0, len(flags), 2)})
            name = "prog_evidence"
        return self.tool(name, arguments, role)

    def exact(self, cursor, path):
        value, evidence = super().exact(cursor, path)
        canonical = super().cli("evidence", cursor, "--path", path, role="parity_reference")
        immutable = ["cursor", "path", "source_id", "operation", "redacted_slice_sha256"]
        self.check("same-cursor canonical evidence parity " + str(len(self.calls)),
                   all(evidence["evidence_ref"][key] == canonical["evidence_ref"][key] for key in immutable)
                   and evidence["excerpt"] == canonical["excerpt"]
                   and evidence["omitted"] == canonical["omitted"])
        return value, evidence

    def exercise(self):
        report = super().exercise()
        report.update(
            schema="prog.registered_coding_loop_smoke",
            scope="deterministic caller through an unpacked npm artifact and real host registry; no agent or provider trial",
            assembly=self.assembly, deliveries=self.deliveries,
            instruction_accounting="assembled sections and tool schemas delivered once to this scripted caller",
        )
        # The direct session/readiness and delta checks in the shared fixture
        # compare the very same facade-created observation IDs and fingerprints.
        # No fresh capture is used as a proxy for parity.
        report["context_bytes"] = {
            "assembled_sections": sum(len(section["text"].encode()) for section in self.assembly["sections"]),
            "assembled_tool_schemas_json": len(encoded(self.assembly["tools"])),
            "tool_requests_json": sum(len(encoded(row["request"])) for row in self.deliveries),
            "tool_presentations_json": sum(len(encoded({key: row["result"][key]
                for key in ("isError", "content", "additionalContexts") if key in row["result"]}))
                for row in self.deliveries),
            "cli_stdout": report["stdout_bytes"], "cli_stderr": report["stderr_bytes"],
            "exported_file_reads": report["file_read_bytes"],
        }
        report["fixture_host_result_bytes"] = sum(len(encoded(row["result"])) for row in self.deliveries)
        report["accounting_limits"] = (
            "Actual fixture deliveries, including tool presentation wrappers and notices. "
            "Internal duplicate value fields are reported separately in fixture_host_result_bytes. "
            "Not provider tokenization, per-turn schema resend, or an actual-agent comparison. "
            "Setup, explicit user criteria, and unsupported truncated-capture control use the advanced CLI."
        )
        return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("prog", "node", "driver", "artifact"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="prog-registered-loop-") as directory:
        root = Path(directory)
        (root / "cli").mkdir()
        cli_loop = InstructedLoop(root / "cli", args.prog.resolve(strict=True))
        baseline = cli_loop.exercise()
        # The fixture reads the installed instructions before its first capture.
        skill = cli_loop.delivered_instructions
        baseline["delivered_instructions"] = skill
        baseline["instruction_accounting"] = "installed skill read once by the scripted caller"
        baseline["context_bytes"] = {
            "instructions": len(skill.encode()), "cli_stdout": baseline["stdout_bytes"],
            "cli_stderr": baseline["stderr_bytes"], "exported_file_reads": baseline["file_read_bytes"],
            "cli_argv_json": sum(len(encoded(row["argv"])) for row in baseline["commands"]),
        }
        (root / "registered").mkdir()
        loop = RegisteredLoop(root / "registered", args.prog.resolve(strict=True),
                              args.node.resolve(strict=True), args.driver.resolve(strict=True),
                              args.artifact.resolve(strict=True))
        try:
            registered = loop.exercise()
            report = {"schema": "prog.installed_facade_comparison", "passed": True,
                      "cli": baseline, "registered": registered}
        finally:
            loop.close()
    output = json.dumps(report, indent=2) + "\n"
    if args.output:
        args.output.write_text(output)
    print(output, end="")


if __name__ == "__main__":
    main()
