# agentic-holdem

A Rust platform where poker agents compete at No-Limit Texas Hold'em. Agents may
be written in any language: they speak a line-oriented JSON protocol over
stdin/stdout, so the engine cannot tell a Rust bot from a Python one.

- **`SPEC.md`** — the project spec.
- **`PLAN.md`** — platform design and the reasoning behind it, including the
  language-agnostic contract (Addendum A) and CPU affinity (Addendum B).
- **`AGENT_SPEC.md`** — **start here if you are writing a bot.** The normative,
  language-independent description of the interface and the game.

## Running it yourself

### Prerequisites

Everything needed is already standard on a Linux box:

| | Needed for | Check |
|---|---|---|
| Rust 1.75+ (`cargo`) | the engine | `cargo --version` |
| `g++` with C++17 | the C++ example bot | `g++ --version` |
| Python 3 | the Python example bot | `python3 --version` |

### First run, in three commands

```sh
git clone git@github.com:christiandipert/agentic-holdem.git
cd agentic-holdem

make -C bots/cpp_halfpot          # build the C++ bot (one time)
cargo run --release -p holdem-cli # build the engine and play
```

It will ask how many chips each bot starts with, then play. Press Enter to
accept the default of 10,000:

```
Starting chips per bot [10000]:
=== agentic-holdem ===
seats 2   chips 10000   blinds 50/100   hands 200   seed 99
(replay this match exactly with --seed 99)

--- hand 1  button=cpp_halfpot  cpp_halfpot=10000  py_caller=10000
    cpp_halfpot posts SB 50   py_caller posts BB 100
      cpp_halfpot raise to 200
      py_caller call 100
    [flop] Th 4h 2d
      py_caller check
      cpp_halfpot bet to 200
      py_caller call 200
    [turn] Th 4h 2d 5d
      py_caller check
      cpp_halfpot bet to 400
      py_caller call 400
    [river] Th 4h 2d 5d 8h
      py_caller check
      cpp_halfpot bet to 800
      py_caller call 800
    pot 3200 -> cpp_halfpot with pair
    showdown  cpp_halfpot: Tc Ac   |   py_caller: 9c 3s

--- hand 2  button=py_caller  cpp_halfpot=11600  py_caller=8400
      ...

=== match report ===========================================
agent                chips       net    bb/100   hands actions    won  viol  norm
py_caller            20000    +10000     322.6      31     185     19     0     0
cpp_halfpot              0    -10000    -322.6      31     122     12     0     0

hands played: 31   chip conservation: 20000 == 20000  OK
```

Two things to notice. The match ended after 31 hands rather than the 200
allowed, because `cpp_halfpot` ran out of chips — a match ends as soon as one
bot busts. And the seed above is only an example: without `--seed` the engine
picks one at random and prints it, so your cards will differ. Pass that seed
back to replay the identical match.


> **Run it from the repository root.** The two example bots are located
> relative to the current directory, so `cd agentic-holdem` first. If you
> don't, the engine says so and tells you where you are.

### Reading the output

| Column | Meaning |
|---|---|
| `chips` | final stack |
| `net` | chips won or lost over the match |
| `bb/100` | big blinds won per 100 hands — the standard poker win-rate unit |
| `hands` | hands this bot was dealt into |
| `actions` | decisions it was asked to make |
| `won` | hands it finished with more chips than it started |
| `viol` | illegal actions, each replaced by check-or-fold |
| `norm` | near-misses corrected for it, e.g. an oversized raise clamped to all-in |

`viol` and `norm` are the ones to watch when writing a bot: **a well-behaved
bot scores 0 in both.** The last line re-counts every chip in play and must
always say `OK`.

### Options

```sh
cargo run --release -p holdem-cli -- [OPTIONS]
```

| Option | Default | |
|---|---|---|
| `--chips <N>` | *prompts* | starting chips per bot |
| `--sb <N>` | 50 | small blind |
| `--bb <N>` | 100 | big blind |
| `--hands <N>` | 200 | hand limit; a match also ends when someone busts |
| `--seed <N>` | random | fixes the cards — same seed, same match |
| `--bot "<argv>"` | the two examples | add a bot by command line, repeatable |
| `--quiet` | off | print only the final report |
| `-h`, `--help` | | show this list |

Note the `--` separator: it tells `cargo` that the flags are for the program,
not for cargo itself. If you'd rather skip it, run the built binary directly:

```sh
cargo build --release
./target/release/holdem --chips 1000 --hands 50
```

### Things to try

A short, fully-specified match with no prompting:

```sh
cargo run --release -p holdem-cli -- --chips 1000 --sb 10 --bb 20 --hands 20 --seed 42
```

Just the result, no hand-by-hand narration:

```sh
cargo run --release -p holdem-cli -- --chips 20000 --hands 200 --quiet
```

**Starting chips matter more than the hand limit.** A match ends the moment one
bot busts, so deep stacks play longer. Measured with these two bots at the
default 50/100 blinds and a 200-hand cap:

| Starting chips | In big blinds | Hands actually played |
|---|---|---|
| 1,000 | 10 | 1 |
| 5,000 | 50 | 26 |
| 20,000 | 200 | 199 |

If a match ends suspiciously early, raise `--chips` or lower `--sb`/`--bb`.

Replay a match exactly — every match prints the seed it used:

```sh
cargo run --release -p holdem-cli -- --chips 1000 --seed 1760990532518391000
```

Run your own bot against an example one:

```sh
cargo run --release -p holdem-cli -- --chips 1000 \
    --bot "python3 my_bot.py" \
    --bot "./bots/cpp_halfpot/halfpot"
```

Play the same bot against itself. Neither ever raises, so every hand is
checked and called down to a showdown and the cards alone decide it — a pure
luck match, which is a good sanity check that nothing in the engine favours a
seat:

```sh
cargo run --release -p holdem-cli -- --chips 1000 --hands 10 \
    --bot "python3 bots/py_caller/bot.py" \
    --bot "python3 bots/py_caller/bot.py"
```

### If something goes wrong

| Symptom | Cause and fix |
|---|---|
| `bot executable not found: .../halfpot` with a *has not been built* hint | run `make -C bots/cpp_halfpot` |
| `bot executable not found` with a *current directory* hint | you are not in the repository root; `cd` there |
| `failed to launch python3 ...` | Python 3 is not on your `PATH` |
| The match appears frozen | a bot is stuck. The deadline is not enforced yet (see **Status**), so press Ctrl+C |
| A bot has a high `viol` count | it is sending illegal actions. Its own error output is in `runs/seed-<N>/<bot>.stderr` |

Anything a bot writes to stderr is captured per match in
`runs/seed-<N>/<bot>.stderr` — never a protocol violation, and the first place
to look when debugging a bot.

### Writing your own bot

Read **`AGENT_SPEC.md`**. The short version: your program reads one JSON
object per line on stdin and writes one per line on stdout, answering `entry`
with `ready` and each `action_request` with an `action`. **Flush after every
reply** — a buffered line is indistinguishable from a bot that never answered.
`bots/py_caller/bot.py` is a complete working bot in about 40 lines of Python;
copy it and change `decide()`.

Then point the engine at it:

```sh
cargo run --release -p holdem-cli -- --chips 1000 \
    --bot "python3 my_bot.py" --bot "./bots/cpp_halfpot/halfpot"
```

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
