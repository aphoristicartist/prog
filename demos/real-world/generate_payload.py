#!/usr/bin/env python3
import json
import sys


def repeated(label, index, size=760):
    base = (
        f"{label} {index}: noisy operational context with request ids, stack frames, "
        "timestamps, labels, retry notes, and copied diagnostics. "
    )
    return (base * ((size // len(base)) + 1))[:size]


def github_pr_review():
    threads = []
    for index in range(96):
        threads.append(
            {
                "id": f"PRRT_{index:04d}",
                "path": f"src/service/module_{index % 11}.rs",
                "line": 20 + index,
                "is_resolved": index % 5 == 0,
                "comments": [
                    {
                        "author": f"reviewer-{index % 7}",
                        "created_at": f"2026-07-0{(index % 6) + 1}T12:{index % 60:02d}:00Z",
                        "body": repeated("github review body", index, 920),
                    },
                    {
                        "author": "ci-bot",
                        "created_at": f"2026-07-0{(index % 6) + 1}T12:{(index + 1) % 60:02d}:00Z",
                        "body": repeated("github ci annotation", index, 620),
                    },
                ],
            }
        )
    return {
        "workflow": "github_pr_review",
        "pull_request": {
            "number": 1842,
            "title": "Refactor billing event fanout",
            "state": "open",
            "author": "platform-team",
        },
        "files": [
            {
                "filename": f"src/service/module_{index}.rs",
                "status": "modified",
                "additions": 40 + index,
                "deletions": 5 + (index % 9),
                "patch": repeated("diff hunk", index, 1100),
            }
            for index in range(18)
        ],
        "review_threads": threads,
    }


def kubectl_events():
    return {
        "kind": "EventList",
        "apiVersion": "v1",
        "items": [
            {
                "metadata": {
                    "name": f"checkout-{index}.17f{index:04x}",
                    "namespace": ["prod", "staging", "payments"][index % 3],
                    "creationTimestamp": f"2026-07-06T14:{index % 60:02d}:00Z",
                },
                "type": "Warning" if index % 4 == 0 else "Normal",
                "reason": ["BackOff", "Pulled", "Scheduled", "Unhealthy"][index % 4],
                "involvedObject": {
                    "kind": "Pod",
                    "name": f"checkout-api-{index % 19}",
                },
                "message": repeated("kubectl event message", index, 880),
                "count": 1 + (index % 13),
            }
            for index in range(132)
        ],
    }


def cloudwatch_logs():
    return {
        "workflow": "cloudwatch_logs",
        "logGroupName": "/aws/lambda/payment-reconciler",
        "events": [
            {
                "timestamp": 1783350000000 + index * 1000,
                "ingestionTime": 1783350005000 + index * 1000,
                "logStreamName": f"2026/07/06/[$LATEST]{index:032x}",
                "message": json.dumps(
                    {
                        "level": "ERROR" if index % 9 == 0 else "INFO",
                        "requestId": f"req-{index:05d}",
                        "tenant": f"tenant-{index % 17}",
                        "detail": repeated("cloudwatch structured log", index, 820),
                    },
                    separators=(",", ":"),
                ),
            }
            for index in range(150)
        ],
    }


def jira_triage():
    return {
        "workflow": "jira_triage",
        "issues": [
            {
                "key": f"PAY-{1200 + index}",
                "summary": f"Payment workflow investigation {index}",
                "status": ["To Do", "In Progress", "Blocked", "Done"][index % 4],
                "priority": ["P0", "P1", "P2", "P3"][index % 4],
                "assignee": f"user-{index % 12}",
                "description": repeated("jira issue description", index, 760),
                "comments": [
                    {
                        "author": f"agent-{index % 5}",
                        "body": repeated("jira comment body", index * 2, 520),
                    },
                    {
                        "author": "incident-commander",
                        "body": repeated("jira escalation note", index * 2 + 1, 520),
                    },
                ],
            }
            for index in range(84)
        ],
    }


def mcp_incidents():
    return {
        "workflow": "mcp_incidents",
        "alerts": [
            {
                "id": f"INC-{5000 + index}",
                "service": ["checkout", "ledger", "search", "identity"][index % 4],
                "severity": ["sev1", "sev2", "sev3"][index % 3],
                "state": "triggered" if index % 6 == 0 else "acknowledged",
                "runbook": repeated("mcp incident runbook", index, 900),
                "evidence": [
                    repeated("alert evidence line", index * 3 + offset, 360)
                    for offset in range(3)
                ],
            }
            for index in range(72)
        ],
    }


PAYLOADS = {
    "github_pr_review": github_pr_review,
    "kubectl_events": kubectl_events,
    "cloudwatch_logs": cloudwatch_logs,
    "jira_triage": jira_triage,
    "mcp_incidents": mcp_incidents,
}


def payload(name):
    try:
        return PAYLOADS[name]()
    except KeyError:
        raise SystemExit(f"unknown demo payload '{name}'; expected one of {', '.join(sorted(PAYLOADS))}")


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: generate_payload.py <demo-name>")
    print(json.dumps(payload(sys.argv[1]), separators=(",", ":")))


if __name__ == "__main__":
    main()
