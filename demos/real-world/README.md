# Real-world demo suite

These demos are deterministic local stand-ins for noisy workflows agents already
see in production: GitHub PR review threads, Kubernetes events, CloudWatch logs,
Jira triage queues, and a separate MCP incident server.

The CI subset runs without external credentials:

```bash
cargo test -p prog-cli --test real_world_demos -- --nocapture
```

Regenerate the checked-in report and metrics with:

```bash
PROG_REAL_WORLD_DEMO_UPDATE=1 cargo test -p prog-cli --test real_world_demos -- --nocapture
```

## Copy-paste demos

Run from the repository root after installing `prog` or replace `prog` with
`cargo run --`.

```bash
rm -rf /tmp/prog-real-world
prog --dir /tmp/prog-real-world discover github_review --kind cli --seed demos/real-world/seeds/github-pr-review.json
prog --dir /tmp/prog-real-world call github_review review --args '{}'
CURSOR=$(prog --dir /tmp/prog-real-world call github_review review --args '{}' | python3 -c 'import json,sys; print(json.load(sys.stdin)["cursor"])')
prog --dir /tmp/prog-real-world expand "$CURSOR" --path /review_threads/37/comments/0/body
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

Each second `call` returns `cache.status: "hit"` and each expansion reads from
the local cursor cache.

## Optional credentialed captures

These commands are examples only and are not run by CI:

```bash
mkdir -p /tmp/prog-external
gh api repos/OWNER/REPO/pulls/NUMBER/comments > /tmp/prog-external/github-pr-comments.json
kubectl get events -A -o json > /tmp/prog-external/kubectl-events.json
aws logs filter-log-events --log-group-name /aws/lambda/SERVICE > /tmp/prog-external/cloudwatch.json
```

Emit a local report for captured payloads:

```bash
python3 demos/real-world/report_payloads.py \
  github:application/json:/tmp/prog-external/github-pr-comments.json:/0/body \
  kubectl:application/json:/tmp/prog-external/kubectl-events.json:/items/0/message \
  cloudwatch:application/json:/tmp/prog-external/cloudwatch.json:/events/0/message
```
