//! The table state the GUI draws, rebuilt incrementally from match events.

use holdem_agent::{Card, Chips, SeatId, Street};
use holdem_engine::{MatchEvent, Outcome};

#[derive(Clone, Default)]
pub struct SeatView {
    pub name: String,
    pub stack: Chips,
    pub street_bet: Chips,
    pub hole: Option<[Card; 2]>,
    pub folded: bool,
    pub all_in: bool,
    pub acting: bool,
    pub violations: u32,
    pub normalizations: u32,
}

#[derive(Clone, Copy, PartialEq)]
pub enum LineKind {
    Info,
    Action,
    Blind,
    Board,
    Pot,
    Bad,
}

#[derive(Clone)]
pub struct FeedLine {
    pub text: String,
    pub kind: LineKind,
}

pub struct GameState {
    pub seats: Vec<SeatView>,
    pub board: Vec<Card>,
    pub pot: Chips,
    pub current_bet: Chips,
    pub hand: u32,
    pub button: SeatId,
    pub street: Option<Street>,
    pub feed: Vec<FeedLine>,
    /// One snapshot of every stack per completed hand, for the chart.
    pub chip_history: Vec<Vec<Chips>>,
    pub starting_stack: Chips,
    pub small_blind: Chips,
    pub big_blind: Chips,
    pub seed: u64,
    pub max_hands: Option<u32>,
    pub finished: bool,
    pub showdown: Vec<(SeatId, [Card; 2])>,
}

impl Default for GameState {
    fn default() -> Self {
        GameState {
            seats: Vec::new(),
            board: Vec::new(),
            pot: 0,
            current_bet: 0,
            hand: 0,
            button: 0,
            street: None,
            feed: Vec::new(),
            chip_history: Vec::new(),
            starting_stack: 0,
            small_blind: 0,
            big_blind: 0,
            seed: 0,
            max_hands: None,
            finished: false,
            showdown: Vec::new(),
        }
    }
}

