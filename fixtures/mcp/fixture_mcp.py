import json
import sys

PAYLOAD = {
    "results": [
        {
            "id": index,
            "state": "open" if index % 3 == 0 else "closed",
            "title": f"MCP item {index}",
            "body": f"fixture body {index} " + ("x" * 512),
        }
        for index in range(30)
    ],
    "meta": {"fixture": "mcp", "item_count": 30},
}


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
                "serverInfo": {"name": "prog-doc-fixture", "version": "1.0.0"},
            },
        )
    elif method == "tools/list":
        send_result(
            message_id,
            {
                "tools": [
                    {
                        "name": "search_docs",
                        "description": "Return fixture results",
                        "inputSchema": {
                            "type": "object",
                            "required": ["query"],
                            "properties": {"query": {"type": "string"}},
                        },
                        "annotations": {"readOnlyHint": True},
                    }
                ]
            },
        )
    elif method == "tools/call":
        send_result(
            message_id,
            {
                "content": [{"type": "text", "text": "structured fixture payload"}],
                "structuredContent": PAYLOAD,
                "isError": False,
            },
        )
    else:
        send_error(message_id, -32601, f"unknown method: {method}")
