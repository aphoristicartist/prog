# Real-world demo metrics

Deterministic local demos for recognizable noisy agent workflows. Token counts use the project heuristic `bytes / 4`, rounded up. `expansion_task_bytes` is the initial `prog call` envelope plus the target `prog expand` envelope.

See `demos/real-world/README.md` for copy-paste commands and optional credentialed captures that can emit a local report with `demos/real-world/report_payloads.py`.

Regenerate with `PROG_REAL_WORLD_DEMO_UPDATE=1 cargo test -p prog-cli --test real_world_demos -- --nocapture`.

| Demo | Raw bytes | call envelope bytes | expansion task bytes | cache hit | Token ratio |
|---|---:|---:|---:|---|---:|
| github-pr-review | 191790 | 11319 | 14301 | hit | 13.41x |
| kubectl-events | 145813 | 7846 | 10727 | hit | 13.59x |
| cloudwatch-logs | 157667 | 7244 | 10233 | hit | 15.40x |
| jira-triage | 169953 | 10330 | 13253 | hit | 12.82x |
| mcp-incidents | 150772 | 12787 | 15684 | hit | 9.61x |

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

