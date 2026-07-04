import json

items = [
    {
        "id": index,
        "state": "open" if index % 3 == 0 else "closed",
        "title": f"CLI item {index}",
        "body": f"fixture body {index} " + ("x" * 512),
    }
    for index in range(30)
]

print(json.dumps({"items": items, "meta": {"fixture": "cli", "item_count": len(items)}}))
