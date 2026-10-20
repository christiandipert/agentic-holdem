#!/usr/bin/env python3
"""Handshakes correctly, then answers with a mismatched id every time."""
import json, sys
for line in sys.stdin:
    try:
        m = json.loads(line)
    except json.JSONDecodeError:
        continue
    t = m.get("type")
    if t == "entry":
        print(json.dumps({"v": 1, "type": "ready", "id": m["id"], "name": "wrong_id"}), flush=True)
    elif t == "action_request":
        print(json.dumps({"v": 1, "type": "action", "id": m["id"] + 999, "action": "call"}), flush=True)
    elif t == "shutdown":
        break
