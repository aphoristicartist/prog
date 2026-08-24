"""The only Terminal-Bench arm difference: install the shipped prog binary."""

import os
from pathlib import Path
from typing import override

from harbor.agents.installed.claude_code import ClaudeCode
from harbor.environments.base import BaseEnvironment


class ClaudeCodeWithProg(ClaudeCode):
    """Claude Code with an exact released prog binary and generated harness files."""

    def __init__(
        self,
        logs_dir: Path,
        prog_binary_path: str,
        expected_prog_version: str,
        *args,
        **kwargs,
    ):
        self._prog_binary_path = (
            Path(os.path.expandvars(prog_binary_path)).expanduser().resolve()
        )
        self._expected_prog_version = expected_prog_version
        super().__init__(logs_dir, *args, **kwargs)

    @staticmethod
    @override
    def name() -> str:
        return "claude-code-prog"

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        await super().install(environment)
        if not self._prog_binary_path.is_file():
            raise ValueError(
                "PROG_PILOT_BINARY must name the verified Linux release binary: "
                f"{self._prog_binary_path}"
            )

        uploaded = "/tmp/prog-terminal-bench-pilot"
        await environment.upload_file(self._prog_binary_path, uploaded)
        await self.exec_as_root(
            environment,
            command=(
                f"install -m 0755 {uploaded} /usr/local/bin/prog && "
                f"test \"$(prog --version)\" = \"prog {self._expected_prog_version}\""
            ),
        )
        await self.exec_as_agent(
            environment,
            cwd="/app",
            command=(
                "prog harness install --root /app "
                "--host agent-skills --host claude-code && "
                "prog harness doctor --root /app "
                "--host agent-skills --host claude-code "
                "> /tmp/prog-harness-doctor.json && "
                "grep -Eq '\"ready\"[[:space:]]*:[[:space:]]*true' "
                "/tmp/prog-harness-doctor.json"
            ),
        )