impl GameState {
    fn name(&self, seat: SeatId) -> String {
        self.seats
            .get(seat as usize)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "?".into())
    }

    fn push(&mut self, kind: LineKind, text: String) {
        self.feed.push(FeedLine { text, kind });
        // The feed is for watching, not archiving; cap it so memory stays flat.
        if self.feed.len() > 4000 {
            self.feed.drain(0..1000);
        }
    }

    /// Fold one event into the state. This is the only place state changes.
    pub fn apply(&mut self, event: &MatchEvent) {
        match event {
            MatchEvent::MatchStart {
                names,
                starting_stack,
                small_blind,
                big_blind,
                seed,
                max_hands,
            } => {
                self.seats = names
                    .iter()
                    .map(|n| SeatView {
                        name: n.clone(),
                        stack: *starting_stack,
                        ..Default::default()
                    })
                    .collect();
                self.starting_stack = *starting_stack;
                self.small_blind = *small_blind;
                self.big_blind = *big_blind;
                self.seed = *seed;
                self.max_hands = *max_hands;
                self.chip_history.push(vec![*starting_stack; names.len()]);
                self.push(
                    LineKind::Info,
                    format!(
                        "match start: {} chips, blinds {}/{}, seed {}",
                        starting_stack, small_blind, big_blind, seed
                    ),
                );
            }

            MatchEvent::HandStart { hand, button, stacks, holes } => {
                self.hand = *hand;
                self.button = *button;
                self.board.clear();
                self.showdown.clear();
                self.pot = 0;
                self.current_bet = 0;
                self.street = Some(Street::Preflop);
                for (i, seat) in self.seats.iter_mut().enumerate() {
                    seat.stack = stacks[i];
                    seat.street_bet = 0;
                    seat.hole = holes[i];
                    seat.folded = holes[i].is_none();
                    seat.all_in = false;
                    seat.acting = false;
                }
                self.push(LineKind::Info, format!("--- hand {hand}"));
            }

            MatchEvent::BlindsPosted { sb_seat, sb_amount, bb_seat, bb_amount } => {
                for (seat, amount) in [(*sb_seat, *sb_amount), (*bb_seat, *bb_amount)] {
                    let s = &mut self.seats[seat as usize];
                    s.stack = s.stack.saturating_sub(amount);
                    s.street_bet += amount;
                }
                self.pot += sb_amount + bb_amount;
                self.current_bet = (*sb_amount).max(*bb_amount);
                let text = format!(
                    "{} posts SB {sb_amount}, {} posts BB {bb_amount}",
                    self.name(*sb_seat),
                    self.name(*bb_seat)
                );
                self.push(LineKind::Blind, text);
            }

            MatchEvent::BoardDealt { street, board } => {
                self.board = board.clone();
                self.street = Some(*street);
                for s in self.seats.iter_mut() {
                    s.street_bet = 0;
                    s.acting = false;
                }
                self.current_bet = 0;
                let cards: Vec<String> = board.iter().map(|c| c.to_string()).collect();
                let text = format!("[{}] {}", street.as_str(), cards.join(" "));
                self.push(LineKind::Board, text);
            }

            MatchEvent::ActionRequested { seat, .. } => {
                for (i, s) in self.seats.iter_mut().enumerate() {
                    s.acting = i == *seat as usize;
                }
            }

            MatchEvent::ActionTaken {
                seat,
                action,
                amount,
                outcome,
                all_in,
                stacks,
                street_bets,
                pot,
                current_bet,
                ..
            } => {
                for (i, s) in self.seats.iter_mut().enumerate() {
                    s.stack = stacks[i];
                    s.street_bet = street_bets[i];
                    s.acting = false;
                }
                self.pot = *pot;
                self.current_bet = *current_bet;
                let s = &mut self.seats[*seat as usize];
                s.all_in = *all_in;
                if matches!(action, holdem_agent::Action::Fold) {
                    s.folded = true;
                }
                match outcome {
                    Outcome::Violation(why) => {
                        self.seats[*seat as usize].violations += 1;
                        let text =
                            format!("{} INVALID ({why}) -> {}", self.name(*seat), action.as_str());
                        self.push(LineKind::Bad, text);
                    }
                    Outcome::Normalized(why) => {
                        self.seats[*seat as usize].normalizations += 1;
                        let text = format!("{} corrected: {why}", self.name(*seat));
                        self.push(LineKind::Bad, text);
                    }
                    Outcome::Clean => {}
                }
                let detail = match action {
                    holdem_agent::Action::Bet { to } | holdem_agent::Action::Raise { to } => {
                        format!(" to {to}")
                    }
                    holdem_agent::Action::Call => format!(" {amount}"),
                    _ => String::new(),
                };
                let allin = if *all_in { "  (all-in)" } else { "" };
                let text = format!("{} {}{detail}{allin}", self.name(*seat), action.as_str());
                self.push(LineKind::Action, text);
            }

            MatchEvent::PotAwarded { amount, winners, hand_class, showdown } => {
                let names: Vec<String> = winners.iter().map(|&w| self.name(w)).collect();
                let how = match (hand_class, showdown) {
                    (Some(c), true) => format!(" with {c}"),
                    (_, false) => " (everyone else folded)".into(),
                    _ => String::new(),
                };
                let text = format!("pot {amount} -> {}{}", names.join(", "), how);
                self.push(LineKind::Pot, text);
            }

            MatchEvent::Showdown { revealed } => {
                self.showdown = revealed.clone();
            }

            MatchEvent::HandEnd { stacks, .. } => {
                for (i, s) in self.seats.iter_mut().enumerate() {
                    s.stack = stacks[i];
                    s.street_bet = 0;
                    s.acting = false;
                }
                self.pot = 0;
                self.chip_history.push(stacks.clone());
            }

            MatchEvent::SeatDisqualified { seat, why } => {
                let text = format!("{} DISQUALIFIED: {why}", self.name(*seat));
                self.push(LineKind::Bad, text);
            }

            MatchEvent::MatchEnd { hands, stacks } => {
                for (i, s) in self.seats.iter_mut().enumerate() {
                    s.stack = stacks[i];
                    s.acting = false;
                }
                self.finished = true;
                self.push(LineKind::Info, format!("match over after {hands} hands"));
            }
        }
    }
}
