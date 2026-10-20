# Agentic Poker Arena — Agent Specification

**Status: DRAFT.** This is the document bot authors read. It is written to be the *only*
thing you need: if something about the platform is not here, you should not have to care
about it. Engine internals, supervision, and performance engineering live in `PLAN.md` and
are deliberately not your problem.

Protocol version: **1**. Draft revision 2026-10-20.

> Open items still being settled are marked **[OPEN]**. They will not change the shape of
> your bot, only details of degree.

---

## 1. What you are writing

A bot is a decision function. The platform runs the game; you answer one question, over
and over:

> Given everything you are allowed to know, what do you do?

You implement exactly **two** required behaviours:

| | Called | You must |
|---|---|---|
| **`entry`** | once, before the first hand | set yourself up |
| **`on_action`** | every time it is your turn | return a legal action within the time limit |

And one **optional** behaviour, `on_hand_end`, called after every hand — the only place you
ever learn an opponent's hole cards. Ignore it and your bot still works.

You may write in **Rust** (in-process, fastest) or **any language** that can read and write
lines on stdin/stdout (§6). The rules, the game, and the penalties are identical either
way; the platform guarantees you cannot tell from the inside which one you are.

### 1.1 What the platform handles so you don't

You never deal cards, never move chips, never compute a pot, never decide a showdown, never
check your own legality, never manage a clock, and never talk to another bot. You are handed
a position and you name an action. Specifically, the platform owns:

- shuffling, dealing, burn cards, board runout
- blinds, antes, button rotation, who acts when
- pot and **side pot** construction and awarding
- hand evaluation and showdown comparison
- **computing how much a `call` costs** — you say `call`, never an amount
- normalizing near-miss actions and penalizing illegal ones (§5)
- the clock, the time bank, logging, and reporting

**You never need to implement poker rules.** Every decision you are asked for comes with
`to_call`, `min_raise_to`, and `max_raise_to` (§4.2). Those three numbers are sufficient to
choose a legal action with no rule knowledge whatsoever.

---

## 2. The system you run on

Published so you can size your own work. Numbers are from the reference host; the live demo
runs on this machine.

| | |
|---|---|
| CPU | Intel Core i5-14600 — 6 performance cores @ 5.2 GHz + 8 efficiency cores @ 3.9 GHz |
| Your core | **one performance core, pinned.** 2 MiB private L2, 24 MiB shared L3 |
| Memory | 31 GiB total on the host |
| OS | Ubuntu 24.04.5 LTS, Linux 6.8.0, x86_64 |
| Rust | 1.75.0, **edition 2021** (newer-edition features will not compile) |
| Available runtimes | Python 3.12.3 · Node 22.17.1 · OpenJDK 21 · GCC 13.3 · Clang 18.1 |
| Disk | network-mounted home directory — **slow**; do not do file I/O during a hand |

### 2.1 You get exactly one CPU core, and it does not move

Your bot is **pinned to a single CPU core** for the whole run, using the OS scheduler's
affinity mechanism. This is not advisory — it is enforced, and there is no way to opt out.

What it means for you:

