//! Renders match events as the terminal transcript.
//!
//! This is the CLI's view of a match. The engine no longer prints anything
//! itself, so this file is the single definition of the text format.

use holdem_agent::Action;
use holdem_engine::{MatchEvent, Observer, Outcome};

pub struct TextObserver {
    names: Vec<String>,
    /// When false, only violations are reported, matching `--quiet`.
    verbose: bool,
}

impl TextObserver {
    pub fn new(verbose: bool) -> Self {
        TextObserver { names: Vec::new(), verbose }
    }

    fn name(&self, seat: u8) -> &str {
        self.names
            .get(seat as usize)
            .map(String::as_str)
            .unwrap_or("?")
    }
}

impl Observer for TextObserver {
    fn on_event(&mut self, event: &MatchEvent) {
        match event {
            MatchEvent::MatchStart { names, .. } => {
                self.names = names.clone();
            }

            MatchEvent::HandStart { hand, button, stacks, .. } => {
                if !self.verbose {
                    return;
                }
                let stacks: Vec<String> = stacks
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("{}={}", self.name(i as u8), s))
                    .collect();
                println!(
                    "\n--- hand {hand}  button={}  {}",
                    self.name(*button),
                    stacks.join("  ")
                );
            }

            MatchEvent::BlindsPosted { sb_seat, sb_amount, bb_seat, bb_amount } => {
                if !self.verbose {
                    return;
                }
                println!(
                    "    {} posts SB {}   {} posts BB {}",
                    self.name(*sb_seat),
                    sb_amount,
                    self.name(*bb_seat),
                    bb_amount
                );
            }

            MatchEvent::BoardDealt { street, board } => {
                if !self.verbose {
                    return;
                }
                let cards: Vec<String> = board.iter().map(|c| c.to_string()).collect();
                println!("    [{}] {}", street.as_str(), cards.join(" "));
            }

            MatchEvent::ActionTaken { seat, action, amount, outcome, all_in, .. } => {
                // Violations are always reported, even under --quiet: a bot
                // misbehaving is not something to hide.
                match outcome {
                    Outcome::Violation(why) => {
                        println!(
                            "      ! {} INVALID ({why}) -> {}",
                            self.name(*seat),
                            action.as_str()
                        );
                    }
                    Outcome::Normalized(why) if self.verbose => {
                        println!("      ~ {} normalized: {why}", self.name(*seat));
                    }
                    _ => {}
                }
                if !self.verbose {
                    return;
                }
                let detail = match action {
                    Action::Bet { to } | Action::Raise { to } => format!(" to {to}"),
                    Action::Call => format!(" {amount}"),
                    _ => String::new(),
                };
                let allin = if *all_in { "  (all-in)" } else { "" };
                println!("      {} {}{detail}{allin}", self.name(*seat), action.as_str());
            }

            MatchEvent::PotAwarded { amount, winners, hand_class, showdown } => {
                if !self.verbose {
                    return;
                }
                let names: Vec<&str> = winners.iter().map(|&w| self.name(w)).collect();
                let how = match (hand_class, showdown) {
                    (Some(c), true) => format!(" with {c}"),
                    (_, false) => " (everyone else folded)".to_string(),
                    _ => String::new(),
                };
                println!("    pot {} -> {}{}", amount, names.join(", "), how);
            }

            MatchEvent::Showdown { revealed } => {
                if !self.verbose {
                    return;
                }
                let shown: Vec<String> = revealed
                    .iter()
                    .map(|(s, h)| format!("{}: {} {}", self.name(*s), h[0], h[1]))
                    .collect();
                println!("    showdown  {}", shown.join("   |   "));
            }

            MatchEvent::SeatDisqualified { seat, why } => {
                eprintln!("  !! seat {seat} ({}) {why}", self.name(*seat));
            }

            MatchEvent::ActionRequested { .. }
            | MatchEvent::HandEnd { .. }
            | MatchEvent::MatchEnd { .. } => {}
        }
    }
}
