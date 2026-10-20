# Agentic Poker Arena — Platform Design Plan

Status: design deliberation for `SPEC.md`. Nothing here is built yet. Where a decision is
genuinely open, this document states the options, the trade-off, and a recommendation
marked **→ Recommend**. Toolchain on this machine is `rustc 1.75.0`, so the plan assumes
**edition 2021** (no edition-2024 features).

---

## 0. The three tensions this design has to resolve

Before any code, three things in the brief pull against each other. Every structural
decision below falls out of resolving them.

1. **"Highly performant" vs. "20-second time limit."** These describe different
   operating modes, not one. A 20s-per-decision budget is a *tournament/demo* concern
   (hundreds of hands, human-watchable). "Millions of hands per second" is a
   *benchmark/self-play* concern (trivial agents, no supervision). Trying to serve both
   with one code path means either paying thread-supervision cost on every action
   forever, or having no enforceable limit at all. **Resolution: one `Agent` trait, two
   supervisors** (§4).

2. **"Highly performant" vs. "enforce a deadline."** You cannot preempt a synchronous
   Rust function. An agent with `loop {}` in `on_action()` hangs the engine permanently
   if called directly. The only real options are cooperative honesty (measure after the
   fact — useless against a hang) or running the agent on another thread and timing out
   the *wait*. **Resolution: persistent per-agent worker threads**, spawned once at
   `entry()`, driven by channel round-trips (§4.2). This amortizes thread spawn to zero
   and keeps per-action overhead at a couple of microseconds.

3. **Heads-up (SPEC) vs. "every bot's chips" (brief).** The SPEC says heads-up; the
   brief implies a table of N bots. **Resolution: the engine is generic over
   `2..=MAX_SEATS` seats with heads-up as the default demo config** (§5.4), where
   `MAX_SEATS == 6` — **DECIDED**, set by the hardware fairness cap in §B.4, not by the
   rules. The only places N matters are blind posting, action order, and side pots. Writing
   it general over N costs nothing at runtime and unlocks both "six agents at a 6-max table"
   and "round-robin of heads-up matches" as demo formats. Heads-up blind rules are a genuine
   special case and are called out explicitly.

---

## 1. Crate layout

```
agentic-holdem/                 # cargo workspace
  crates/
    holdem-core/      # cards, deck, hand evaluator, pot/side-pot math, betting legality.
                      # Pure functions + plain data. No agents, no threads, no I/O.
    holdem-agent/     # THE PUBLIC CONTRACT: `Agent` trait, `Action`, `PlayerView`,
                      # `MatchContext`, `HandResult`. Tiny, stable, few deps.
    holdem-engine/    # hand loop, match loop, supervision, timing, invalid-action policy,
                      # match logging. Depends on core + agent.
    holdem-agents/    # the ~6 example agents.
    holdem-cli/       # binary: prompts for starting chips, runs matches, prints report.
  tests/              # integration: scripted hands with known winners, misbehavior suite.
  benches/            # criterion: evaluator throughput, engine hands/sec, action overhead.
```

**Why split `holdem-agent` out as its own crate** — this is the decision I'd most defend.
A bot author depends on `holdem-agent` *only*. They cannot name an engine internal, so
they cannot reach into the deck, read another seat's hole cards, or mutate pot state —
not by policy, but because those types aren't in their dependency graph. It also means
engine internals can churn freely without breaking six example agents and whatever
students write later. The cost is one extra crate boundary.

---

## 2. The agent interface

### 2.1 Core types (`holdem-agent`)

```rust
pub type Chips = u64;      // integer chips, always. Never f64 — chip conservation must be exact.
pub type SeatId = u8;      // index into the table, 0..n_seats

/// A card as 0..=51. rank = c >> 2 (0=Two .. 12=Ace), suit = c & 3.
/// Chosen so rank/suit extraction is one shift/mask and a 52-card set is a u64 bitmask.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub struct Card(pub u8);

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Street { Preflop, Flop, Turn, River }

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Fold,
    Check,
    Call,                      // engine computes the amount; agent never states it
    Bet   { to: Chips },       // opening wager on a street where current_bet == 0
    Raise { to: Chips },       // TOTAL this street, not an increment (see §2.4)
    AllIn,                     // sugar: shove whole stack, always well-defined
}
```

### 2.2 What the agent is shown

`on_action` receives a **borrowed view** built from engine-owned storage. Borrowed, not
owned, because constructing an owned snapshot per action would allocate — and at a
million actions/sec that allocation *is* the hot loop.

```rust
pub struct PlayerView<'a> {
    // ---- identity & position
    pub me: SeatId,
    pub button: SeatId,
    pub street: Street,

    // ---- private: only ever this agent's own two cards
    pub hole: [Card; 2],
    pub board: &'a [Card],            // len 0 / 3 / 4 / 5

    // ---- money
    pub pot: Chips,                   // committed on previous streets + this street
    pub current_bet: Chips,           // highest total wagered by any seat this street
    pub to_call: Chips,               // current_bet - my street_bet, clamped to my stack
    pub min_raise_to: Chips,          // smallest legal Raise{to}; == 0 if raising is illegal
    pub max_raise_to: Chips,          // my stack + my street_bet (i.e. all-in)
    pub big_blind: Chips,

    // ---- table, same order as seats, every seat visible
    pub seats: &'a [SeatPublic],

    // ---- history: this agent may ignore it, or model opponents from it
    pub hand_log: &'a [LoggedAction], // every action this hand, in order, all seats
    pub hand_number: u32,

    // ---- clock (see §3)
    pub deadline: Instant,            // hard wall for this decision
    pub time_bank_remaining: Option<Duration>, // None once spent
}

pub struct SeatPublic {
    pub seat: SeatId,
    pub stack: Chips,          // behind, not counting street_bet
    pub street_bet: Chips,     // wagered this street
    pub committed: Chips,      // wagered this hand, all streets
    pub status: SeatStatus,    // Active | Folded | AllIn | Busted | Disqualified
}

pub struct LoggedAction { pub seat: SeatId, pub street: Street, pub action: Action, pub amount: Chips }
```

Note what is **absent**: the deck, the shuffle seed, any other seat's hole cards, and any
`&mut` anything. Hidden information is hidden by construction. Per the brief, opponent
modelling is opt-in: the view hands over `hand_log` and `seats`, and whether the agent
stores a history across hands is entirely its own business.

### 2.3 The trait

```rust
pub trait Agent: Send {
    /// Stable identifier for logs and reports.
    fn name(&self) -> &str;

    /// REQUIRED. Called exactly once per match, before any hand is dealt.
    /// Allocate lookup tables, size buffers, seed an RNG, read config.
    fn entry(&mut self, ctx: &MatchContext);

    /// REQUIRED. The decision. Must return before `view.deadline`.
    fn on_action(&mut self, view: &PlayerView<'_>) -> Action;

    // ---- optional hooks, defaulted to no-ops so the required surface stays at two ----

    fn on_hand_start(&mut self, _view: &PlayerView<'_>) {}

    /// Called after every hand, including hands this agent folded out of.
    /// This is the ONLY place revealed showdown hole cards are delivered.
    fn on_hand_end(&mut self, _result: &HandResult) {}
}

pub struct MatchContext {
    pub me: SeatId,
    pub n_seats: u8,
    pub starting_stack: Chips,
    pub small_blind: Chips,
    pub big_blind: Chips,
    pub ante: Chips,
    pub max_hands: Option<u32>,
    pub action_timeout: Duration,      // 20s by default
    pub time_bank: Option<Duration>,   // one-shot extension for this whole run
    pub rng_seed: u64,                 // per-agent child seed; reproducible, independent of the deck
}
```

