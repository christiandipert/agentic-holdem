//! The public agent contract for the Agentic Poker Arena.
//!
//! This crate is the *only* thing a bot author depends on. It is deliberately
//! dependency-free and contains no engine internals: there is no deck here, no
//! shuffle seed, and no way to reach another seat's hole cards. Hidden
//! information is hidden by construction rather than by policy.
//!
//! See `AGENT_SPEC.md` for the normative, language-independent description.

use std::fmt;

/// Chips are always exact integers. Never floating point: chip conservation is
/// an invariant the engine asserts, and floats cannot hold it.
pub type Chips = u64;

/// Seat index, `0..n_seats`. Stable for the whole match.
pub type SeatId = u8;

/// Hardware fairness cap, not a poker rule. The reference host has six
/// performance cores, so six is the largest table it can seat fairly.
/// See `PLAN.md` Addendum B.4.
pub const MAX_SEATS: usize = 6;

// ---------------------------------------------------------------- cards

/// A card as `0..=51`, encoded so that `rank = c >> 2` and `suit = c & 3`.
///
/// Rank `0`=Two .. `12`=Ace. Suit `0`=spades, `1`=hearts, `2`=diamonds, `3`=clubs.
/// The encoding is chosen so rank/suit extraction is a single shift or mask and a
/// set of cards fits in a `u64` bitmask.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Card(pub u8);

impl Card {
    pub const RANKS: [char; 13] = ['2', '3', '4', '5', '6', '7', '8', '9', 'T', 'J', 'Q', 'K', 'A'];
    pub const SUITS: [char; 4] = ['s', 'h', 'd', 'c'];

    #[inline]
    pub fn new(rank: u8, suit: u8) -> Self {
        debug_assert!(rank < 13 && suit < 4);
        Card((rank << 2) | suit)
    }
    #[inline]
    pub fn rank(self) -> u8 {
        self.0 >> 2
    }
    #[inline]
    pub fn suit(self) -> u8 {
        self.0 & 3
    }

    /// Parse the wire form, e.g. `"Ah"`, `"Td"`, `"2c"`. Case-sensitive, as specified.
    pub fn parse(s: &str) -> Option<Self> {
        let b: Vec<char> = s.chars().collect();
        if b.len() != 2 {
            return None;
        }
        let rank = Self::RANKS.iter().position(|&r| r == b[0])? as u8;
        let suit = Self::SUITS.iter().position(|&s| s == b[1])? as u8;
        Some(Card::new(rank, suit))
    }
}

impl fmt::Display for Card {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}",
            Card::RANKS[self.rank() as usize],
            Card::SUITS[self.suit() as usize]
        )
    }
}

impl fmt::Debug for Card {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

// ---------------------------------------------------------------- game state

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Street {
    Preflop,
    Flop,
    Turn,
    River,
}

impl Street {
    pub fn as_str(self) -> &'static str {
        match self {
            Street::Preflop => "preflop",
            Street::Flop => "flop",
            Street::Turn => "turn",
            Street::River => "river",
        }
    }
    /// How many board cards are face up on this street.
    pub fn board_len(self) -> usize {
        match self {
            Street::Preflop => 0,
            Street::Flop => 3,
            Street::Turn => 4,
            Street::River => 5,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SeatStatus {
    /// In the hand and able to act.
    Active,
    /// Out of this hand.
    Folded,
    /// In the hand, all chips committed, cannot act again.
    AllIn,
    /// Out of the match: no chips left.
    Busted,
    /// Unreachable (crashed or hung). Auto-folded for the remainder.
    Disqualified,
}

impl SeatStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SeatStatus::Active => "active",
            SeatStatus::Folded => "folded",
            SeatStatus::AllIn => "allin",
            SeatStatus::Busted => "busted",
            SeatStatus::Disqualified => "disqualified",
        }
    }
    /// Can still win chips in the current hand.
    pub fn in_hand(self) -> bool {
        matches!(self, SeatStatus::Active | SeatStatus::AllIn)
    }
}

/// Everything about a seat that every player is allowed to see.
#[derive(Copy, Clone, Debug)]
pub struct SeatPublic {
    pub seat: SeatId,
    /// Chips behind, not counting what is already wagered this street.
    pub stack: Chips,
    /// Wagered this street.
    pub street_bet: Chips,
    /// Wagered this hand, across all streets.
    pub committed: Chips,
    pub status: SeatStatus,
}

/// One entry in the current hand's action log.
#[derive(Copy, Clone, Debug)]
pub struct LoggedAction {
    pub seat: SeatId,
    pub street: Street,
    pub action: Action,
    /// Chips moved by this action (0 for check and fold).
    pub amount: Chips,
}

// ---------------------------------------------------------------- actions

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Fold,
    Check,
    /// Match the outstanding bet. The engine computes the cost; the agent never states it.
    Call,
    /// Opening wager on a street where `current_bet == 0`. `to` is the TOTAL for the street.
    Bet { to: Chips },
    /// Increase the wager. `to` is the TOTAL for the street, not an increment.
    Raise { to: Chips },
    /// Commit the entire stack. Always well defined.
    AllIn,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Fold => "fold",
            Action::Check => "check",
            Action::Call => "call",
            Action::Bet { .. } => "bet",
            Action::Raise { .. } => "raise",
            Action::AllIn => "allin",
        }
    }
}

// ---------------------------------------------------------------- the view