- **Threads will not make you faster.** Spawning 8 worker threads gets you 8 threads
  time-slicing one core, which is *slower* than one thread because you pay context switches
  for nothing. Write single-threaded code. (Subprocess bots: your child processes inherit
  the same pin, so forking doesn't escape it either.)
- **Your core does not change mid-run.** The CPU cache you warmed up on your last turn is
  still warm on your next one, so lookup tables and transposition tables built at `entry`
  pay off. Size hot tables to **fit in 2 MiB** if you want them to stay in L2 — a table
  much larger than that will be served from shared L3 or main memory and will be
  dramatically slower than its size suggests.
- **Every bot in the run gets the same core type.** Nobody is handed a faster CPU than you.
  The host has six performance cores, so **a match holds at most six bots** — the platform
  refuses to start a seventh rather than seat it on a slower core and quietly taint the
  result.
- **You share the machine, but not your core,** and poker is sequential: only one bot is
  ever thinking at a time, so you effectively get the core to yourself during your turn.

One caveat the platform cannot remove: the host runs a **power-saving CPU governor** that
idles cores at 800 MHz and ramps up under load, and we do not have permission to change it.
Your first decision of a run may execute at a fraction of full clock speed before the core
ramps. The platform does a warmup pass before hand 1 to mitigate this, but **do not
calibrate a fixed iteration count** at `entry` and assume it will take the same wall time
later in the run — it won't. Bound your work by the clock the platform gives you (§3.1),
never by a count you measured once.

**Your budget per bot** (**[OPEN]** — exact caps may tighten):

| Resource | Limit | Notes |
|---|---|---|
| Wall clock per decision | **20 s** | §3 |
| Time bank | **one 60 s extension per run** | §3.2 |
| Startup / `entry` | **10 s**, separate from the above | load tables and models here |
| CPU parallelism | **1 core, pinned and enforced** (§2.1) | extra threads cannot help |
| Memory | ~**1 GiB** resident, soft | be able to justify more |
| Disk | read-only, your own directory, at `entry` time only | no writes expected |
| Network | **none.** Assume no connectivity | no remote APIs mid-hand |
| Persistence between runs | **none.** You start fresh every run | learning must happen within a run |

Measure against the wall clock the platform hands you, never against an assumed core
count or a previously calibrated iteration count.

---

## 3. The clock

### 3.1 The 20-second rule

You have **20 seconds of wall-clock time per decision.** The clock starts when the platform
hands you the position and stops when it has your answer in hand. For subprocess bots your
own JSON parsing and writing are inside that window (overhead is a few milliseconds against
20,000 — not something to engineer around, but don't count on the last 50 ms).

Every decision tells you how much time you have left, so a search bot can bound itself:

```
while time_left() > safety_margin:
    do_one_more_rollout()
```

**Leave a safety margin.** Returning at 19.99 s is a coin flip you lose.

### 3.2 The time bank: one extension per RUN, not per hand

You get **one** extension for the **entire run** — not one per hand. Spend it and it is gone
until the next run.

It engages **automatically**: blow through 20 s and, if your bank is unspent, the platform
keeps waiting for up to 60 s more and accepts an answer arriving inside that window. There is
no button to press and no message to send.

Three consequences worth internalizing:

1. **You can spend it by accident.** An unintentionally slow decision on hand 3 burns the
   bank you wanted for a river decision on hand 300.
2. **You can see whether you still have it.** Every decision reports your remaining bank.
   Treat it as a resource: if it is gone, your 20 s is hard.
3. **It does not stack.** Time out again after the bank is spent and you are penalized (§5).

Reasonable strategy: hold the bank for genuinely large pots, and check the remaining-bank
field before committing to a deep search.

### 3.3 Running out

Not answering in time is a **penalty**, not a crash: the platform acts for you — **check if
nothing is owed, otherwise fold** (§5.2) — logs it, and the match continues. You are never
removed for being slow once.

---

## 4. What you are told

### 4.1 Cards

A card is a **rank** then a **suit**, both case-sensitive:

```
rank   2 3 4 5 6 7 8 9 T J Q K A
suit   s spades   h hearts   d diamonds   c clubs

"Ah" = ace of hearts      "Td" = ten of diamonds      "2c" = deuce of clubs
```

All chip amounts are **non-negative integers**. There are no fractional chips anywhere in
this platform.

### 4.2 The position you are handed

Every time it is your turn you receive:

**Who and where**
- `me` — your seat; `button` — the dealer button's seat; `street` — `preflop` / `flop` /
  `turn` / `river`
- `hand_number` — which hand of the run this is

**Your cards**
- `hole` — your two cards
- `board` — community cards: 0 on preflop, 3 / 4 / 5 after

**The money**
- `pot` — everything committed so far, all streets, all players
- `current_bet` — the largest total any player has wagered **this street**
- `to_call` — **what it costs you to call.** `0` means checking is free
- `min_raise_to` — the smallest legal raise total. **`0` means you may not raise**
- `max_raise_to` — the largest legal raise total, i.e. your all-in
- `big_blind` — so you can reason in big blinds rather than raw chips

**The table** — one entry per seat, **including yours**:
- `stack` (chips behind), `street_bet` (wagered this street), `committed` (wagered this
  hand), `status` (`active` / `folded` / `allin` / `busted` / `disqualified`)

**The history**
- `hand_log` — every action taken **this hand** by **every** player, in order, with amounts

**The clock**
- time remaining for this decision, and time bank remaining for the run

### 4.3 What you are NOT told, ever

- **Any other player's hole cards** while a hand is live. There is no leak and no trick to
  find; the platform does not possess them in anything you are handed.
- The deck, its order, or its shuffle seed. You cannot predict a future card.
- Other bots' internals, code, or state.

### 4.4 Remembering things is up to you

The platform gives you `hand_log` for **the current hand only**. It does not track history
across hands for you, and it does not compute opponent statistics.

If you want to know that seat 1 has raised preflop 70% of the time over 200 hands, **store
that yourself** — your bot object persists for the whole run, so accumulate whatever you
like in it. This is deliberate: profiling opponents is a strategy choice, and bots that
don't want to pay for it shouldn't have to.

Your memory is wiped between runs (§2). Anything learned must be learned within the run.

### 4.5 Randomness

You are given your own random seed at `entry`. **Use it** rather than seeding from the clock
or from system entropy: your seed is independent of the deck (you cannot infer cards from
it), and using it keeps matches reproducible, which is how results get verified. A bot
seeded from wall-clock time makes a run impossible to replay.

---

## 5. Actions, legality, and what happens when you are wrong

### 5.1 The six actions

| Action | Amount | Legal when | Means |
|---|---|---|---|
| `fold` | — | always | give up the hand |
| `check` | — | `to_call == 0` | pass, wager nothing |
| `call` | — | `to_call > 0` | match the bet. **The platform computes the cost** |
| `bet` | **required** | `current_bet == 0` | open the betting this street |
| `raise` | **required** | `min_raise_to > 0` | increase the bet |
| `allin` | — | always | shove your whole stack |

**`amount` is a TOTAL, not an increment.** `raise` with amount `900` means "my total wager
this street becomes 900" — *not* "add 900 to the current bet." This is the single most
common mistake; the platform hands you `min_raise_to` and `max_raise_to` in exactly the same
units, so **any integer in `[min_raise_to, max_raise_to]` is a legal raise** and no
arithmetic is required of you.

Always-safe fallbacks, if you want a trivially correct bot:

```
if to_call == 0:  check      # always legal
else:             fold       # always legal
```

### 5.2 When you are wrong

Two tiers, and most mistakes land in the forgiving one.

**Tier 1 — quietly fixed.** Actions with one obvious poker meaning are corrected, logged,
and played. You are not penalized:

- `call` when calling costs your whole stack → all-in call
- `raise` above `max_raise_to` → clamped to all-in
- `allin` → the correct shove or short all-in call for your situation
- a short all-in that falls below `min_raise_to` → allowed, it's a real poker action

**Tier 2 — penalized.** Everything else. The platform substitutes:

> **`check` if `to_call == 0`, otherwise `fold`.**

That is the rule for *all* of these:

- timing out with the bank spent or already used
- crashing, panicking, or exiting
- `check` when you owe chips
- `raise` below `min_raise_to` while you still have more chips than that
- `bet` when there is already a bet (that is a `raise`), or `bet` of `0`
- acting when it is not your turn, or after you have folded or are all-in
- an unparseable or structurally invalid reply (subprocess bots)

### 5.3 You are not disqualified for playing badly

There is no three-strikes rule. Keep sending garbage and you keep folding, bleed blinds, and
bust out on your own — the match completes and your violation count is printed next to your
final chips. The report shows, per bot: timeouts, time-bank use, Tier-1 normalizations,
Tier-2 violations, and decision-time percentiles. A clean bot and a sloppy bot are
distinguishable at a glance, which is the point.

The **one** fatal failure is becoming unreachable — hanging forever, or exiting. That seat is
marked `disqualified` and auto-folds for the rest of the run, because there is no longer
anyone to ask.

---

## 6. Writing a bot

### 6.1 Rust (in-process)

Depend on **`holdem-agent` only**. You cannot reach engine internals, so you cannot
accidentally cheat.

```rust
use holdem_agent::{Agent, Action, PlayerView, MatchContext, HandResult, Chips};

pub struct MyBot {
    big_blind: Chips,
    seat: u8,
    aggression: [u32; 6],    // my own opponent tracking, across hands (max 6 seats)
}

impl Agent for MyBot {
    fn name(&self) -> &str { "my_bot" }

    // REQUIRED — called once, before the first hand. ~10s budget.
    fn entry(&mut self, ctx: &MatchContext) {
        self.big_blind = ctx.big_blind;
        self.seat = ctx.me;
        self.aggression = [0; 6];
    }

    // REQUIRED — called every time it is your turn. 20s budget.
    fn on_action(&mut self, v: &PlayerView<'_>) -> Action {
        if v.to_call == 0 {
            return Action::Check;
        }
        if v.to_call * 4 <= v.pot && v.min_raise_to > 0 {
            return Action::Raise { to: v.min_raise_to };   // amount is a TOTAL
        }
        Action::Fold
    }

    // OPTIONAL — the only place you see opponents' cards.
    fn on_hand_end(&mut self, result: &HandResult) {
        for (seat, _hole) in result.revealed {
            self.aggression[*seat as usize] += 1;
        }
    }
}
```

### 6.2 Any other language (subprocess)

Your program is launched once per run and speaks **one JSON object per line**: read a line
from stdin, write a line to stdout. `stderr` is yours for logging and is captured for you —
never a violation.

**You must flush after every reply.** A buffered line is indistinguishable from a bot that
never answered, and this is the #1 cause of a working bot mysteriously timing out.

Full conforming Python bot:

```python
#!/usr/bin/env python3
import sys, json

state = {}

for line in sys.stdin:                      # one message per line
    msg = json.loads(line)
    t = msg["type"]

    if t == "entry":
        state["bb"] = msg["big_blind"]
        print(json.dumps({"v": 1, "type": "ready", "id": msg["id"],
                          "name": "py_bot", "language": "python"}), flush=True)

    elif t == "action_request":
        if msg["to_call"] == 0:
            reply = {"action": "check"}
        elif msg["to_call"] <= 2 * state["bb"]:
            reply = {"action": "call"}
        else:
            reply = {"action": "fold"}
        reply.update({"v": 1, "type": "action", "id": msg["id"]})
        print(json.dumps(reply), flush=True)   # FLUSH. Always.

    elif t == "shutdown":
        break
    # hand_start / hand_end expect NO reply. Ignore unknown types.
```

Rules for subprocess bots:

- **Echo the `id`** from the request in your reply. A mismatched `id` is discarded and
  treated as no answer at all.
- **Ignore message types and fields you don't recognize.** New fields will be added; this
  rule is what keeps your bot working when they are.
- **Never reply to `hand_start`, `hand_end`, or `shutdown`.**
- Include `"v": 1` in everything you send.
- Exit within 1 second of `shutdown`.
- Do all slow setup when you receive `entry`, inside its 10 s budget.

### 6.3 Messages you will receive

| `type` | Reply | Carries |
|---|---|---|
| `entry` | **`ready`** | your seat, seat count, starting stack, blinds, antes, max hands, timeout, time bank, your RNG seed |
| `hand_start` | none | hand number, button, stacks, your hole cards |
| `action_request` | **`action`** | everything in §4.2 |
| `hand_end` | none | board, chip deltas, pot winners, `revealed` showdown cards |
| `shutdown` | none | match is over — exit |

Example `action_request` and reply:

```json
{"v":1,"type":"action_request","id":77,"hand_number":42,"street":"flop","button":1,
 "hole":["Ah","Kd"],"board":["7h","2d","Kc"],
 "pot":700,"current_bet":300,"to_call":300,
 "min_raise_to":600,"max_raise_to":9700,"big_blind":100,
 "seats":[{"seat":0,"stack":9400,"street_bet":0,"committed":600,"status":"active"},
          {"seat":1,"stack":9100,"street_bet":300,"committed":900,"status":"active"}],
 "hand_log":[{"seat":1,"street":"preflop","action":"raise","amount":300},
             {"seat":0,"street":"preflop","action":"call","amount":300}],
 "time_remaining_ms":20000,"time_bank_remaining_ms":60000}
```
```json
{"v":1,"type":"action","id":77,"action":"raise","amount":900}
```

`revealed` in `hand_end` contains only cards that actually reached a showdown — a hand won
by everyone folding reveals nothing, exactly as at a real table.

---

## 7. The game

Standard **No-Limit Texas Hold'em**, by the book. Nothing here should surprise anyone who
has played.

### 7.1 Shape of a hand

1. Button posts; blinds and any ante are posted automatically.
2. Each player is dealt **2 private cards**.
3. **Preflop** betting.
4. **Flop** — 3 community cards — betting.
5. **Turn** — 1 more — betting.
6. **River** — 1 more — betting.
7. **Showdown**: best five-card hand from your 2 + the 5 board cards wins. You may use any
   combination, including all five board cards ("playing the board").
8. Button moves one seat left; next hand.

A hand ends immediately if all but one player folds; the pot is awarded with no showdown and
no cards revealed.

### 7.2 Betting rules

- **No-limit:** you may wager any amount from the minimum up to your entire stack.
- Minimum opening bet = one big blind.
- Minimum raise = the size of the previous raise, added on top — the platform computes this
  and hands it to you as `min_raise_to`. You never calculate it.
- A **short all-in** below the minimum is always allowed.
- A betting round ends when every player still in the hand has either matched the current
  bet or is all-in.
- Preflop, the big blind gets the **option** to raise even when nobody has raised.
- You can never wager more than your stack. There is no table stakes surprise: `max_raise_to`
  is always exactly your all-in.

### 7.3 Heads-up (2 players) — note the inversion

Heads-up blind order is **not** the same as a full table. With two players:

- The **button posts the small blind**, the other player posts the big blind.
- **Preflop:** the button acts **first**.
- **Flop, turn, river:** the button acts **last**.

The platform handles all of it; you are simply never asked to act out of turn. It is called
out here because strategy that assumes full-table position will be wrong heads-up.

### 7.4 All-ins and side pots

If you cannot cover a bet you may still go all-in for your stack and remain eligible for the
portion of the pot you covered. Chips beyond your reach form a **side pot** contested only by
players who covered it. Side pots are built and awarded by the platform; you never compute
them. Odd chips go to the first eligible seat left of the button.

### 7.5 Table and match format

- **2 to 6 seats.** Six is a hard maximum: the host has six performance cores, and the
  platform will not seat a bot on a slower core just to fit it in (§2.1). Heads-up is the
  default; the live demo format is **[OPEN]** (one 6-max table vs. a round-robin of heads-up
  matches). Either way you are never seated against more than five opponents.
- **Starting chips are chosen at run time** — the operator is prompted — so never hardcode a
  stack size. Read it from `entry`.
- Blinds, antes, and any escalation schedule come from `entry`.
- A run ends when one bot holds all the chips, or the hand limit is reached, or all but one
  bot is busted.
- Busted bots sit out. Whether rebuys exist is **[OPEN]**.
- **Chip conservation is verified** every hand and at match end. Chips are never created or
  destroyed; if they ever were, that is an engine bug, not a strategy.

---

## 8. Checklist before you submit

- [ ] `entry` reads `starting_stack`, blinds and seat count from the context — nothing is
      hardcoded.
- [ ] `on_action` returns within 20 s **in the worst case**, with margin.
- [ ] Deep searches are bounded by the reported remaining time, not by a fixed iteration
      count.
- [ ] Your bot is single-threaded, or at least correct and no slower when confined to one
      core (§2.1) — it is pinned to one, inescapably.
- [ ] `bet` / `raise` amounts are **totals** inside `[min_raise_to, max_raise_to]`.
- [ ] You never `check` when `to_call > 0`.
- [ ] You never `raise` when `min_raise_to == 0`.
- [ ] Randomness comes from the provided seed, not the clock.
- [ ] You handle being all-in, short-stacked, and heads-up without erroring.
- [ ] Subprocess bots: **flush after every reply**, echo the `id`, never reply to
      `hand_start` / `hand_end` / `shutdown`, ignore unknown fields.
- [ ] `holdem-cli conformance --bot <your bot>` passes. **[OPEN — harness being built]**

### Not allowed

- Reading engine memory, files, or logs to discover hidden information.
- Coordinating with another bot in the same run, in any way.
- Interfering with the platform or any other bot's process.
- Network access.

Playing badly is fine. Being slow is survivable. Cheating is not — and the interface is
built so that honest play is the only thing that is *possible*, not merely the only thing
that is permitted.