**Debating `entry()`.** You floated `entry()` as initialization and I agree it should
exist, but it should be an **init hook on `&mut self`, not a constructor.** A constructor
(`fn entry(cfg) -> Self`) forces `Self: Sized`, which makes the trait non-object-safe, so
the engine couldn't hold `Vec<Box<dyn Agent>>` — the whole point of a pluggable arena.
Construction instead goes through a registry of factory functions
(`fn() -> Box<dyn Agent>`), and `entry()` does the per-match setup. This also matches the
SPEC's own wording ("requiring implementations of `entry()` and `on_action()`") and gives
a clean place to reset state between matches in a round-robin.

**`on_hand_end` — DECIDED, in.** An agent can reconstruct everything about the betting
from `hand_log`, but it can *never* learn a mucked or shown hole card from within
`on_action` — that information only exists after the hand resolves. Without this hook, no
agent can do opponent hand-range modelling at all, which guts the most interesting
strategy class. It stays defaulted to a no-op, so the *required* surface is still exactly
`entry()` + `on_action()`; an agent that doesn't care about showdowns never writes it.

`HandResult` carries: the hand number, each seat's chip delta and resulting stack, the
final board, who won which pot layer, and `revealed: &[(SeatId, [Card; 2])]` — populated
only for cards that genuinely went to showdown. Hands that ended by everyone folding
reveal nothing, exactly as at a real table.

**Why `Send` on the trait.** Required for the worker-thread supervisor (§4.2). It is a
near-free constraint — any agent holding plain data satisfies it — and it also rules out
`Rc`-based shared state between two agents in the same match, which would be an accidental
collusion channel.

### 2.4 `Raise { to }` vs `Raise { by }`

→ **Recommend `to` (total this street).** The no-limit raise rules are written in terms of
totals ("raise to at least the current bet plus the last raise size"), so `to` matches the
legality check with no arithmetic in between. `by` requires the agent to know
`current_bet` and reconstruct the total, and the classic bug — confusing "raise by 100"
with "raise to 100" — becomes an *invalid action penalty* instead of a type error. With
`to`, the view hands the agent `min_raise_to` and `max_raise_to` and a legal raise is
literally any value in that inclusive range.

---

## 3. The clock: 20 seconds and a one-per-run time bank

### 3.1 Definitions

- **Action timeout:** 20s wall-clock, measured from when the engine hands the view to the
  agent until the decision comes back. Configurable; `0`/`None` disables enforcement.
- **Run:** one match from `entry()` to the final hand. The time bank is **per agent, per
  run — not per hand**, exactly as specified. `entry()` being called again (new match in
  a round-robin) resets it.
  - Open knob: `TimeBankScope::{ Match, Session }`. → **Recommend `Match`**, since it's
    tied to `entry()` and keeps a round-robin symmetric; expose `Session` as config for
    anyone who wants a stingier tournament.
- **Time bank size:** `MatchContext::time_bank`, default 60s.

### 3.2 How the bank activates — the one real debate here

**Option A — explicit request.** Add `Action::RequestTimeBank`; the agent returns it, the
engine re-calls `on_action` with a longer deadline.
*Against:* two round-trips; the agent must predict it'll need the time *before* it starts
thinking; and a buggy agent can return it after the bank is spent, which is now another
invalid-action case to specify. It also doesn't actually help a hung agent, which is the
case the clock exists for.

**Option B — automatic on first timeout.** The agent blocks past 20s; if its bank is
unspent, the engine simply waits again for `time_bank` more, marks the bank spent, and
accepts a decision that arrives inside the extension.
*For:* this is how a real time bank works — it engages when your clock runs out, you
don't press anything first. Zero protocol surface: with the worker-thread supervisor it is
*literally a second `recv_timeout` call*. It cannot be gamed, it needs no new `Action`
variant, and it still protects against hangs.
*Against:* an accidentally-slow agent can burn its bank without meaning to.

→ **Recommend Option B**, with `time_bank_remaining` exposed in `PlayerView` so a
deliberate agent can see the bank is still there and budget a deep search accordingly. The
"against" is mitigated by that visibility, and the simplicity win is large.

### 3.3 Letting agents see the clock

`PlayerView` carries `deadline: Instant`. This is a small thing that makes a whole agent
class possible: iterative-deepening search / Monte-Carlo rollouts that loop
`while Instant::now() < view.deadline - margin`. It costs one field and makes "use your 20
seconds well" an actual strategic axis rather than a tripwire.

---

## 4. Supervision: one trait, two execution modes

### 4.0 What this section is actually about (plain version)

The trait in §2 is the **bot author's** side of the contract: "write a struct with an
`on_action` method." This section is the **engine's** side: *how the engine calls that
method.* Those are separate concerns, and conflating them is what makes the 20-second rule
hard.

Here is the problem, concretely. The obvious way for the engine to get a decision is:

```rust
let action = agent.on_action(&view);   // a normal function call
```

That is fast — a single indirect call, nanoseconds. But now ask: **where would the
20-second timeout go?** There is nowhere to put it. Once control enters `on_action`, the
engine is not running. It cannot check a clock, because it has no code executing. If the
bot does `loop {}`, that line never returns and the whole tournament is frozen — forever.
Rust (and every other language without green-thread preemption) gives you no way to yank
control back out of a function that refuses to return. You can time the call *afterwards*
and say "that took 25s, penalty" — which works for a merely slow bot, but does nothing at
all for a hung one, and a hung bot is precisely what a timeout is for.

The fix is to not call it on the engine's own thread. Put the bot on a **second thread**,
hand it work over a channel, and have the engine wait on the *reply* with a timeout:

```rust
tx.send(view_snapshot).unwrap();                  // "your turn"
match rx.recv_timeout(Duration::from_secs(20)) {  // engine is still alive here
    Ok(action) => action,                         // decided in time
    Err(_)     => penalize(),                     // too slow, panicked, or hung
}
```

Now the engine always has code running — it is sitting in `recv_timeout`, holding a clock.
The bot can hang all it likes; the engine stops waiting at 20 seconds and moves on. This
also gets panic containment for free: a bot that panics kills only its own thread, the
channel hangs up, and `recv_timeout` returns an error the engine can treat like any other
failure.

So why not just always do the thread version? Because it is not free: a channel
round-trip plus an owned copy of the game state, every single action. Irrelevant when the
budget is 20 seconds; ruinous when you want a million hands per second for self-play or a
benchmark, where the bot's own decision takes ~50ns and the supervision would cost 50× the
thing it supervises. It also makes runs non-reproducible, since a timeout depends on how
loaded the machine is.

**"Supervisor" is just a name for which of those two calling strategies the engine uses.**
It is an enum with two variants, chosen by config at match start. Bots are completely
unaware of it — they implement `Agent` once and the same code runs identically under
either. Tournaments and the live demo use the thread version because the rules need
enforcing; benchmarks and reproducible regression tests use the direct call because
nothing needs enforcing and speed and determinism matter.

