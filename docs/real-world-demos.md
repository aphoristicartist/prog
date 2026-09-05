# Real-world demo metrics

Deterministic local demos for recognizable noisy agent workflows. Token counts use the project heuristic `bytes / 4`, rounded up. `expansion_task_bytes` is the initial `prog call` envelope plus the target `prog expand` envelope.

See `demos/real-world/README.md` for copy-paste commands and optional credentialed captures that can emit a local report with `demos/real-world/report_payloads.py`.

Regenerate with `PROG_REAL_WORLD_DEMO_UPDATE=1 cargo test -p prog-cli --test real_world_demos -- --nocapture`.

| Demo | Raw bytes | call envelope bytes | expansion task bytes | cache hit | Token ratio |
|---|---:|---:|---:|---|---:|
| github-pr-review | 191790 | 10266 | 14367 | hit | 13.35x |
| kubectl-events | 145813 | 7950 | 11967 | hit | 12.18x |
| cloudwatch-logs | 157667 | 7336 | 11371 | hit | 13.86x |
| jira-triage | 169953 | 9283 | 13333 | hit | 12.74x |
| mcp-incidents | 150772 | 12120 | 16136 | hit | 9.34x |

## Copy-paste seeds

```bash
prog --dir /tmp/prog-real-world discover github_review --kind cli --seed demos/real-world/seeds/github-pr-review.json
prog --dir /tmp/prog-real-world call github_review review --args '{}'
```

```bash
prog --dir /tmp/prog-real-world discover kubectl_events --kind cli --seed demos/real-world/seeds/kubectl-events.json
prog --dir /tmp/prog-real-world call kubectl_events events --args '{}'
```

```bash
prog --dir /tmp/prog-real-world discover cloudwatch_logs --kind cli --seed demos/real-world/seeds/cloudwatch-logs.json
prog --dir /tmp/prog-real-world call cloudwatch_logs logs --args '{}'
```

```bash
prog --dir /tmp/prog-real-world discover jira_triage --kind cli --seed demos/real-world/seeds/jira-triage.json
prog --dir /tmp/prog-real-world call jira_triage issues --args '{}'
```

```bash
prog --dir /tmp/prog-real-world discover incident_mcp --kind mcp --seed demos/real-world/seeds/mcp-incidents.json
prog --dir /tmp/prog-real-world call incident_mcp list_incidents --args '{}'
```

