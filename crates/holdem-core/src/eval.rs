//! Hand evaluation.
//!
//! This is the deliberately simple reference evaluator described in `PLAN.md`
//! §5.2: it scores a 7-card hand by trying all 21 five-card subsets and keeping
//! the best. It is obviously correct by construction, which is what we want
//! first; it is also the oracle a faster evaluator must be validated against
//! before it may replace this one.

use holdem_agent::Card;

/// A packed, comparable hand strength. Higher is better.
///
/// Layout: `category << 20 | r1 << 16 | r2 << 12 | r3 << 8 | r4 << 4 | r5`,
/// where the `r` slots are the category's tiebreakers in significance order.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct HandRank(pub u32);

pub const HIGH_CARD: u32 = 0;
pub const PAIR: u32 = 1;
pub const TWO_PAIR: u32 = 2;
pub const TRIPS: u32 = 3;
pub const STRAIGHT: u32 = 4;
pub const FLUSH: u32 = 5;
pub const FULL_HOUSE: u32 = 6;
pub const QUADS: u32 = 7;
pub const STRAIGHT_FLUSH: u32 = 8;

impl HandRank {
    pub fn category(self) -> u32 {
        self.0 >> 20
    }

    /// Lower-case class name, as it appears in `hand_end` messages.
    pub fn class(self) -> &'static str {
        match self.category() {
            HIGH_CARD => "high_card",
            PAIR => "pair",
            TWO_PAIR => "two_pair",
            TRIPS => "three_of_a_kind",
            STRAIGHT => "straight",
            FLUSH => "flush",
            FULL_HOUSE => "full_house",
            QUADS => "four_of_a_kind",
            STRAIGHT_FLUSH => "straight_flush",
            _ => unreachable!(),
        }
    }
}

fn pack(category: u32, tiebreakers: &[u8]) -> HandRank {
    debug_assert!(tiebreakers.len() <= 5);
    let mut v = category << 20;
    for (i, &r) in tiebreakers.iter().enumerate() {
        v |= (r as u32) << (16 - 4 * i);
    }
    HandRank(v)
}

/// Highest rank index completing a 5-card run, or `None`.
///
/// Returns `3` (the five) for the wheel A-2-3-4-5, since the ace plays low there.
fn straight_high(mask: u16) -> Option<u8> {
    for high in (4u8..13).rev() {
        let need = 0b11111u16 << (high - 4);
        if mask & need == need {
            return Some(high);
        }
    }
    const WHEEL: u16 = (1 << 12) | 1 | (1 << 1) | (1 << 2) | (1 << 3);
    if mask & WHEEL == WHEEL {
        return Some(3);
    }
    None
}

/// Score exactly five cards.
pub fn eval5(cards: &[Card; 5]) -> HandRank {
    let mut counts = [0u8; 13];
    let mut suits = [0u8; 4];
    let mut mask: u16 = 0;
    for c in cards {
        counts[c.rank() as usize] += 1;
        suits[c.suit() as usize] += 1;
        mask |= 1 << c.rank();
    }

    let is_flush = suits.iter().any(|&n| n == 5);
    let straight = straight_high(mask);

    // Ranks ordered by multiplicity first, then by rank: exactly the order in
    // which tiebreakers matter for every category.
    let mut groups: Vec<(u8, u8)> = (0..13u8)
        .rev()
        .filter(|&r| counts[r as usize] > 0)
        .map(|r| (counts[r as usize], r))
        .collect();
    groups.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

    match (is_flush, straight) {
        (true, Some(high)) => return pack(STRAIGHT_FLUSH, &[high]),
        (true, None) => {
            let r: Vec<u8> = groups.iter().map(|g| g.1).collect();
            return pack(FLUSH, &r);
        }
        (false, Some(high)) => return pack(STRAIGHT, &[high]),
        (false, None) => {}
    }

    match groups[0].0 {
        4 => pack(QUADS, &[groups[0].1, groups[1].1]),
        3 if groups[1].0 == 2 => pack(FULL_HOUSE, &[groups[0].1, groups[1].1]),
        3 => pack(TRIPS, &[groups[0].1, groups[1].1, groups[2].1]),
        2 if groups[1].0 == 2 => pack(TWO_PAIR, &[groups[0].1, groups[1].1, groups[2].1]),
        2 => pack(PAIR, &[groups[0].1, groups[1].1, groups[2].1, groups[3].1]),
        _ => {
            let r: Vec<u8> = groups.iter().map(|g| g.1).collect();
            pack(HIGH_CARD, &r)
        }
    }
}

/// Best five-card hand out of seven. All 21 subsets, keep the maximum.
pub fn eval7(cards: &[Card; 7]) -> HandRank {
    let mut best = HandRank(0);
    let mut five = [Card(0); 5];
    for a in 0..7 {
        for b in (a + 1)..7 {
            // The five cards that are neither `a` nor `b`.
            let mut n = 0;
            for i in 0..7 {
                if i != a && i != b {
                    five[n] = cards[i];
                    n += 1;
                }
            }
            let r = eval5(&five);
            if r > best {
                best = r;
            }
        }
    }
    best
}