### 4.0.1 The two modes at a glance

| | `Direct` | `Guarded` |
|---|---|---|
| How the bot is called | plain method call | channel round-trip to a worker thread |
| Overhead per action | ~0 (one indirect call) | ~µs (2 channel hops + state copy) |
| 20s deadline enforced | no | yes |
| Time bank | n/a | yes |
| Bot panics | takes down the process | contained, seat penalized |
| Bot hangs forever | engine frozen | thread abandoned, seat auto-folds |
| Bit-exact replay | yes | no (timing is wall-clock) |
| Used for | benches, self-play, golden-log tests | tournaments, the live demo |

```rust
pub enum Supervisor {
    /// Direct call. Zero overhead, no deadline enforcement, fully deterministic.
    /// For benchmarks, self-play, and reproducible regression tests.
    Direct,
    /// Persistent worker thread per agent. Enforces the 20s deadline and the time bank,
    /// survives agent panics and hangs. For tournaments and the live demo.
    Guarded { timeout: Duration, time_bank: Option<Duration> },
}
```

### 4.1 `Direct`

`agent.on_action(&view)` — a single dynamic dispatch. A panic takes the process down
(acceptable: this mode is for trusted code and tests), and an infinite loop hangs. Used
wherever throughput or bit-exact reproducibility matters.

### 4.2 `Guarded` — persistent worker threads

The design I'd build. At `entry()`, each agent moves onto its own thread that parks on a
channel for its whole life:

```
engine thread                         agent thread (one per seat, spawned once)
-------------                         ----------------------------------------
send(Request { view_owned })  ───────► recv() → agent.on_action(&view) → send(Action)
rx.recv_timeout(20s)          ◄───────
  Ok(action)        → validate (§5.5)
  Err(Timeout)      → if bank unspent { rx.recv_timeout(bank); mark spent } else PENALIZE
  Err(Disconnected) → agent panicked → PENALIZE, respawn or mark Disqualified
```

Properties that make this the right call:

- **Spawn cost amortized to zero.** One thread per agent per match, not per action. Thread
  creation is ~10–30µs; paying it once per match instead of once per decision is the
  difference between a usable guarded mode and an unusable one.
- **Per-action overhead is two channel hops**, order of a few microseconds — negligible
  against a 20s budget, and the `Direct` mode exists for when it isn't.
- **Panics are contained.** A panicking agent kills only its own thread; the engine sees
  `Disconnected`, penalizes, and the match continues.
- **Hangs are survivable, if leaky.** A thread stuck in `loop {}` cannot be killed — that's
  a hard OS/Rust reality, not a design flaw. The engine abandons it (one leaked thread),
  marks the seat `Disqualified`, and auto-folds that seat for the rest of the match. Memory
  is reclaimed at process exit. This is the honest cost of in-process agents and should be
  documented rather than papered over.
- **Forward-compatible with real sandboxing.** Because supervision is already a
  *message protocol* rather than a function call, swapping the channel for a subprocess
  pipe or a WASM instance later is a `Supervisor` variant, not a rewrite. Worth noting in
  the report as future work: in-process Rust trait objects fundamentally cannot be
  memory- or CPU-limited.

