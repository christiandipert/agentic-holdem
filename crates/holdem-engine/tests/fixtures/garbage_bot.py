#!/usr/bin/env python3
"""Handshakes correctly, then answers every decision with non-JSON."""
import json, sys
for line in sys.stdin:
    try:
        m = json.loads(line)
    except json.JSONDecodeError:
        continue
    t = m.get("type")
    if t == "entry":
        print(json.dumps({"v": 1, "type": "ready", "id": m["id"], "name": "garbage"}), flush=True)
    elif t == "action_request":
        print("this is definitely not json", flush=True)
    elif t == "shutdown":
        break
