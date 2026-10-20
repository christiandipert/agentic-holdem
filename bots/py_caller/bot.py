#!/usr/bin/env python3
"""py_caller - a Python example bot for the Agentic Poker Arena.

Strategy, in full: match whatever is bet. If it cannot match the full
outstanding wager, it folds.

Ambiguities and the assumptions made about them:

  * "matches the bet" means calling, never raising. This bot never puts in
    more than it has to.
  * "if it can't, it folds" is taken literally. Real poker would let a short
    stack call all-in for less, and the platform even offers that for free
    (`to_call` arrives clamped to our stack, so `call` is always affordable).
    This bot deliberately does *not* take it: it compares the FULL amount owed
    against its stack and folds when it cannot cover it. That is the stated
    behaviour, and it also exercises the engine's fold path on short stacks,
    which is more useful for testing than a bot that never folds.
  * With nothing owed it checks, since that is the only legal free action.

See AGENT_SPEC.md for the protocol. Every reply is flushed: a buffered line is
indistinguishable from a bot that never answered.
"""

import json
import sys

PROTOCOL_VERSION = 1


def send(msg):
    """One JSON object per line, flushed immediately."""
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def decide(msg):
    """Return (action, amount_or_None) for an action_request."""
    me = msg["me"]
    seats = msg["seats"]
    mine = next(s for s in seats if s["seat"] == me)

    # `to_call` is clamped to our stack by the engine, so compute the real
    # outstanding wager ourselves to decide whether we can truly match it.
    owed = msg["current_bet"] - mine["street_bet"]

    if owed <= 0:
        return "check", None
    if owed <= mine["stack"]:
        return "call", None
    return "fold", None


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError as exc:
            # Never crash on a line we cannot read; the engine is the authority.
            print(f"py_caller: unreadable line: {exc}", file=sys.stderr)
            continue

        kind = msg.get("type")

        if kind == "entry":
            print(
                f"py_caller: seat {msg['me']}, stack {msg['starting_stack']}, "
                f"blinds {msg['small_blind']}/{msg['big_blind']}",
                file=sys.stderr,
            )
            send({
                "v": PROTOCOL_VERSION,
                "type": "ready",
                "id": msg["id"],
                "name": "py_caller",
                "author": "example",
                "language": "python",
            })

        elif kind == "action_request":
            action, amount = decide(msg)
            reply = {
                "v": PROTOCOL_VERSION,
                "type": "action",
                "id": msg["id"],
                "action": action,
            }
            if amount is not None:
                reply["amount"] = amount
            send(reply)

        elif kind == "shutdown":
            print("py_caller: shutting down", file=sys.stderr)
            break

        # hand_start and hand_end expect NO reply. Unknown types are ignored,
        # which is what keeps this bot working when the engine adds messages.


if __name__ == "__main__":
    main()
