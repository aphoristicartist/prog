"""Owned, externally bounded MCP connection/diagnostic lifecycle fixtures."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

mode, root = sys.argv[1], Path(sys.argv[2])
signal.alarm(10)
(root / "parent.pid").write_text(str(os.getpid()))
if mode != "empty":
    child_code = """
import signal, sys, time
signal.alarm(10)
time.sleep(float(sys.argv[1]))
if sys.argv[2] == 'finite':
    print('late diagnostic', file=sys.stderr, flush=True)
"""
    child = subprocess.Popen(
        [sys.executable, "-c", child_code, "0.05" if mode == "finite" else "8", mode],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL if mode == "worker" else sys.stderr,
        start_new_session=mode == "escaped",
    )
    (root / "holder.pid").write_text(str(child.pid))
    print("diagnostic marker", file=sys.stderr, flush=True)
    print("Authorization: Bearer secret-mcp-token", file=sys.stderr, flush=True)


def reply(message_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": message_id, "result": result}), flush=True)


def task(status):
    return {"taskId": "fixture-task", "status": status,
            "createdAt": "2026-09-05T00:00:00Z", "lastUpdatedAt": "2026-09-05T00:00:00Z",
            "ttl": 30000, "pollInterval": 500}


for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    with (root / "methods.jsonl").open("a") as log:
        log.write(json.dumps(method) + "\n")
    message_id = message.get("id")
    if message_id is None:
        continue
    if method == "initialize":
        if mode == "handshake_timeout":
            time.sleep(8)
        if mode == "handshake_error":
            sys.exit(3)
        reply(message_id, {"protocolVersion": message["params"]["protocolVersion"],
              "capabilities": {"tools": {}, "tasks": {"requests": {"tools": {"call": {}}}, "cancel": {}}},
              "serverInfo": {"name": "lifecycle", "version": "1"}})
    elif method == "tools/list":
        reply(message_id, {"tools": [{"name": "read", "inputSchema": {"type": "object"},
              "annotations": {"readOnlyHint": True}, "execution": {"taskSupport": "optional"}}]})
    elif method == "tools/call":
        if mode == "request_timeout":
            time.sleep(8)
        if mode == "request_error":
            print(json.dumps({"jsonrpc": "2.0", "id": message_id,
                  "error": {"code": -32603, "message": "fixture error"}}), flush=True)
        elif "task" in message.get("params", {}):
            reply(message_id, {"task": task("working")})
        else:
            reply(message_id, {"content": [{"type": "text", "text": "fixture result"}]})
    elif method == "tasks/get":
        reply(message_id, task("working"))
    elif method == "tasks/result":
        reply(message_id, {"content": [{"type": "text", "text": "task result"}]})
    elif method == "tasks/cancel":
        reply(message_id, task("cancelled"))

if mode == "shutdown_timeout":
    (root / "shutdown.ready").write_text("stdin reached EOF")
    time.sleep(8)
