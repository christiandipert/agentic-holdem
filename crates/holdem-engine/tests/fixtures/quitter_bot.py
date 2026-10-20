#!/usr/bin/env python3
"""Handshakes, plays one decision, then exits mid-match."""
import json, sys
decisions = 0
for line in sys.stdin:
    try:
        m = json.loads(line)
    except json.JSONDecodeError:
        continue
    t = m.get("type")
    if t == "entry":
        print(json.dumps({"v": 1, "type": "ready", "id": m["id"], "name": "quitter"}), flush=True)
    elif t == "action_request":
        if decisions >= 1:
            sys.exit(0)
        decisions += 1
        print(json.dumps({"v": 1, "type": "action", "id": m["id"], "action": "call"}), flush=True)
    elif t == "shutdown":
        break
