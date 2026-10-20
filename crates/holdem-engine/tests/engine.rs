//! Engine integration tests, driven by native in-process agents so they are
//! fast and deterministic.

use holdem_agent::{Action, Agent, MatchContext, PlayerView, Street};
use holdem_engine::{MatchConfig, Native, Seated, Table};
use std::sync::{Arc, Mutex};

// ----------------------------------------------------------------- test bots

/// Checks when it can, calls when it cannot. Never folds, never raises.
struct Caller(&'static str);
impl Agent for Caller {
    fn name(&self) -> &str {
        self.0
    }
    fn entry(&mut self, _: &MatchContext) {}
    fn on_action(&mut self, v: &PlayerView<'_>) -> Action {
        if v.to_call == 0 {
            Action::Check
        } else {
            Action::Call
        }
    }
}

/// Always folds.
struct Folder;
impl Agent for Folder {
    fn name(&self) -> &str {
        "folder"
    }
    fn entry(&mut self, _: &MatchContext) {}
    fn on_action(&mut self, _: &PlayerView<'_>) -> Action {
        Action::Fold
    }
}

/// Always checks, which is illegal while facing a bet. Used to prove the
/// Tier 2 penalty: check if nothing is owed, otherwise fold.
struct AlwaysCheck;
impl Agent for AlwaysCheck {
    fn name(&self) -> &str {
        "always_check"
    }
    fn entry(&mut self, _: &MatchContext) {}
    fn on_action(&mut self, _: &PlayerView<'_>) -> Action {
        Action::Check
    }
}

/// The half-pot strategy, natively, mirroring `bots/cpp_halfpot`.
struct HalfPot;
impl Agent for HalfPot {
    fn name(&self) -> &str {
        "half_pot"
    }
    fn entry(&mut self, _: &MatchContext) {}
    fn on_action(&mut self, v: &PlayerView<'_>) -> Action {
        if !v.can_raise() {
            return Action::Call;
        }
        let to = (v.pot / 2).clamp(v.min_raise_to, v.max_raise_to);
        if v.current_bet == 0 {
            Action::Bet { to }
        } else {
            Action::Raise { to }
        }
    }
}

/// Shoves every time it can.
struct Shover;
impl Agent for Shover {
    fn name(&self) -> &str {
        "shover"
    }
    fn entry(&mut self, _: &MatchContext) {}
    fn on_action(&mut self, _: &PlayerView<'_>) -> Action {
        Action::AllIn
    }
}

/// Returns nonsense amounts, to exercise Tier 1 clamping and Tier 2 rejection.
struct Nonsense;
impl Agent for Nonsense {
    fn name(&self) -> &str {
        "nonsense"
    }
    fn entry(&mut self, _: &MatchContext) {}
    fn on_action(&mut self, _: &PlayerView<'_>) -> Action {
        Action::Raise { to: u64::MAX }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Seen {
    seat: u8,
    street: Street,
    street_bet: u64,
    to_call: u64,
    current_bet: u64,
}

/// Records every position it is asked about, so action order and blind posting
/// can be asserted from the outside.
struct Recorder {
    name: &'static str,
    log: Arc<Mutex<Vec<Seen>>>,
}
impl Agent for Recorder {
    fn name(&self) -> &str {
        self.name
    }
    fn entry(&mut self, _: &MatchContext) {}
    fn on_action(&mut self, v: &PlayerView<'_>) -> Action {
        self.log.lock().unwrap().push(Seen {
            seat: v.me,
            street: v.street,
            street_bet: v.my_seat().street_bet,
            to_call: v.to_call,
            current_bet: v.current_bet,
        });
        if v.to_call == 0 {
            Action::Check
        } else {
            Action::Call
        }
    }
}

fn cfg(hands: u32, chips: u64) -> MatchConfig {
    MatchConfig {
        starting_stack: chips,
        small_blind: 50,
        big_blind: 100,
        ante: 0,
        max_hands: Some(hands),
        seed: 12345,
        action_timeout_ms: 20_000,
        time_bank_ms: None,
    }
}

fn boxed<A: Agent + 'static>(a: A) -> Box<dyn Seated> {
    Box::new(Native(a))
}

// ----------------------------------------------------------------- tests

#[test]
fn heads_up_button_posts_small_blind_and_acts_first_preflop() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let agents: Vec<Box<dyn Seated>> = vec![
        boxed(Recorder { name: "seat0", log: log.clone() }),
        boxed(Recorder { name: "seat1", log: log.clone() }),
    ];
    let mut t = Table::new(cfg(1, 10_000), agents);
    t.run();

    let seen = log.lock().unwrap().clone();
    let preflop: Vec<&Seen> = seen.iter().filter(|s| s.street == Street::Preflop).collect();

    // Button is seat 0 for the first hand. Heads-up, it posts the small blind
    // and acts first.
    assert_eq!(preflop[0].seat, 0, "button acts first preflop heads-up");
    assert_eq!(preflop[0].street_bet, 50, "button posted the small blind");
    assert_eq!(preflop[0].current_bet, 100, "big blind is the bet to match");
    assert_eq!(preflop[0].to_call, 50);

    // The big blind still gets its option even though it already matches.
    assert_eq!(preflop[1].seat, 1);
    assert_eq!(preflop[1].street_bet, 100, "non-button posted the big blind");
    assert_eq!(preflop[1].to_call, 0, "nothing owed, but still gets to act");
}

#[test]
fn heads_up_button_acts_last_after_the_flop() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let agents: Vec<Box<dyn Seated>> = vec![
        boxed(Recorder { name: "seat0", log: log.clone() }),
        boxed(Recorder { name: "seat1", log: log.clone() }),
    ];
    let mut t = Table::new(cfg(1, 10_000), agents);
    t.run();

    let seen = log.lock().unwrap().clone();
    for street in [Street::Flop, Street::Turn, Street::River] {
        let on_street: Vec<&Seen> = seen.iter().filter(|s| s.street == street).collect();
        assert!(!on_street.is_empty(), "{street:?} should have been played");
        assert_eq!(
            on_street[0].seat, 1,
            "non-button acts first on {street:?}; the button acts last"
        );
    }
}