/// Convenience: two hole cards plus a five-card board.
pub fn eval_hand(hole: [Card; 2], board: &[Card]) -> HandRank {
    assert_eq!(board.len(), 5, "showdown requires a complete board");
    eval7(&[
        hole[0], hole[1], board[0], board[1], board[2], board[3], board[4],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> Vec<Card> {
        s.split_whitespace().map(|c| Card::parse(c).unwrap()).collect()
    }
    fn r5(s: &str) -> HandRank {
        let v = h(s);
        eval5(&[v[0], v[1], v[2], v[3], v[4]])
    }
    fn r7(s: &str) -> HandRank {
        let v = h(s);
        eval7(&[v[0], v[1], v[2], v[3], v[4], v[5], v[6]])
    }

    #[test]
    fn categories_are_recognised() {
        assert_eq!(r5("As Ks Qs Js Ts").category(), STRAIGHT_FLUSH);
        assert_eq!(r5("5s 4s 3s 2s As").category(), STRAIGHT_FLUSH, "steel wheel");
        assert_eq!(r5("9s 9h 9d 9c 2s").category(), QUADS);
        assert_eq!(r5("9s 9h 9d 2c 2s").category(), FULL_HOUSE);
        assert_eq!(r5("As Ks Qs Js 9s").category(), FLUSH);
        assert_eq!(r5("As Kh Qd Jc Ts").category(), STRAIGHT);
        assert_eq!(r5("5s 4h 3d 2c As").category(), STRAIGHT, "wheel");
        assert_eq!(r5("9s 9h 9d 5c 2s").category(), TRIPS);
        assert_eq!(r5("9s 9h 5d 5c 2s").category(), TWO_PAIR);
        assert_eq!(r5("9s 9h 7d 5c 2s").category(), PAIR);
        assert_eq!(r5("As Kh 9d 5c 2s").category(), HIGH_CARD);
    }

    #[test]
    fn wheel_is_the_lowest_straight() {
        assert!(r5("6s 5h 4d 3c 2s") > r5("5s 4h 3d 2c As"));
        assert!(r5("5s 4h 3d 2c As") > r5("As Kh Qd Jc 9s"));
    }

    #[test]
    fn category_ordering_is_total() {
        let ladder = [
            "As Kh 9d 5c 2s",
            "9s 9h 7d 5c 2s",
            "9s 9h 5d 5c 2s",
            "9s 9h 9d 5c 2s",
            "As Kh Qd Jc Ts",
            "As Ks Qs Js 9s",
            "9s 9h 9d 2c 2s",
            "9s 9h 9d 9c 2s",
            "As Ks Qs Js Ts",
        ];
        for w in ladder.windows(2) {
            assert!(r5(w[0]) < r5(w[1]), "{} should lose to {}", w[0], w[1]);
        }
    }

    #[test]
    fn tiebreakers_within_category() {
        assert!(r5("As Ah Kd Qc Js") > r5("Ks Kh Ad Qc Js"), "higher pair wins");
        assert!(r5("As Ah Kd Qc Js") > r5("As Ah Kd Qc 9s"), "kicker decides");
        assert!(r5("As Ah Ks Kh Qd") > r5("As Ah Qs Qh Kd"), "second pair decides");
        assert!(r5("Ks Kh Kd 2c 2s") > r5("Qs Qh Qd Ac As"), "trips rank beats pair rank");
        assert_eq!(r5("As Ah Kd Qc Js"), r5("Ad Ac Ks Qh Jd"), "suits never matter");
    }

    #[test]
    fn seven_card_picks_the_best_five() {
        // Board plays: hole cards are irrelevant against a made board straight.
        assert_eq!(r7("2c 3d As Kh Qd Jc Ts").category(), STRAIGHT);
        // A flush beats the pair the hole cards would otherwise make.
        // (Five spades: A, K, 9, 5, 2.)
        assert_eq!(r7("As Ah 2s 5s 9s Ks 3c").category(), FLUSH);
        // Quads from board trips plus a hole card.
        assert_eq!(r7("9s 2d 9h 9d 9c Kd 3c").category(), QUADS);
    }

    #[test]
    fn eval7_matches_brute_force_over_subsets() {
        // Independent check: the max over all 21 subsets, computed separately.
        let v = h("As Ks 7h 7d 7c 2s 3d");
        let seven: [Card; 7] = [v[0], v[1], v[2], v[3], v[4], v[5], v[6]];
        let mut best = HandRank(0);
        for i in 0..7 {
            for j in (i + 1)..7 {
                let five: Vec<Card> = (0..7).filter(|&k| k != i && k != j).map(|k| seven[k]).collect();
                let rank = eval5(&[five[0], five[1], five[2], five[3], five[4]]);
                best = best.max(rank);
            }
        }
        assert_eq!(eval7(&seven), best);
        assert_eq!(best.category(), TRIPS);
    }
}