**Cost accepted:** `Guarded` must hand the worker an *owned* snapshot (the borrowed
`PlayerView<'a>` can't cross a thread boundary), so it allocates per action. Mitigate by
reusing per-seat buffers and sending a pooled owned struct. `Direct` keeps the zero-alloc
borrowed path. This asymmetry is why both modes exist.

### 4.3 Why not `async`/tokio

Agents are CPU-bound. An async task that blocks the executor cannot be cancelled either,
so tokio buys no enforcement a thread doesn't, while adding a runtime, coloured functions,
and an `async_trait` boxing allocation on the hot path. `recv_timeout` on an OS thread is
the right-sized tool.

---

## 5. Engine design

### 5.1 Determinism and reproducibility

```rust
pub enum DeckSource {
    Shuffled { seed: u64 },        // ChaCha8Rng, Fisher–Yates — same seed ⇒ same cards, forever
    Scripted(Vec<Card>),           // predetermined hands with known winners, for tests/demo
}
```

Critically, **RNG streams are separated**: the deck RNG is derived from the master seed,
and each agent gets an *independent* child seed (`MatchContext::rng_seed`). An agent can
therefore be randomized and reproducible without being able to infer the deck from its own
RNG state. A single `(master_seed, MatchConfig, [agent names])` tuple reproduces a match
exactly under `Direct`.

**Honest caveat:** under `Guarded`, wall-clock timeouts are nondeterministic by nature — a
loaded machine can make a borderline agent time out on one run and not the next. So:
reproducible regression runs use `Direct`; the live demo uses `Guarded` and records every
timeout, time-bank activation, and penalty into the match log so a run can be *audited*
even when it can't be bit-replayed. Don't claim replay determinism for guarded runs.

### 5.2 Hand evaluation — the performance centrepiece

Card as `u8`, a hand as a `u64` bitmask (4 suits × 13 ranks) so flush detection is a
popcount per suit nibble-block and straight detection is a shift-and-mask on the
rank-union.

Candidates to bake off (SPEC already commits to evaluating existing libraries and
documenting reuse):

| Approach | Table size | Speed | Notes |
|---|---|---|---|
| Naïve 7-choose-5 + categorize | 0 | slow (~µs) | **Build this anyway** as the reference oracle |
| Bitmask + rank-table hybrid | tens of KB | fast | easy to write in-repo, no dependency |
| Cactus-Kev / perfect-hash family (e.g. `rs-poker`, `poker`, SKPokerEval-style) | ~KB–MB | very fast (~ns) | needs license/attribution audit |
| Two-plus-two 7-card lookup chain | ~130 MB | fastest (7 array reads) | table generation + load time + memory is a real cost |

→ **Recommend:** define an internal `HandEvaluator` abstraction, ship the naïve evaluator
as the correctness oracle, implement the bitmask hybrid as the default (so the repo has a
clear own contribution and no hard external dependency), then benchmark one or two crates
behind the same abstraction and switch the default only if the win is measured. Skip the
two-plus-two table unless benchmarks say the 130MB is worth it — it is almost certainly
not, given the engine, not the evaluator, will dominate at this scale.

**Validation gate before the evaluator is trusted:** all 2,598,960 five-card hands
exhaustively ranked and cross-checked against the oracle, plus several million random
7-card hands, comparing *relative ordering* (not just category) in both directions.

### 5.3 Hot-loop performance budget

Targets to hold ourselves to, as *budgets to measure*, not claims:

- Evaluator: ≥ 10M 7-card evaluations/sec/core.
- Engine under `Direct` with trivial agents: ≥ 1M hands/sec/core.
- `Guarded` per-action supervision overhead: ≤ 5µs.
- **Zero heap allocation in the `Direct` steady state.**

Discipline that gets us there: fixed-size arrays over `Vec` for anything bounded by seat
count (`[SeatPublic; MAX_SEATS]` + a live length) and by board size (`[Card; 5]`); `Vec::clear()`
to reuse the action-log capacity across hands rather than reallocating; `dyn` dispatch only
at the `Agent` boundary and never inside the evaluator or pot math; `#[inline]` on card and
mask helpers; and a `criterion` bench plus a `#[should_panic]`-style allocation-counter test
guarding the no-alloc invariant so a regression is caught, not discovered.

### 5.4 Betting mechanics

- **Round loop:** track `current_bet`, `min_raise` (size of the last raise), and
  `last_aggressor`. A street closes when action returns to `last_aggressor` and every
  non-folded, non-all-in seat has `street_bet == current_bet`. The subtle case is the big
  blind's option preflop when the pot is unraised — handle explicitly, it is a classic bug.
- **Heads-up special case (required, and easy to get wrong):** with two players the button
  posts the *small* blind, acts *first* preflop, and acts *last* on every later street.
  This inverts the N≥3 ordering and deserves its own targeted test.
- **Side pots** for multiway all-ins: take each seat's `committed` total, sort the distinct
  all-in levels ascending, and for each level form a layer of
  `Σ min(committed_i, level) − (levels already accounted)`; eligibility for a layer is
  every non-folded seat with `committed ≥ level`. Award each layer to the best hand among
  its eligible seats, odd chips to the first seat left of the button. Heads-up never needs
  this, which is exactly why it must be tested on a 3+ seat scripted hand. With
  `MAX_SEATS = 6` the number of side-pot layers is bounded at 6, so the layer buffer is a
  fixed-size array like everything else.
- **Chip conservation:** `debug_assert!` after every hand that
  `Σ stacks + pot_in_flight == initial_total`, and assert it again as a hard check at
  match end in all builds. SPEC calls this out explicitly; make it an invariant, not a
  spot check.
- **Termination guarantee:** a match ends when one seat holds all chips, or `max_hands` is
  reached, or all but one seat is busted. Blinds and antes move chips every hand even when
  every agent auto-folds, so progress is guaranteed; optional blind escalation bounds it
  further.

### 5.5 Invalid behaviour: a two-tier policy

Per the brief, the penalty is **check if no bet is outstanding, fold if there is.** The
part worth deliberating is *what counts as invalid*, because several "wrong" actions have
an unambiguous poker-standard meaning and penalizing them is needlessly harsh.

**Tier 1 — Normalize** (silently coerced, counted as `normalizations` in the report):

| Agent returns | Engine does |
|---|---|
| `Call` with `to_call >= my stack` | all-in call for the stack |
| `Raise{to}` where `to > max_raise_to` | clamp to all-in |
| `Raise{to}` where `0 < to < min_raise_to` but `to == max_raise_to` | legal short all-in |
| `Bet{to}` when `current_bet == 0` and `to >= min_bet` | normal bet |
| `Check` when `to_call == 0` | legal, obviously |
| `AllIn` | `Raise{to: max_raise_to}` or an all-in call when short |

**Tier 2 — Penalize** (`Check` if `to_call == 0`, else `Fold`; counted as `violations`):

- Decision arrives after the deadline and the time bank is spent or already used.
- Agent thread panicked (`Disconnected`).
- `Check` while `to_call > 0`.
- `Raise{to}` below `min_raise_to` while the agent still has more chips than that.
- `Bet{to}` when `current_bet > 0` (incoherent: that's a raise), or `Bet{to: 0}`.
- `Raise`/`Bet` while already all-in or folded, or any action from a seat not to act.

**Repeat offenders.** → **Recommend: no three-strikes rule.** An agent that keeps
violating simply keeps folding, bleeds blinds, and busts out naturally — the match
terminates on its own and the report shows a violation count next to its stack, which is
more informative than a disqualification flag. The *only* status that needs special
handling is `Disqualified`, reserved for a thread that hung and can no longer be consulted
at all; that seat is auto-folded without a channel round-trip for the remainder.

Every normalization and violation is recorded with seat, hand number, street, the action
requested, and the action substituted — so the demo can literally show "errors default to
folding," as the SPEC promises.

---

## 6. How a run works in practice

```
$ cargo run -p holdem-cli -- match --agents tight,loose,calling_station,random,heuristic,mcts
Starting chips per bot? 10000
Small/big blind [50/100]:
Action timeout [20s]:  Time bank per run [60s]:  Seed [random]:

  hand   1  BTN=tight   board 7h 2d Kc Ts
     tight      raise to 300
     loose      call 300            (3.1s)
     mcts       fold
     random     TIMEOUT 20.0s → time bank engaged (60s) → raise to 900   [bank spent]
     ...
  hand  42  calling_station INVALID Check facing 400 → FOLD  [violation 3]

=== match report =========================================
 agent             chips     bb/100   VPIP   PFR   AF   timeouts  bank  viol  norm  p99 ms
 mcts              24,310     +18.2   24.1  19.8  2.4         0  used     0     2   19,840
 tight             17,905      +7.1   15.2  13.1  3.1         0  held     0     0       12
 ...
 chip conservation: 60,000 == 60,000  OK
 log written: runs/2026-10-20T15-41-03_seed-8841203.jsonl   (replay: --replay <file>)
```

Flow: CLI prompts for starting chips → builds `MatchConfig` → instantiates agents from the
registry → `entry()` on each (spawning its worker thread under `Guarded`) → the hand loop
deals from the seeded deck, drives betting streets, calls `on_action` through the
supervisor, resolves the pot via the evaluator, fires `on_hand_end` with revealed cards →
repeat until a termination condition → write the JSONL log and print the report.

A minimal conforming agent, in full:

```rust
pub struct CallingStation { bb: Chips }

impl Agent for CallingStation {
    fn name(&self) -> &str { "calling_station" }
    fn entry(&mut self, ctx: &MatchContext) { self.bb = ctx.big_blind; }
    fn on_action(&mut self, v: &PlayerView<'_>) -> Action {
        if v.to_call == 0 { Action::Check } else { Action::Call }
    }
}
```

### 6.1 The six example agents

Chosen to span the strategy space *and* to exercise different engine paths:

1. **`always_fold`** — baseline floor; makes bb/100 interpretable.
2. **`calling_station`** — never folds; drives showdowns and side-pot code.
3. **`random`** — uniform over legal actions from its child RNG; the fuzzing workhorse.
4. **`tight_aggressive`** — hand-strength table preflop, pot-odds postflop; the "real" baseline.
5. **`pot_odds_equity`** — Monte-Carlo equity vs. a random opponent range, compared against
   pot odds. First agent that actually *uses* its 20 seconds.
6. **`time_bank_mcts`** — iterative-deepening rollouts bounded by `view.deadline`, with a
   deliberate deep search on big river decisions. Exercises the time bank end to end.

Plus a non-shipped **misbehaviour suite** used only in tests: `panicking`, `sleeping`
(45s), `hanging` (`loop {}`), `illegal` (always `Check`), `nonsense`
(`Raise{to: u64::MAX}`).

---

## 7. Testing and verification

1. **Scripted hands with known winners** — `DeckSource::Scripted`, asserting the exact
   winner and chip delta. Must include: heads-up blind order, BB option preflop,
   split pot, three-way all-in with two side pots, odd-chip assignment, board-plays
   (counterfeited hole cards), and a straight/flush tie broken correctly.
2. **Evaluator cross-validation** — exhaustive 5-card against the oracle, millions of
   random 7-card, ordering compared both directions (§5.2).
3. **Property tests** (`proptest`): over random seeds and **every seat count 2–6
   exhaustively** (cheap, now that `MAX_SEATS = 6`) — chips conserved;
   `Σ seat.committed == pot`; the engine never offers an action set the legality checker
   would reject; every `on_action` return either normalizes or penalizes but never panics
   the engine.
4. **Misbehaviour suite** — each bad agent plays a full match under `Guarded`; assert the
   match completes, chips conserve, and the penalty log matches expectation exactly.
5. **Golden logs** — a fixed-seed `Direct` match's JSONL is committed; a diff is a
   regression.
6. **Benches** (`criterion`) against the §5.3 budgets, run in CI so a throughput
   regression is visible.

---

## 8. Open questions for you

1. **Table format for the demo:** six agents at one 6-max table, or a round-robin of
   heads-up matches with aggregate standings? The SPEC says heads-up; the engine supports
   both, and the §B.4 cap of `MAX_SEATS = 6` means a 6-max table *exactly* fills the host's
   six performance cores — one bot per core, nothing idle, nothing unfair. Round-robin gives
   cleaner head-to-head numbers; a single table is a better live demo and a better use of
   the machine.
2. **Time-bank scope** — reset per match (recommended) or one per CLI invocation across a
   whole round-robin?
3. **Bust-out / rebuy:** does a busted agent sit out, or rebuy to the starting stack for a
   fixed number of hands (which makes bb/100 much more statistically meaningful at these
   sample sizes)?
4. **Tier-1 normalization** — accept it, or do you want the stricter reading where *any*
   deviation, including an over-sized raise, is a penalty? Stricter is simpler to explain
   in a demo; normalization is kinder to student bots.
5. Confirm **`Raise { to }`** (total) over `by` — this shapes every agent anyone ever writes.

---

# Addendum A — The language-agnostic contract

Added after the §0–§8 design was accepted. This addendum exists because the §4 supervisor
design has a property worth cashing in: **`Guarded` already talks to agents by passing
messages, not by calling functions.** A native Rust agent and a subprocess written in any
language differ only in the transport those messages cross. So the contract can be defined
once, semantically, and bound to two transports.

Nothing in §0–§8 changes. This is an additional binding, not a redesign.

## A.1 Two layers

```
         ┌─────────────────────────────────────────────────┐
         │  SEMANTIC CONTRACT  (§A.3 — the real spec)      │
         │  messages, their fields, their ordering,        │
         │  legality, timing, penalties                    │
         └─────────────────────────────────────────────────┘
                  │                            │
      ┌───────────▼────────────┐   ┌───────────▼─────────────┐
      │ BINDING 1: native      │   │ BINDING 2: subprocess   │
      │ Rust trait, in-process │   │ JSONL over stdin/stdout │
      │ (§2's `Agent`)         │   │ any language            │
      │ Supervisor::{Direct,   │   │ Supervisor::Subprocess  │
      │              Guarded}  │   │                         │
      └────────────────────────┘   └─────────────────────────┘
```

The semantic contract is normative. A binding is normative only in how it encodes that
contract. **A bot is conforming if its message behaviour is indistinguishable from a
conforming native agent's** — it must not be possible to tell from the engine's side which
binding a bot used, except in latency.

The `Supervisor` enum therefore grows a third variant, and that is the *entire* engine-side
cost of language agnosticism:

```rust
pub enum Supervisor {
    Direct,
    Guarded    { timeout: Duration, time_bank: Option<Duration> },
    Subprocess { argv: Vec<String>, cwd: PathBuf, timeout: Duration,
                 time_bank: Option<Duration>, handshake_timeout: Duration },
}
```

## A.2 Why JSONL over stdio, and not the alternatives

| Transport | Verdict |
|---|---|
| **Newline-delimited JSON on stdin/stdout** | → **Recommend.** Every language has JSON and stdio in its standard library, with zero dependencies and no build step. Trivially debuggable: a log line *is* the wire format, and a bot can be driven by hand with `echo`. Framing is "read a line." |
| gRPC / protobuf | Faster and typed, but imposes a codegen toolchain on every bot author. Hostile to a student writing 40 lines of Python. |
| HTTP / REST per decision | Needs a server, a port, and bind/teardown per bot; connection setup noise lands inside the 20s budget. |
| Unix domain sockets, raw | Same framing work as stdio with worse portability and no free debuggability. |
| FFI / C ABI shared objects | Fastest cross-language option, but a segfault takes the engine down and every language needs its own glue. |

JSON's one real cost is that chip amounts must stay within exact-integer range. Mandated
below: all amounts are non-negative integers, and the engine must reject a config whose
total chips exceed 2^53 − 1 so no conforming JSON decoder can lose precision.

## A.3 The semantic contract

### A.3.1 Encodings (binding-independent)

| Concept | Encoding |
|---|---|
| Card | String, rank then suit, both case-sensitive: rank `23456789TJQKA`, suit `shdc`. e.g. `"Ah"`, `"Td"`, `"2c"`. The native binding's `Card(u8)` must map to exactly this string set. |
| Chips | JSON integer, `>= 0`. No floats, no strings, no nulls. |
| Seat | JSON integer `0 .. n_seats-1`, stable for the whole match. |
| Street | `"preflop" | "flop" | "turn" | "river"` |
| Seat status | `"active" | "folded" | "allin" | "busted" | "disqualified"` |
| Durations | Integer **milliseconds**. |
| Unknown fields | A bot **must** ignore message fields it does not recognize. This is the forward-compatibility rule that lets the engine add fields without breaking old bots. |

### A.3.2 Message set

Engine → bot, five kinds. Bot → engine, two kinds. That is the whole protocol.

| # | Direction | `type` | When | Reply required |
|---|---|---|---|---|
| 1 | E→B | `entry` | once, before any hand | `ready` |
| 2 | E→B | `hand_start` | once per hand, to every seated bot | none |
| 3 | E→B | `action_request` | each time it is this bot's turn | `action` |
| 4 | E→B | `hand_end` | once per hand, to every seated bot, incl. folded | none |
| 5 | E→B | `shutdown` | once, at match end | none; exit within 1s |
| 6 | B→E | `ready` | answering `entry` | — |
| 7 | B→E | `action` | answering `action_request` | — |

Every message carries `"v": 1` (protocol version) and `"type"`. Every
request/reply pair carries a matching `"id"` integer; **a reply whose `id` does not match
the outstanding request is discarded and the bot is treated as not having answered.** This
is what keeps a bot that falls a message behind from drifting permanently out of sync and
"answering" with stale decisions.

### A.3.3 `entry` → `ready`

```json
{"v":1,"type":"entry","id":1,"me":0,"n_seats":2,"starting_stack":10000,
 "small_blind":50,"big_blind":100,"ante":0,"max_hands":500,
 "action_timeout_ms":20000,"time_bank_ms":60000,"rng_seed":8841203887,
 "hand_history_provided":true}
```
```json
{"v":1,"type":"ready","id":1,"name":"my_bot","author":"cdd2847","language":"python"}
```

`rng_seed` is the bot's **own** independent stream, derived from the master seed but
cryptographically separated from the deck's — randomize freely and stay reproducible
without being able to infer the deck. `name` from `ready` is what appears in reports.

### A.3.4 `action_request` → `action`

This is the only latency-critical message and mirrors §2.2's `PlayerView` exactly.

```json
{"v":1,"type":"action_request","id":77,
 "hand_number":42,"street":"flop","button":1,
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

- `action` ∈ `"fold" | "check" | "call" | "bet" | "raise" | "allin"`.
- `amount` is **required** for `bet` and `raise`, and is the **total wagered this street**
  (the `to`-semantics of §2.4), not an increment. Ignored, and may be omitted, for the
  other four.
- `min_raise_to == 0` means raising is not legal right now; `to_call == 0` means checking
  is legal. A bot needs no rule knowledge beyond these two fields to pick a legal action.
- `time_remaining_ms` replaces the native binding's `deadline: Instant`, which cannot cross
  a process boundary. Same purpose: it is what lets a search bot bound its own rollouts.

### A.3.5 `hand_start`, `hand_end`, `shutdown`

`hand_start` carries the hand number, button, blind levels, every seat's stack, and this
bot's hole cards — enough to set up per-hand state before the first decision.

`hand_end` is the §2.3 `on_hand_end` hook on the wire, and the only place hole cards other
than your own ever appear:

```json
{"v":1,"type":"hand_end","id":0,"hand_number":42,"board":["7h","2d","Kc","Ts","3h"],
 "deltas":[{"seat":0,"delta":-900,"stack":8500},{"seat":1,"delta":900,"stack":10900}],
 "pots":[{"amount":1800,"eligible":[0,1],"winners":[1],"hand_class":"two_pair"}],
 "revealed":[{"seat":1,"hole":["Kh","Ks"]}]}
```

`revealed` contains **only** cards that genuinely reached a showdown. A hand won when
everyone folded reveals nothing, exactly as at a real table. Neither `hand_start` nor
`hand_end` nor `shutdown` expects a reply, and a bot that writes one commits a protocol
violation (§A.3.7) — though an unsolicited line with a non-matching `id` is simply
discarded by the `id` rule, which makes this failure mode self-limiting.

### A.3.6 Timing — where the clock starts and stops

Precisely, because this is the most gameable part of any subprocess protocol:

1. **Process startup is free, and bounded separately.** The engine spawns the bot, then
   sends `entry` and waits `handshake_timeout` (default **10s**, distinct from the action
   clock) for `ready`. Interpreter startup, model loading, and table generation belong
   here. No `ready` in time → the bot never enters the match and is reported as
   `failed_to_start`.
2. **The action clock** is measured from the instant the engine finishes writing
   `action_request` to the instant it finishes reading a well-formed `action` line.
   Serialization, pipe transit, and the bot's own JSON parsing are all **inside** the
   budget. This is deliberate: it is the only definition the engine can measure honestly,
   and it keeps a bot from offloading work into a "free" phase. Expect low single-digit
   milliseconds of unavoidable overhead against a 20,000ms budget.
3. **The time bank is per run, not per hand** (§3.1), and activates automatically on the
   first timeout (§3.2) — on the wire this is just a second read with a longer deadline, no
   extra message. Identical semantics to the native binding.
4. **Bots must flush.** A line sitting in a language's default-buffered stdout is
   indistinguishable from a bot that never answered. Every quick-start (§A.5) sets
   unbuffered or flushes explicitly; this is the single most likely cause of a conforming
   bot's first mysterious timeout, and belongs in the agent-facing doc in bold.

### A.3.7 Failure taxonomy for subprocess bots

The §5.5 two-tier policy applies unchanged — Tier 1 normalizes, Tier 2 penalizes with
**check if `to_call == 0`, else fold.** Subprocess-only failures all map onto Tier 2:

| Failure | Handling |
|---|---|
| No reply before deadline (bank spent/used) | Tier 2 penalty |
| Malformed JSON, missing `type`/`action`, unknown `action` string | Tier 2 penalty |
| `id` mismatch, or a reply to a no-reply message | discarded → counts as no reply → Tier 2 |
| `amount` missing on `bet`/`raise`, or non-integer, or negative | Tier 2 penalty |
| Process exited / pipe closed mid-match | seat → `disqualified`, auto-folds the remainder, **no further round-trips** |
| Wrong `v` in a reply | Tier 2 penalty, logged distinctly as a version mismatch |
| Writes to **stderr** | never a violation; captured to `runs/<id>/<bot>.stderr` for debugging |

A hung subprocess is strictly *better* contained than a hung in-process thread: the engine
can `SIGKILL` it and actually reclaim the resources, where §4.2's leaked thread cannot be
killed. Worth stating in the report as a genuine advantage of the subprocess binding.

## A.4 What this does to the performance story

It does not touch it, and that is the point of keeping the native binding.

| Mode | Per-action overhead | Use |
|---|---|---|
| `Direct` | ~0 | benches, self-play, golden-log replay |
| `Guarded` | ~µs | native-Rust tournament, live demo |
| `Subprocess` | ~0.1–2ms (spawn amortized; steady state is 2 pipe writes + JSON) | polyglot tournament |

The §5.3 budgets (≥1M hands/sec under `Direct`, ≥10M evaluations/sec) are stated for the
native path and remain the headline numbers. A subprocess tournament is throughput-bound by
pipes and JSON, roughly ~10³ hands/sec — which is irrelevant, because its budget is 20
seconds per decision, not nanoseconds. **The engine, the evaluator, and the rules are
shared by all three modes**; only the agent boundary differs, so language agnosticism costs
exactly one `Supervisor` variant and a serializer and buys zero slowdown where speed is
claimed.

Secondary benefit worth noting: a polyglot tournament parallelizes across the host's 20
hardware threads far better than an in-process one, since each bot is its own OS process.

## A.5 Deliverables this addendum implies

1. `AGENT_SPEC.md` — the normative agent-facing document (drafted; see that file).
2. `schema/` — JSON Schema for all seven message types, so bots can self-validate.
3. **A conformance harness**, `holdem-cli conformance --bot "python3 bot.py"`, that replays
   a fixed scripted script of requests at a candidate bot and checks every reply, including
   the nasty cases: an illegal spot where only folding and calling are legal, a short-stack
   all-in, a timeout, a malformed-reply recovery, and a no-reply-expected message. A bot
   author should be able to prove conformance without reading engine source.
4. **A reference bot per language** (Rust, Python, C++, Java, JS — all five toolchains are
   present on the host, §A.6) doing nothing but call/check. ~40 lines each. These double as
   integration tests of the protocol.
5. A `bot.toml` manifest per bot (`name`, `argv`, `language`, `author`) so the CLI can
   discover and launch bots without hard-coded paths.

## A.6 Reference host

The system the demo and all published benchmark numbers run on. Agent-facing resource
limits derived from it are in `AGENT_SPEC.md` §2.

| | |
|---|---|
| CPU | Intel Core i5-14600 — 14 cores / 20 threads, up to 5.2 GHz, 24 MiB L3 |
| Memory | 31 GiB |
| OS | Ubuntu 24.04.5 LTS, Linux 6.8.0 x86_64 |
| Rust | 1.75.0 (**edition 2021**) |
| Other toolchains | Python 3.12.3, Node 22.17.1, OpenJDK 21, GCC 13.3, Clang 18.1 |
| Storage | network home directory — treat disk I/O as slow; bots should not rely on it mid-hand |

Benchmarks are single-core unless stated. The 20-thread count matters for §A.4's
parallelism claim and bounds how many subprocess bots can run truly concurrently.

## A.7 Revised open questions

Superseding §8's list where they overlap:

1. **Is the subprocess binding in scope for the deliverable, or documented-but-unbuilt?**
   The SPEC's fallback is a CLI running six Rust agents. → **Recommend: build the native
   bindings first and completely, then land `Subprocess` plus two reference bots
   (Python, Rust) as the stretch goal.** Defining the contract now costs nothing and
   prevents the native trait from accreting assumptions that would make it unmappable
   later; building the transport too early risks the core.
2. Do the six example agents stay native Rust? → **Recommend yes** — they are also the
   performance benchmark, and they must run under `Direct`.
3. Should the engine ever run a **mixed** table (native + subprocess)? Clean to support
   since `Supervisor` is per-seat, and a good demo moment. Fairness caveat: a subprocess
   bot pays ~1ms of protocol tax out of its 20,000ms, which is immaterial — state it
   explicitly rather than leaving it as a suspicion.
4. Protocol version policy: a bot sends `v`, and the engine refuses mismatched majors at
   handshake. Adding a field is a non-breaking change *only* because of the
   ignore-unknown-fields rule in §A.3.1 — keep that rule inviolate.

---

# Addendum B — CPU affinity, core allocation, and measurement hygiene

Added on request: pin threads so they don't migrate between cores. Investigating the
reference host turned this from routine benchmark hygiene into a **fairness requirement**,
because the i5-14600 is a hybrid CPU and its cores are not interchangeable.

## B.1 The reference host is heterogeneous — measured, not assumed

```
$ lscpu -e   (abridged)      $ cache topology
CPU  CORE  MAXMHZ            P-core (cpu0-11):  L1d 48K  L2 2048K private per core
 0     0   5200  ┐           E-core (cpu12-19): L1d 32K  L2 4096K SHARED by 4 cores
 1     0   5200  ┘ SMT pair  L3 24576K shared by all 20
 2     1   5200  ┐
 3     1   5200  ┘ SMT pair      P-cores  6 physical / 12 logical, SMT on,  5200 MHz
 ...                             E-cores  8 physical /  8 logical, no SMT,  3900 MHz
12     6   3900  ┐ L2 cluster    governor powersave (NOT writable — no root)
13     7   3900  │ 12-15         turbo    enabled  (NOT writable — no root)
14     8   3900  │               isolcpus not set   (needs boot params)
15     9   3900  ┘               taskset  works unprivileged ✓
16    10   3900  ┐ L2 cluster
...              ┘ 16-19
```

Three distinct hazards, in descending order of severity:

1. **P-core vs E-core: not a tie.** 5200 vs 3900 MHz is a 33% clock gap, and on top of that
   Raptor Cove and Gracemont are different microarchitectures with materially different IPC.
   A search-heavy bot can plausibly get **1.5–2.5× more work done** on a P-core in the same
   20 seconds. If the scheduler puts bot A on cpu2 and bot B on cpu14, **bot B was handed a
   weaker computer**, and the match result is partly an artifact of the Linux scheduler. This
   is not a measurement nuisance; it is a correctness problem for a competition.
2. **SMT siblings share a physical core.** cpu0 and cpu1 are one core. Two bots there
   contend for the same execution ports and L1/L2 — each loses roughly 20–40% depending on
   the mix.
3. **E-core L2 is shared four ways.** A table-heavy bot on cpu12 evicts the L2 of cpu13–15.
   One bot can degrade three others through a cache it doesn't appear to share.

Add plain migration on top: moving a thread discards its warm L1/L2, and our evaluator is
exactly the lookup-table workload that punishes a cold L2.

## B.2 Pinning does three jobs, not one

Worth separating, because they justify the complexity independently:

1. **Fairness.** Every competing bot gets an *identical* core type. This matters more than
   getting a *fast* core type (§B.4).
2. **Measurement.** Removes migration and core-type variance from the §5.3 budgets, so a
   benchmark delta reflects a code change rather than scheduler luck.
3. **Enforcement.** `AGENT_SPEC.md` §2 already promises each bot "1 core." Pinning is what
   makes that a *mechanism* instead of a documented hope: a bot that spawns 20 threads
   becomes merely *useless* (all its threads share one core) rather than *unfair* (stealing
   the machine from everyone else). A rule enforced by the OS needs no policing.

## B.3 Core allocation plan

One observation shapes the whole plan: **poker is sequential.** Exactly one bot is ever
thinking at a time. So bots never contend with each other in the steady state, and each one
thinks under effectively single-core-turbo conditions — the best case the host offers, and
identical for everyone. The engine, meanwhile, is parked in `recv_timeout` doing nothing
while a bot thinks.

That makes the right allocation almost obvious:

| Role | Pinned to | Why |
|---|---|---|
| **Engine / dealer thread** | **cpu19** (one E-core) | blocked on a channel ~100% of the time while bots think; wasting a P-core on it is indefensible |
| **Bot seats 0–5** | **cpu0, 2, 4, 6, 8, 10** — one logical CPU per physical P-core | even-numbered = one per SMT pair, so **every sibling stays idle**: no SMT contention, full 2 MB private L2 per bot, and L2 stays warm across that bot's turns |
| SMT siblings cpu1, 3, 5, 7, 9, 11 | **left idle, deliberately** | using them would halve each bot's effective core |
| cpu12–18 | OS, logging, other tenants | keeps background work off the competition cores |

Six P-cores for exactly the six example agents — the SPEC's demo fits the hardware without
compromise.

## B.4 The six-bot cap — DECIDED

**`MAX_SEATS = 6`, and `FairPerformance` is the default and only built allocation policy.**
The host has exactly six performance cores; six competing bots is therefore the largest
table this machine can seat *fairly*, and that hardware limit — not a poker rule — is what
sets the engine's maximum seat count.

The naive alternative, spilling a seventh bot onto an E-core, is the one thing we must not
do: it hands seats 0–5 a ~2× hardware advantage and silently corrupts the result. The
engine **refuses to start** a match with more than six competing bots rather than produce a
tainted one.

This cap costs nothing and simplifies several things at once: `[SeatPublic; 6]` instead of a
`Vec`, a bounded side-pot layer count, and a test matrix that can plausibly cover every seat
count from 2 to 6 exhaustively.

**What stays general on purpose.** The engine's *logic* remains written over N — blind
order, action order, and side pots are all N-generic, with heads-up as the one special case
(§5.4). `MAX_SEATS` is a single constant governing array sizes and config validation. Going
to 9-max later is therefore "bump one constant and pick a core-allocation policy," not a
rewrite. That is the whole reason to keep the logic general while capping the number.

The policy enum below is retained as **documentation of the design space**; only
`FairPerformance` and `None` are built for now:

```rust
pub const MAX_SEATS: usize = 6;   // == the host's P-core count. See §B.4.

pub enum CoreAllocation {
    /// BUILT, default. One logical CPU per physical P-core, SMT siblings idle,
    /// engine on an E-core. Hard cap MAX_SEATS (6). Refuses to start with more.
    FairPerformance,
    /// BUILT. No pinning. Honest, reproducible-only-in-spirit; every log and report from
    /// such a run is stamped UNPINNED so its numbers are never silently compared to
    /// pinned ones. Also the automatic fallback when sched_setaffinity fails (§B.5).
    None,
    /// NOT BUILT — design space only. All bots on E-cores, one each. Slower for everyone,
    /// identical for everyone; would support up to 7-8 seats on this host.
    FairEfficient,
    /// NOT BUILT — design space only. Explicit cpu-per-seat map for experiments; would log
    /// a heterogeneity warning if the assigned cores are not all the same type.
    Manual(Vec<usize>),
}
```

The guiding principle, recorded because it is what would govern any future expansion:
**a table where everyone is on a 3900 MHz E-core is fairer than one where six bots get
5200 MHz and the rest don't.** Homogeneity beats speed. Failing loudly above six seats is
the whole value of having thought about this.

## B.5 Implementation

**Native threads.** Pin from *inside* each thread as its first act, so the pin is in effect
before any allocation or table build warms the wrong core's cache:

```rust
// in holdem-engine; Linux-only path, feature-gated
fn pin_current_thread(cpu: usize) -> std::io::Result<()> {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        // pid 0 == calling thread
        if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}
```

→ **Recommend raw `libc::sched_setaffinity` over the `core_affinity` crate.** It is ~15
lines, the platform is Linux-only by §A.6, and `core_affinity`'s portable abstraction hides
exactly what we need to see (logical-CPU identity and core type). Non-Linux builds compile
to a no-op that reports `Unpinned`.

**Subprocess bots** (§A.1 binding 2): set affinity in the child between fork and exec via
`CommandExt::pre_exec`, which pins the bot *and everything it spawns*, since affinity is
inherited:

```rust
use std::os::unix::process::CommandExt;
unsafe { cmd.pre_exec(move || { pin_current_thread(cpu)?; Ok(()) }); }
```

Equivalent to prefixing `taskset -c <cpu>`, but without depending on `taskset` being
installed. Verified working unprivileged on the host (`taskset -c 2 nproc` → `1`).

**Topology discovery, not hardcoding.** Read `/sys/devices/system/cpu/`:
`cpufreq/cpuinfo_max_freq` clusters logical CPUs into core types (5200000 vs 3900000);
`topology/thread_siblings_list` identifies SMT pairs; `cache/index2/shared_cpu_list` reveals
the E-core L2 clusters. Build the allocation from that at startup so the engine behaves
correctly on a non-hybrid machine (where every core is one type and the plan degenerates to
"one core each") and on a future different host.

**Failure is non-fatal but loud.** In a container, under a restrictive cgroup, or on a
cpuset that excludes the target CPU, `sched_setaffinity` fails. The engine then warns, marks
the run `UNPINNED`, and continues — a run must never be *blocked* by pinning, but its
numbers must never be silently comparable to a pinned run's either.

## B.6 What pinning does NOT fix — the honest part

Pinning removes migration and core-type variance. It does not remove frequency variance, and
on this host we cannot:

| Knob | State | Can we change it? |
|---|---|---|
| `scaling_governor` | **powersave** | **No** — not writable, needs root |
| Turbo (`intel_pstate/no_turbo`) | enabled | **No** — not writable, needs root |
| `isolcpus` / `nohz_full` | not set | **No** — needs boot parameters |
| SMT | on | **No** — needs root (we work around it by idling siblings) |
| CPU affinity | — | **Yes**, unprivileged ✓ |

Consequences to design around rather than wish away:

- **Cores idle at 800 MHz** under `powersave` (observed). A bot's *first* decision can run
  at a fraction of full clock and ramp up. → **Mandate a warmup**: the engine spins a short
  fixed-work loop on each pinned core before the first hand, and benchmarks discard warmup
  iterations (criterion does this by default). Without it, hand 1 is systematically slower
  than hand 50 and every "first decision" timing is garbage.
- **Turbo is opportunistic**, so absolute wall-clock times are not portable off this host.
  Every published number must name the host, the core type, and the pinning policy. Since
  turns are sequential (§B.3), all bots see the same single-core boost conditions, so this
  degrades *absolute* comparability, not *between-bot fairness*.
- **Other tenants exist.** Without `isolcpus` the kernel can still schedule other work on
  our cores. Mitigation: run on an idle machine, pin OS-adjacent work away from cpu0–10, and
  record load average at match start into the log.

## B.7 Log the plan, and log CPU time alongside wall time

Every match record gets a `host` block so a result is interpretable a year later:

```json
{"host":{"cpu":"Intel Core i5-14600","p_cores":6,"e_cores":8,
         "governor":"powersave","turbo":true,"smt":"on",
         "allocation":"FairPerformance","pinned":true,
         "seat_cpu":{"0":0,"1":2,"2":4,"3":6,"4":8,"5":10},
         "engine_cpu":19,"loadavg_at_start":[0.41,0.55,0.60]}}
```

**And a refinement pinning makes worthwhile:** record each decision's **CPU time**
(`CLOCK_THREAD_CPUTIME_ID` natively, `getrusage`/`/proc/<pid>/stat` for subprocess bots)
next to its wall time. The 20-second limit stays **wall-clock normative** — it is what
`AGENT_SPEC.md` promises, it is what an observer can verify, and it correctly charges a bot
for sleeping or blocking on I/O. But with threads pinned, wall ≈ CPU for a well-behaved bot,
so a large divergence is *diagnostic*: it means the bot was descheduled (our problem — the
host was contended, and the run should be flagged) or the bot blocked on something it
shouldn't (its problem). Logging both is what lets us tell those two apart instead of
guessing, and it is nearly free.

## B.8 Cross-reference: this tightens the evaluator decision

A pinned P-core has **2 MB of private L2**. That is now a concrete design constraint on §5.2:

- A compact bitmask/perfect-hash evaluator with tables in the tens-of-KB to low-MB range
  stays **L2-resident** on its pinned core and keeps that L2 warm across the bot's turns.
- The ~130 MB two-plus-two lookup table does not fit L2, does not fit the 24 MB L3, and
  every lookup chain becomes a dependent chain of DRAM round-trips — while also thrashing
  the L3 that all 20 cores share, so it degrades *every other bot* on the machine.

→ **Reinforces §5.2's recommendation**: ship the compact in-repo evaluator as the default
and treat the two-plus-two table as rejected on this host, now for a measured architectural
reason rather than a vague distaste for a large file.

## B.9 Open questions

1. ~~`FairPerformance` vs `FairEfficient` as the default?~~ **RESOLVED: `FairPerformance`,
   capped at 6 bots** (§B.4). `MAX_SEATS = 6`; `FairEfficient` is documented but not built.
2. Should the engine **refuse to run** a benchmark whose numbers will be published if
   `pinned == false` or `loadavg > threshold`? → **Recommend: warn, stamp, proceed.**
   Blocking a run on machine conditions is more annoying than useful; an unignorable stamp
   in the log and the report achieves the same discipline.
3. Is it worth asking whoever administers this host for a **`performance` governor** and
   `isolcpus=0-10`? It would make absolute timings genuinely stable. Cheap to ask, large
   payoff for the benchmark section of the report; everything above works without it.