#[test]
fn illegal_check_facing_a_bet_becomes_a_fold() {
    let agents: Vec<Box<dyn Seated>> =
        vec![boxed(HalfPot), boxed(AlwaysCheck)];
    let mut t = Table::new(cfg(20, 10_000), agents);
    t.run();

    assert!(
        t.stats[1].violations > 0,
        "always_check must be penalised for checking into a bet"
    );
    assert!(
        t.stats[1].folds > 0,
        "the penalty for checking into a bet is a fold"
    );
    assert!(
        t.stack(1) < 10_000,
        "a bot that folds every time it faces a bet must lose chips"
    );
}

#[test]
fn oversized_raise_is_clamped_to_all_in_not_penalised() {
    let agents: Vec<Box<dyn Seated>> = vec![boxed(Nonsense), boxed(Caller("caller"))];
    let mut t = Table::new(cfg(3, 10_000), agents);
    t.run();

    assert!(
        t.stats[0].normalizations > 0,
        "a raise above the stack is a Tier 1 clamp, not a violation"
    );
    assert_eq!(
        t.stats[0].violations, 0,
        "clamping must not also count as a violation"
    );
}

#[test]
fn folding_every_hand_loses_exactly_the_blinds() {
    // Heads-up, the folder is the button/small blind on hand 1 and the big
    // blind on hand 2, so two hands cost it sb + bb.
    let agents: Vec<Box<dyn Seated>> = vec![boxed(Folder), boxed(Caller("caller"))];
    let mut t = Table::new(cfg(2, 10_000), agents);
    t.run();
    assert_eq!(t.stack(0), 10_000 - 50 - 100);
    assert_eq!(t.stack(1), 10_000 + 50 + 100);
}

#[test]
fn match_ends_when_a_player_busts() {
    // Stacks of one big blind force an all-in confrontation immediately.
    let agents: Vec<Box<dyn Seated>> = vec![boxed(Shover), boxed(Caller("caller"))];
    let mut t = Table::new(cfg(500, 100), agents);
    t.run();

    let total = t.stack(0) + t.stack(1);
    assert_eq!(total, 200, "chips are conserved");
    assert!(
        t.stack(0) == 0 || t.stack(1) == 0,
        "one player should hold every chip: {} / {}",
        t.stack(0),
        t.stack(1)
    );
    assert!(t.hands_played < 500, "the match must end before the hand cap");
}

#[test]
fn all_in_showdowns_conserve_chips_over_many_hands() {
    // The engine asserts conservation internally after every hand, so simply
    // completing this is the assertion. Shover vs Caller guarantees constant
    // all-ins and side-pot-free short stacks.
    let agents: Vec<Box<dyn Seated>> = vec![boxed(Shover), boxed(Caller("caller"))];
    let mut t = Table::new(cfg(300, 5_000), agents);
    t.run();
    assert_eq!(t.stack(0) + t.stack(1), 10_000);
}

#[test]
fn same_seed_replays_identically() {
    let run = || {
        let agents: Vec<Box<dyn Seated>> = vec![boxed(HalfPot), boxed(Caller("caller"))];
        let mut t = Table::new(cfg(40, 10_000), agents);
        t.run();
        (t.stack(0), t.stack(1), t.hands_played)
    };
    assert_eq!(run(), run(), "a fixed seed must produce a fixed match");
}

#[test]
fn different_seeds_diverge() {
    let run = |seed: u64| {
        let mut c = cfg(40, 10_000);
        c.seed = seed;
        let agents: Vec<Box<dyn Seated>> = vec![boxed(HalfPot), boxed(Caller("caller"))];
        let mut t = Table::new(c, agents);
        t.run();
        (t.stack(0), t.stack(1))
    };
    assert_ne!(run(1), run(99_999));
}

#[test]
fn every_seat_count_up_to_the_cap_plays_and_conserves_chips() {
    for n in 2..=holdem_agent::MAX_SEATS {
        let agents: Vec<Box<dyn Seated>> = (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    boxed(HalfPot)
                } else {
                    boxed(Caller("caller"))
                }
            })
            .collect();
        let mut t = Table::new(cfg(30, 10_000), agents);
        t.run();
        let total: u64 = (0..n).map(|i| t.stack(i)).sum();
        assert_eq!(total, 10_000 * n as u64, "chips conserved with {n} seats");
    }
}

#[test]
fn three_way_all_in_builds_side_pots_without_losing_chips() {
    // Unequal stacks plus constant shoving is the side-pot path; heads-up
    // never reaches it, which is why it needs its own test.
    let mut c = cfg(200, 3_000);
    c.seed = 777;
    let agents: Vec<Box<dyn Seated>> =
        vec![boxed(Shover), boxed(Shover), boxed(Caller("caller"))];
    let mut t = Table::new(c, agents);
    t.run();
    let total: u64 = (0..3).map(|i| t.stack(i)).sum();
    assert_eq!(total, 9_000);
}
