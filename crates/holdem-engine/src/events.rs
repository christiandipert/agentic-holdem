//! Structured match events.
//!
//! The table narrates what it is doing through an `Observer` rather than
//! printing. That gives one source of truth with two consumers: the CLI
//! renders events as text, and the GUI renders them as a table. Adding a
//! JSONL exporter later is a third observer and no engine change.

use holdem_agent::{Action, Card, Chips, SeatId, Street};

/// What the table did with the action an agent asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Legal as requested.
    Clean,
    /// Tier 1: an unambiguous near-miss, corrected and played.
    Normalized(String),
    /// Tier 2: unusable, replaced by check-or-fold.
    Violation(String),
}

#[derive(Clone, Debug)]
pub enum MatchEvent {
    MatchStart {
        names: Vec<String>,
        starting_stack: Chips,
        small_blind: Chips,
        big_blind: Chips,
        seed: u64,
        max_hands: Option<u32>,
    },
    HandStart {
        hand: u32,
        button: SeatId,
        stacks: Vec<Chips>,
        /// Hole cards per seat; `None` for a seat not dealt in. Observers are
        /// spectators, not players, so they see everything.
        holes: Vec<Option<[Card; 2]>>,
    },
    /// Both blinds in one event, because they are posted together and a text
    /// view renders them on one line. Amounts are what was actually taken,
    /// which is less than the blind when a seat is too short to cover it.
    BlindsPosted {
        sb_seat: SeatId,
        sb_amount: Chips,
        bb_seat: SeatId,
        bb_amount: Chips,
    },
    BoardDealt {
        street: Street,
        board: Vec<Card>,
    },
    /// Emitted just before an agent is consulted, so a view can show who is
    /// thinking.
    ActionRequested {
        seat: SeatId,
        street: Street,
        to_call: Chips,
        min_raise_to: Chips,
        max_raise_to: Chips,
    },
    ActionTaken {
        seat: SeatId,
        street: Street,
        /// The action actually applied, after normalisation or penalty.
        action: Action,
        /// Chips this action moved.
        amount: Chips,
        outcome: Outcome,
        all_in: bool,
        stacks: Vec<Chips>,
        street_bets: Vec<Chips>,
        pot: Chips,
        current_bet: Chips,
    },
    PotAwarded {
        amount: Chips,
        winners: Vec<SeatId>,
        hand_class: Option<String>,
        /// False when everyone else folded and no cards were shown.
        showdown: bool,
    },
    Showdown {
        revealed: Vec<(SeatId, [Card; 2])>,
    },
    HandEnd {
        hand: u32,
        stacks: Vec<Chips>,
        deltas: Vec<i64>,
    },
    SeatDisqualified {
        seat: SeatId,
        why: String,
    },
    MatchEnd {
        hands: u32,
        stacks: Vec<Chips>,
    },
}

/// A consumer of match events. `Send` so a match can run on a worker thread
/// while something else renders it.
pub trait Observer: Send {
    fn on_event(&mut self, event: &MatchEvent);
}

/// Any suitable closure is an observer.
impl<F: FnMut(&MatchEvent) + Send> Observer for F {
    fn on_event(&mut self, event: &MatchEvent) {
        self(event)
    }
}
