#!/usr/bin/env python3
"""Noisy failing-tool fixture for modern JUnit and SARIF recipes."""

import json
import os
from pathlib import Path
import sys
from typing import Optional


def option_value(arguments: list[str], option: str) -> Optional[str]:
    prefix = f"{option}="
    for index, argument in enumerate(arguments):
        if argument.startswith(prefix):
            return argument[len(prefix) :]
        if argument == option and index + 1 < len(arguments):
            return arguments[index + 1]
    return None


def require(arguments: list[str], option: str, expected: Optional[str] = None) -> str:
    value = option_value(arguments, option)
    if value is None or (expected is not None and value != expected):
        raise SystemExit(f"missing expected {option} option")
    return value


def main() -> int:
    if len(sys.argv) < 2:
        raise SystemExit("pass one fixture tool name")
    tool = sys.argv[1]
    arguments = sys.argv[2:]

    if tool == "vitest":
        require(arguments, "--reporter", "junit")
        output = require(arguments, "--outputFile")
        report_kind = "junit"
    elif tool == "playwright":
        require(arguments, "--reporter", "junit")
        output = os.environ.get("PLAYWRIGHT_JUNIT_OUTPUT_FILE")
        if not output:
            raise SystemExit("missing PLAYWRIGHT_JUNIT_OUTPUT_FILE")
        report_kind = "junit"
    elif tool == "bun":
        require(arguments, "--reporter", "junit")
        output = require(arguments, "--reporter-outfile")
        report_kind = "junit"
    elif tool == "deno":
        output = require(arguments, "--junit-path")
        report_kind = "junit"
    elif tool == "ruff":
        require(arguments, "--output-format", "sarif")
        output = require(arguments, "--output-file")
        report_kind = "sarif"
    elif tool == "biome":
        require(arguments, "--reporter", "sarif")
        output = require(arguments, "--reporter-file")
        report_kind = "sarif"
    elif tool == "semgrep":
        output = require(arguments, "--sarif-output")
        report_kind = "sarif"
    else:
        raise SystemExit(f"unknown fixture tool: {tool}")

    for index in range(512):
        print(f"runner noise {index:04d} " + "x" * 96)

    path = Path(output)
    path.parent.mkdir(parents=True, exist_ok=True)
    if report_kind == "junit":
        path.write_text(
            """<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="modern" tests="1" failures="1">
  <testcase classname="checkout" name="rejects stale total" time="0.01">
    <failure message="expected 19, received 21">modern fixture failure</failure>
  </testcase>
</testsuite>
""",
            encoding="utf-8",
        )
    else:
        path.write_text(
            json.dumps(
                {
                    "version": "2.1.0",
                    "runs": [
                        {
                            "tool": {"driver": {"name": tool}},
                            "results": [
                                {
                                    "ruleId": "fixture.error",
                                    "level": "error",
                                    "message": {"text": "modern fixture diagnostic"},
                                    "locations": [
                                        {
                                            "physicalLocation": {
                                                "artifactLocation": {"uri": "src/example.ts"},
                                                "region": {"startLine": 7, "startColumn": 3},
                                            }
                                        }
                                    ],
                                }
                            ],
                        }
                    ],
                }
            ),
            encoding="utf-8",
        )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