/// Everything an agent is allowed to know when it is their turn.
///
/// Borrowed rather than owned so the engine can hand it over without allocating.
pub struct PlayerView<'a> {
    pub me: SeatId,
    pub button: SeatId,
    pub street: Street,
    pub hand_number: u32,

    /// This agent's own two cards. No other seat's hole cards exist in this struct.
    pub hole: [Card; 2],
    /// Community cards: 0, 3, 4 or 5 of them.
    pub board: &'a [Card],

    /// Everything committed so far this hand, by everyone, including this street.
    pub pot: Chips,
    /// Largest total any seat has wagered this street.
    pub current_bet: Chips,
    /// What it costs this agent to call, **clamped to this agent's stack**, so
    /// `Action::Call` is always affordable and never needs an amount. To learn
    /// the full outstanding wager, compare `current_bet` with
    /// `my_seat().street_bet`; `owed()` does exactly that.
    pub to_call: Chips,
    /// Smallest legal `Raise { to }`. `0` means raising is not legal right now.
    pub min_raise_to: Chips,
    /// Largest legal `Raise { to }`, i.e. this agent's all-in.
    pub max_raise_to: Chips,
    pub big_blind: Chips,

    /// Every seat, in seat order, including this one.
    pub seats: &'a [SeatPublic],
    /// Every action taken this hand, in order. The agent may ignore it.
    pub hand_log: &'a [LoggedAction],

    /// Milliseconds left for this decision. Advisory in this build: see `AGENT_SPEC.md` §3.
    pub time_remaining_ms: u64,
    /// Remaining one-per-run time bank, or `None` once spent.
    pub time_bank_remaining_ms: Option<u64>,
}

impl PlayerView<'_> {
    /// This agent's own seat record.
    pub fn my_seat(&self) -> &SeatPublic {
        &self.seats[self.me as usize]
    }
    /// Chips behind, not counting this street's wager.
    pub fn my_stack(&self) -> Chips {
        self.my_seat().stack
    }
    pub fn can_check(&self) -> bool {
        self.to_call == 0
    }
    pub fn can_raise(&self) -> bool {
        self.min_raise_to > 0
    }
    /// The full outstanding wager, *not* clamped to this agent's stack. Larger
    /// than `to_call` exactly when calling would mean going all-in for less.
    pub fn owed(&self) -> Chips {
        self.current_bet.saturating_sub(self.my_seat().street_bet)
    }
}

// ---------------------------------------------------------------- results

/// One pot layer and who won it.
#[derive(Clone, Debug)]
pub struct PotResult {
    pub amount: Chips,
    pub eligible: Vec<SeatId>,
    pub winners: Vec<SeatId>,
    /// Human-readable class of the winning hand, e.g. `"two_pair"`. `None` if
    /// everyone folded and there was no showdown.
    pub hand_class: Option<String>,
}

/// Delivered after every hand, to every seated agent, including ones that folded.
#[derive(Clone, Debug)]
pub struct HandResult {
    pub hand_number: u32,
    pub board: Vec<Card>,
    /// Net chip change and resulting stack, per seat.
    pub deltas: Vec<(SeatId, i64, Chips)>,
    pub pots: Vec<PotResult>,
    /// Hole cards that genuinely reached a showdown. A hand won because everyone
    /// folded reveals nothing, exactly as at a real table.
    pub revealed: Vec<(SeatId, [Card; 2])>,
}

/// Delivered at the start of every hand, before any action is requested.
#[derive(Clone, Debug)]
pub struct HandStart<'a> {
    pub hand_number: u32,
    pub button: SeatId,
    /// This agent's own two cards for the hand about to be played.
    pub hole: [Card; 2],
    /// Every seat's stack as the hand begins, before blinds are posted.
    pub seats: &'a [SeatPublic],
    pub small_blind: Chips,
    pub big_blind: Chips,
}

/// Handed to each agent once, before the first hand.
#[derive(Clone, Debug)]
pub struct MatchContext {
    pub me: SeatId,
    pub n_seats: u8,
    pub starting_stack: Chips,
    pub small_blind: Chips,
    pub big_blind: Chips,
    pub ante: Chips,
    pub max_hands: Option<u32>,
    pub action_timeout_ms: u64,
    pub time_bank_ms: Option<u64>,
    /// This agent's own RNG stream, independent of the deck's. Use it instead of
    /// the clock so that matches stay reproducible.
    pub rng_seed: u64,
}

// ---------------------------------------------------------------- the trait

/// What every bot implements. Two required methods; the rest are defaulted.
///
/// Native Rust agents implement this directly. Agents in other languages
/// implement the equivalent JSON protocol and the engine adapts them to this
/// same trait, so the engine cannot tell the difference.
pub trait Agent: Send {
    /// Stable identifier used in logs and reports.
    fn name(&self) -> &str;

    /// REQUIRED. Called exactly once per match, before any hand is dealt.
    fn entry(&mut self, ctx: &MatchContext);

    /// REQUIRED. Called every time it is this agent's turn to act.
    fn on_action(&mut self, view: &PlayerView<'_>) -> Action;

    /// Called at the start of every hand.
    fn on_hand_start(&mut self, _start: &HandStart<'_>) {}

    /// Called after every hand, including hands this agent folded out of. The only
    /// place revealed showdown hole cards are delivered.
    fn on_hand_end(&mut self, _result: &HandResult) {}

    /// Called once when the match is over.
    fn shutdown(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_roundtrip() {
        for raw in 0u8..52 {
            let c = Card(raw);
            assert_eq!(Card::parse(&c.to_string()), Some(c));
        }
        assert_eq!(Card::parse("Ah"), Some(Card::new(12, 1)));
        assert_eq!(Card::parse("2s"), Some(Card::new(0, 0)));
        assert_eq!(Card::parse("ah"), None, "parsing is case-sensitive");
        assert_eq!(Card::parse("Axh"), None);
    }

    #[test]
    fn rank_suit_extraction() {
        let c = Card::new(12, 3);
        assert_eq!((c.rank(), c.suit()), (12, 3));
        assert_eq!(c.to_string(), "Ac");
    }
}
