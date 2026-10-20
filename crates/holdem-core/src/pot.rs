//! Pot construction and awarding, including side pots.
//!
//! Heads-up never needs more than the uncalled-bet refund, but the general
//! layering algorithm handles that case for free, so there is no reason to
//! special-case two players here.

use holdem_agent::{Chips, SeatId};

/// One layer of the pot and who may win it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PotLayer {
    pub amount: Chips,
    /// Seats still in the hand that contributed to this layer's level.
    pub eligible: Vec<SeatId>,
}

/// Split the total wagered into layers by all-in level.
///
/// `committed[i]` is everything seat `i` put in this hand, whether or not it
/// folded; `in_hand[i]` says whether seat `i` can still win. Folded players'
/// chips stay in the pot but win nothing, which is what the two arrays express.
///
/// A top layer with a single eligible seat is the uncalled-bet refund, so no
/// separate rule is needed for it.
pub fn build_layers(committed: &[Chips], in_hand: &[bool]) -> Vec<PotLayer> {
    assert_eq!(committed.len(), in_hand.len());

    let mut levels: Vec<Chips> = committed.iter().copied().filter(|&c| c > 0).collect();
    levels.sort_unstable();
    levels.dedup();

    let mut layers = Vec::new();
    let mut prev = 0;
    for level in levels {
        let amount: Chips = committed
            .iter()
            .map(|&c| c.min(level).saturating_sub(prev))
            .sum();

        let mut eligible: Vec<SeatId> = (0..committed.len())
            .filter(|&i| in_hand[i] && committed[i] >= level)
            .map(|i| i as SeatId)
            .collect();

        // Defensive: a layer nobody can win would strand chips and break the
        // conservation invariant. Hand it to whoever put the most in.
        if eligible.is_empty() {
            let max = committed.iter().copied().max().unwrap_or(0);
            eligible = (0..committed.len())
                .filter(|&i| committed[i] == max)
                .map(|i| i as SeatId)
                .collect();
        }

        if amount > 0 {
            layers.push(PotLayer { amount, eligible });
        }
        prev = level;
    }
    layers
}

/// Split one layer among its winners, giving odd chips to the first winner
/// clockwise from the button, as at a real table.
pub fn split(amount: Chips, winners: &[SeatId], button: SeatId, n_seats: usize) -> Vec<(SeatId, Chips)> {
    assert!(!winners.is_empty());
    let share = amount / winners.len() as Chips;
    let mut odd = amount % winners.len() as Chips;

    let mut payouts: Vec<(SeatId, Chips)> = winners.iter().map(|&w| (w, share)).collect();

    // Seat order starting immediately left of the button.
    for step in 1..=n_seats {
        let seat = ((button as usize + step) % n_seats) as SeatId;
        if odd == 0 {
            break;
        }
        if let Some(p) = payouts.iter_mut().find(|p| p.0 == seat) {
            p.1 += 1;
            odd -= 1;
        }
    }
    payouts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_pot_when_everyone_matched() {
        let layers = build_layers(&[100, 100], &[true, true]);
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].amount, 200);
        assert_eq!(layers[0].eligible, vec![0, 1]);
    }

    #[test]
    fn uncalled_bet_comes_back_as_its_own_layer() {
        // Seat 0 is all-in for 100, seat 1 wagered 300. 200 is uncalled.
        let layers = build_layers(&[100, 300], &[true, true]);
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0], PotLayer { amount: 200, eligible: vec![0, 1] });
        assert_eq!(layers[1], PotLayer { amount: 200, eligible: vec![1] });
    }

    #[test]
    fn folded_chips_stay_in_but_win_nothing() {
        let layers = build_layers(&[50, 200, 200], &[false, true, true]);
        let total: Chips = layers.iter().map(|l| l.amount).sum();
        assert_eq!(total, 450);
        assert_eq!(layers[0].eligible, vec![1, 2], "seat 0 folded");
    }

    #[test]
    fn three_way_all_in_builds_two_side_pots() {
        // 100 / 500 / 500: main pot 300, side pot 800.
        let layers = build_layers(&[100, 500, 500], &[true, true, true]);
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0], PotLayer { amount: 300, eligible: vec![0, 1, 2] });
        assert_eq!(layers[1], PotLayer { amount: 800, eligible: vec![1, 2] });
    }

    #[test]
    fn layers_always_conserve_chips() {
        for &c in &[[0, 0, 0], [10, 0, 0], [10, 20, 30], [7, 7, 7], [1, 100, 50]] {
            let total: Chips = c.iter().sum();
            let layers = build_layers(&c, &[true, true, true]);
            assert_eq!(layers.iter().map(|l| l.amount).sum::<Chips>(), total, "{c:?}");
        }
    }

    #[test]
    fn odd_chip_goes_left_of_the_button() {
        // 101 split two ways: seat 1 is left of button 0, so it gets the extra.
        let p = split(101, &[0, 1], 0, 2);
        assert_eq!(p, vec![(0, 50), (1, 51)]);
        let p = split(101, &[0, 1], 1, 2);
        assert_eq!(p, vec![(0, 51), (1, 50)]);
    }

    #[test]
    fn split_conserves_chips() {
        for amount in [0u64, 1, 2, 3, 100, 101, 999] {
            let p = split(amount, &[0, 1, 2], 1, 3);
            assert_eq!(p.iter().map(|x| x.1).sum::<Chips>(), amount);
        }
    }
}
