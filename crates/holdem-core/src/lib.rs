//! Pure poker: cards, decks, hand evaluation, and pot math.
//!
//! No agents, no threads, no I/O. Everything here is a plain function over
//! plain data, which is what makes it straightforward to test exhaustively.

pub mod deck;
pub mod eval;
pub mod pot;

pub use deck::{agent_seed, hand_seed, Deck, DeckSource};
pub use eval::{eval5, eval7, eval_hand, HandRank};
pub use pot::{build_layers, split, PotLayer};
