# agentic-holdem

A Rust platform where poker agents compete at No-Limit Texas Hold'em. Agents may
be written in any language: they speak a line-oriented JSON protocol over
stdin/stdout, so the engine cannot tell a Rust bot from a Python one.

- **`SPEC.md`** — the project spec.
- **`PLAN.md`** — platform design and the reasoning behind it, including the
  language-agnostic contract (Addendum A) and CPU affinity (Addendum B).
- **`AGENT_SPEC.md`** — **start here if you are writing a bot.** The normative,
  language-independent description of the interface and the game.

## Build and run

```sh
cargo build --release          # the engine
make -C bots/cpp_halfpot       # the C++ example bot
cargo run -p holdem-cli        # prompts for starting chips, then plays
```

Useful flags:

```sh
cargo run -p holdem-cli -- --chips 1000 --sb 10 --bb 20 --hands 50 --seed 42
cargo run -p holdem-cli -- --quiet                 # report only
cargo run -p holdem-cli -- --bot "python3 my.py" --bot ./other_bot
```

Every match prints its seed; passing the same `--seed` replays the identical
cards and the identical match.

## Example bots

| Bot | Language | Strategy |
|---|---|---|
| `bots/cpp_halfpot` | C++17 | Wagers half the pot every time |
| `bots/py_caller` | Python 3 | Matches any bet; folds when it cannot cover it |

Both are deliberately simple — they exist to exercise the platform, not to play
well. Each file documents the assumptions it makes where the brief was ambiguous.

## Layout

```
crates/holdem-agent    the public agent contract; the only crate a bot needs
crates/holdem-core     cards, deck, hand evaluator, pot and side-pot math
crates/holdem-engine   hand loop, betting rules, agent bindings, wire protocol
crates/holdem-cli      the `holdem` binary
bots/                  example bots
```

## Tests

```sh
cargo test
```

Covers hand-evaluator correctness, side-pot and chip-conservation invariants,
heads-up blind order, determinism from a seed, and a suite of deliberately
misbehaving bots (malformed replies, mismatched ids, a bot that exits
mid-match) to show that bad behaviour is penalised rather than fatal.

## Status

Implemented: the game loop and the JSON protocol, with the two-tier
invalid-action policy (unambiguous near-misses corrected, everything else
replaced by check-or-fold).

Not yet implemented, by design — see `PLAN.md`: the 20-second deadline and the
per-run time bank are **advisory** (sent to bots, not enforced), and CPU pinning
is not wired up. A bot that hangs forever will currently wedge a match; one that
crashes or exits is handled cleanly.
