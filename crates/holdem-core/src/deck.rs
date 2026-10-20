//! Deck and deal.
//!
//! Two sources, per `PLAN.md` §5.1: a seeded shuffle (reproducible from the seed
//! alone) and a scripted deck for tests whose winner is known independently.

use holdem_agent::Card;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

#[derive(Clone, Debug)]
pub enum DeckSource {
    /// Fisher-Yates with ChaCha8. The same seed always produces the same deck.
    Shuffled { seed: u64 },
    /// Cards dealt from the front, in the given order. For predetermined hands.
    Scripted(Vec<Card>),
}

pub struct Deck {
    cards: Vec<Card>,
    next: usize,
}

impl Deck {
    pub fn new(source: &DeckSource) -> Self {
        let cards = match source {
            DeckSource::Shuffled { seed } => {
                let mut cards: Vec<Card> = (0..52).map(Card).collect();
                let mut rng = ChaCha8Rng::seed_from_u64(*seed);
                cards.shuffle(&mut rng);
                cards
            }
            DeckSource::Scripted(cards) => cards.clone(),
        };
        Deck { cards, next: 0 }
    }

    pub fn deal(&mut self) -> Card {
        let c = self.cards[self.next];
        self.next += 1;
        c
    }

    /// Hold'em burns a card before the flop, turn and river. The engine does
    /// this so that a scripted deck can be written exactly as a real deal.
    pub fn burn(&mut self) {
        self.next += 1;
    }

    pub fn remaining(&self) -> usize {
        self.cards.len() - self.next
    }
}

/// Derive a per-hand deck seed from the match seed, so hand N is reproducible
/// without replaying hands 1..N.
pub fn hand_seed(match_seed: u64, hand_number: u32) -> u64 {
    // SplitMix64 finalizer: cheap, well-distributed avalanche.
    let mut z = match_seed.wrapping_add((hand_number as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Derive an agent's RNG seed from the match seed. Separate stream from the
/// deck's, so an agent cannot infer cards from its own randomness.
pub fn agent_seed(match_seed: u64, seat: u8) -> u64 {
    hand_seed(match_seed ^ 0xA5A5_5A5A_DEAD_BEEF, seat as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn shuffled_deck_is_a_permutation() {
        let d = Deck::new(&DeckSource::Shuffled { seed: 7 });
        let set: HashSet<u8> = d.cards.iter().map(|c| c.0).collect();
        assert_eq!(set.len(), 52);
        assert_eq!(d.remaining(), 52);
    }

    #[test]
    fn same_seed_same_deck() {
        let a = Deck::new(&DeckSource::Shuffled { seed: 42 });
        let b = Deck::new(&DeckSource::Shuffled { seed: 42 });
        let c = Deck::new(&DeckSource::Shuffled { seed: 43 });
        assert_eq!(a.cards, b.cards);
        assert_ne!(a.cards, c.cards);
    }

    #[test]
    fn scripted_deals_in_order() {
        let cards: Vec<Card> = ["As", "Kd", "2c"].iter().map(|s| Card::parse(s).unwrap()).collect();
        let mut d = Deck::new(&DeckSource::Scripted(cards));
        assert_eq!(d.deal().to_string(), "As");
        assert_eq!(d.deal().to_string(), "Kd");
    }

    #[test]
    fn agent_and_deck_seeds_differ() {
        assert_ne!(hand_seed(100, 1), agent_seed(100, 1));
        assert_ne!(agent_seed(100, 0), agent_seed(100, 1));
    }
}
