#!/usr/bin/env python3
import json
import sys

import generate_payload


PAYLOAD = generate_payload.payload("mcp_incidents")


def send_result(message_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": message_id, "result": result}) + "\n")
    sys.stdout.flush()


def send_error(message_id, code, message):
    sys.stdout.write(
        json.dumps(
            {
                "jsonrpc": "2.0",
                "id": message_id,
                "error": {"code": code, "message": message},
            }
        )
        + "\n"
    )
    sys.stdout.flush()


for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    message_id = message.get("id")
    if message_id is None:
        continue
    if method == "initialize":
        send_result(
            message_id,
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "prog-real-world-incident-demo", "version": "1.0.0"},
            },
        )
    elif method == "tools/list":
        send_result(
            message_id,
            {
                "tools": [
                    {
                        "name": "list_incidents",
                        "description": "Return incident alerts with evidence and runbooks",
                        "inputSchema": {"type": "object", "properties": {}},
                        "annotations": {"readOnlyHint": True},
                    }
                ]
            },
        )
    elif method == "tools/call":
        send_result(
            message_id,
            {
                "content": [{"type": "text", "text": "incident alert payload"}],
                "structuredContent": PAYLOAD,
                "isError": False,
            },
        )
    else:
        send_error(message_id, -32601, f"unknown method: {method}")
