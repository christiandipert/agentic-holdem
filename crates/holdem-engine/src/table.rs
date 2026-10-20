//! The hand loop: blinds, betting rounds, showdown, and payout.

use crate::events::{MatchEvent, Observer, Outcome};
use crate::seated::{Fault, Seated};
use holdem_agent::{
    Action, Card, Chips, HandResult, HandStart, LoggedAction, MatchContext, PlayerView, PotResult,
    SeatId, SeatPublic, SeatStatus, Street,
};
use holdem_core::{build_layers, eval_hand, hand_seed, split, Deck, DeckSource, HandRank};

#[derive(Clone, Debug)]
pub struct MatchConfig {
    pub starting_stack: Chips,
    pub small_blind: Chips,
    pub big_blind: Chips,
    pub ante: Chips,
    pub max_hands: Option<u32>,
    pub seed: u64,
    /// Advisory in this build: sent to bots, not enforced. See `PLAN.md` §4.
    pub action_timeout_ms: u64,
    pub time_bank_ms: Option<u64>,
}

impl Default for MatchConfig {
    fn default() -> Self {
        MatchConfig {
            starting_stack: 10_000,
            small_blind: 50,
            big_blind: 100,
            ante: 0,
            max_hands: Some(200),
            seed: 1,
            action_timeout_ms: 20_000,
            time_bank_ms: Some(60_000),
        }
    }
}

/// Per-seat counters for the end-of-match report.
#[derive(Clone, Debug, Default)]
pub struct SeatStats {
    pub hands_played: u32,
    pub actions: u32,
    pub folds: u32,
    pub checks: u32,
    pub calls: u32,
    pub bets: u32,
    pub raises: u32,
    pub all_ins: u32,
    /// Tier 1: an unambiguous near-miss we corrected and played.
    pub normalizations: u32,
    /// Tier 2: an illegal or unusable action replaced by check-or-fold.
    pub violations: u32,
    pub hands_won: u32,
}

struct Seat {
    stack: Chips,
    street_bet: Chips,
    committed: Chips,
    status: SeatStatus,
    hole: [Card; 2],
    acted: bool,
    stack_at_hand_start: Chips,
}

impl Seat {
    fn public(&self, seat: SeatId) -> SeatPublic {
        SeatPublic {
            seat,
            stack: self.stack,
            street_bet: self.street_bet,
            committed: self.committed,
            status: self.status,
        }
    }
    fn can_act(&self) -> bool {
        self.status == SeatStatus::Active
    }
}

pub struct Table {
    cfg: MatchConfig,
    agents: Vec<Box<dyn Seated>>,
    seats: Vec<Seat>,
    pub stats: Vec<SeatStats>,
    button: SeatId,
    board: Vec<Card>,
    hand_log: Vec<LoggedAction>,
    pub hands_played: u32,
    total_chips: Chips,
    observer: Option<Box<dyn Observer>>,
}

impl Table {
    pub fn new(cfg: MatchConfig, agents: Vec<Box<dyn Seated>>) -> Self {
        let n = agents.len();
        assert!(n >= 2, "need at least two seats");
        assert!(n <= holdem_agent::MAX_SEATS, "at most {} seats", holdem_agent::MAX_SEATS);
        let seats = (0..n)
            .map(|_| Seat {
                stack: cfg.starting_stack,
                street_bet: 0,
                committed: 0,
                status: SeatStatus::Active,
                hole: [Card(0); 2],
                acted: false,
                stack_at_hand_start: cfg.starting_stack,
            })
            .collect();
        Table {
            total_chips: cfg.starting_stack * n as Chips,
            cfg,
            agents,
            seats,
            stats: vec![SeatStats::default(); n],
            button: 0,
            board: Vec::with_capacity(5),
            hand_log: Vec::with_capacity(64),
            hands_played: 0,
            observer: None,
        }
    }

    /// Attach a consumer for match events. The table narrates through this
    /// instead of printing, so the CLI and the GUI see exactly the same thing.
    pub fn with_observer(mut self, observer: Box<dyn Observer>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Events are built before this is called, so nothing borrows `self`.
    fn emit(&mut self, event: MatchEvent) {
        if let Some(o) = self.observer.as_mut() {
            o.on_event(&event);
        }
    }

    fn stacks(&self) -> Vec<Chips> {
        self.seats.iter().map(|s| s.stack).collect()
    }

    fn street_bets(&self) -> Vec<Chips> {
        self.seats.iter().map(|s| s.street_bet).collect()
    }

    fn pot(&self) -> Chips {
        self.seats.iter().map(|s| s.committed).sum()
    }

    pub fn n(&self) -> usize {
        self.seats.len()
    }

    pub fn name(&self, seat: usize) -> &str {
        self.agents[seat].name()
    }

    pub fn stack(&self, seat: usize) -> Chips {
        self.seats[seat].stack
    }

    /// Still in the match: has chips and is reachable.
    fn live(&self, i: usize) -> bool {
        self.seats[i].stack > 0 && self.seats[i].status != SeatStatus::Disqualified
    }

    fn live_count(&self) -> usize {
        (0..self.n()).filter(|&i| self.live(i)).count()
    }

    fn in_hand_count(&self) -> usize {
        self.seats.iter().filter(|s| s.status.in_hand()).count()
    }

    fn active_count(&self) -> usize {
        self.seats.iter().filter(|s| s.can_act()).count()
    }

    fn next_occupied(&self, from: SeatId) -> SeatId {
        let n = self.n();
        for step in 1..=n {
            let i = (from as usize + step) % n;
            if self.live(i) {
                return i as SeatId;
            }
        }
        from
    }

    // ------------------------------------------------------------ entry

    pub fn entry_all(&mut self) {
        let n = self.n() as u8;
        for i in 0..self.n() {
            let ctx = MatchContext {
                me: i as SeatId,
                n_seats: n,
                starting_stack: self.cfg.starting_stack,
                small_blind: self.cfg.small_blind,
                big_blind: self.cfg.big_blind,
                ante: self.cfg.ante,
                max_hands: self.cfg.max_hands,
                action_timeout_ms: self.cfg.action_timeout_ms,
                time_bank_ms: self.cfg.time_bank_ms,
                rng_seed: holdem_core::agent_seed(self.cfg.seed, i as u8),
            };
            if let Err(e) = self.agents[i].entry(&ctx) {
                self.seats[i].status = SeatStatus::Disqualified;
                let why = format!("failed to start: {e}");
                self.emit(MatchEvent::SeatDisqualified { seat: i as SeatId, why });
            }
        }
    }

    // ------------------------------------------------------------ match loop

    pub fn run(&mut self) {
        let start = MatchEvent::MatchStart {
            names: (0..self.n()).map(|i| self.agents[i].name().to_string()).collect(),
            starting_stack: self.cfg.starting_stack,
            small_blind: self.cfg.small_blind,
            big_blind: self.cfg.big_blind,
            seed: self.cfg.seed,
            max_hands: self.cfg.max_hands,
        };
        self.emit(start);
        self.entry_all();
        // Put the button somewhere live before the first hand.
        if !self.live(self.button as usize) {
            self.button = self.next_occupied(self.button);
        }

        loop {
            if self.live_count() < 2 {
                break;
            }
            if let Some(max) = self.cfg.max_hands {
                if self.hands_played >= max {
                    break;
                }
            }
            self.play_hand();
            self.button = self.next_occupied(self.button);
        }

        let end = MatchEvent::MatchEnd {
            hands: self.hands_played,
            stacks: self.stacks(),
        };
        self.emit(end);

        for a in self.agents.iter_mut() {
            a.shutdown();
        }
    }

    // ------------------------------------------------------------ one hand

    fn play_hand(&mut self) {
        self.hands_played += 1;
        let hand_number = self.hands_played;

        // --- reset
        self.board.clear();
        self.hand_log.clear();
        for i in 0..self.n() {
            let s = &mut self.seats[i];
            s.street_bet = 0;
            s.committed = 0;
            s.acted = false;
            s.stack_at_hand_start = s.stack;
            s.status = if s.status == SeatStatus::Disqualified {
                SeatStatus::Disqualified
            } else if s.stack > 0 {
                SeatStatus::Active
            } else {
                SeatStatus::Busted
            };
        }

        // --- deal
        let mut deck = Deck::new(&DeckSource::Shuffled {
            seed: hand_seed(self.cfg.seed, hand_number),
        });
        for i in 0..self.n() {
            if self.seats[i].can_act() {
                self.seats[i].hole = [deck.deal(), deck.deal()];
                self.stats[i].hands_played += 1;
            }
        }

        let ev = MatchEvent::HandStart {
            hand: hand_number,
            button: self.button,
            stacks: self.stacks(),
            holes: (0..self.n())
                .map(|i| self.seats[i].can_act().then_some(self.seats[i].hole))
                .collect(),
        };
        self.emit(ev);

        // --- hand_start notification
        let snapshot: Vec<SeatPublic> = (0..self.n()).map(|i| self.seats[i].public(i as SeatId)).collect();
        for i in 0..self.n() {
            if !self.seats[i].can_act() {
                continue;
            }
            let start = HandStart {
                hand_number,
                button: self.button,
                hole: self.seats[i].hole,
                seats: &snapshot,
                small_blind: self.cfg.small_blind,
                big_blind: self.cfg.big_blind,
            };
            if self.agents[i].on_hand_start(&start).is_err() {
                self.kill_seat(i, "died during hand_start");
            }
        }

        // --- antes and blinds
        if self.cfg.ante > 0 {
            for i in 0..self.n() {
                if self.seats[i].can_act() {
                    let a = self.cfg.ante;
                    self.commit(i, a);
                }
            }
            // Antes are dead money: they belong to the pot, not to a street bet.
            for i in 0..self.n() {
                self.seats[i].street_bet = 0;
            }
        }

        let (sb_seat, bb_seat) = self.blind_seats();
        let sb_amount = self.commit(sb_seat, self.cfg.small_blind);
        let bb_amount = self.commit(bb_seat, self.cfg.big_blind);
        self.emit(MatchEvent::BlindsPosted {
            sb_seat: sb_seat as SeatId,
            sb_amount,
            bb_seat: bb_seat as SeatId,
            bb_amount,
        });

        // --- streets
        let streets = [Street::Preflop, Street::Flop, Street::Turn, Street::River];
        for (idx, &street) in streets.iter().enumerate() {
            if idx > 0 {
                // Burn, then deal. Burning keeps a scripted deck identical to a
                // real deal, which matters for the predetermined-hand tests.
                deck.burn();
                let n_cards = if street == Street::Flop { 3 } else { 1 };
                for _ in 0..n_cards {
                    self.board.push(deck.deal());
                }
                if self.in_hand_count() > 1 {
                    let ev = MatchEvent::BoardDealt {
                        street,
                        board: self.board.clone(),
                    };
                    self.emit(ev);
                }
            }
            if self.in_hand_count() <= 1 {
                break;
            }
            self.betting_round(street, hand_number);
            if self.in_hand_count() <= 1 {
                break;
            }
        }

        self.settle(hand_number);
        self.assert_chips();
    }

    /// Heads-up inverts the blinds: the button posts the small blind. This is
    /// the classic place to get two-player poker wrong.
    fn blind_seats(&self) -> (usize, usize) {
        let live: Vec<usize> = (0..self.n()).filter(|&i| self.live(i)).collect();
        if live.len() == 2 {
            let sb = self.button as usize;
            let bb = self.next_occupied(self.button) as usize;
            (sb, bb)
        } else {
            let sb = self.next_occupied(self.button) as usize;
            let bb = self.next_occupied(sb as SeatId) as usize;
            (sb, bb)
        }
    }

    /// Heads-up: the button acts first preflop and last on every later street.
    fn first_to_act(&self, street: Street) -> SeatId {
        let live = self.live_count();
        if street == Street::Preflop {
            if live == 2 {
                self.button
            } else {
                let sb = self.next_occupied(self.button);
                let bb = self.next_occupied(sb);
                self.next_occupied(bb)
            }
        } else {
            self.next_occupied(self.button)
        }
    }

    fn commit(&mut self, i: usize, amount: Chips) -> Chips {
        let s = &mut self.seats[i];
        let amt = amount.min(s.stack);
        s.stack -= amt;
        s.street_bet += amt;
        s.committed += amt;
        if s.stack == 0 && s.status == SeatStatus::Active {
            s.status = SeatStatus::AllIn;
        }
        amt
    }

    fn kill_seat(&mut self, i: usize, why: &str) {
        self.seats[i].status = SeatStatus::Disqualified;
        let ev = MatchEvent::SeatDisqualified {
            seat: i as SeatId,
            why: why.to_string(),
        };
        self.emit(ev);
    }

    // ------------------------------------------------------------ betting

    fn betting_round(&mut self, street: Street, hand_number: u32) {
        if street != Street::Preflop {
            for s in self.seats.iter_mut() {
                s.street_bet = 0;
            }
        }
        for s in self.seats.iter_mut() {
            s.acted = false;
        }

        let mut current_bet = self.seats.iter().map(|s| s.street_bet).max().unwrap_or(0);
        let mut min_raise = self.cfg.big_blind;

        let mut idx = self.first_to_act(street);
        let mut guard = 0usize;

        loop {
            guard += 1;
            assert!(guard < 10_000, "betting round failed to terminate");

            if self.in_hand_count() <= 1 || self.active_count() == 0 {
                break;
            }
            if self.round_complete(current_bet) {
                break;
            }
            let i = idx as usize;
            idx = self.next_in_hand(idx);
            if !self.seats[i].can_act() {
                continue;
            }

            let (to_call, min_raise_to, max_raise_to) = self.limits(i, current_bet, min_raise);
            self.emit(MatchEvent::ActionRequested {
                seat: i as SeatId,
                street,
                to_call,
                min_raise_to,
                max_raise_to,
            });
            let requested = self.ask(i, street, hand_number, to_call, min_raise_to, max_raise_to, current_bet);

            let (action, resolution) = match requested {
                Ok(a) => Self::resolve(a, to_call, min_raise_to, max_raise_to, current_bet),
                Err(fault) => {
                    let fatal = fault.is_fatal();
                    let msg = fault.to_string();
                    if fatal {
                        self.kill_seat(i, &format!("unreachable ({msg})"));
                    }
                    (
                        if to_call == 0 { Action::Check } else { Action::Fold },
                        Outcome::Violation(msg),
                    )
                }
            };

            match &resolution {
                Outcome::Clean => {}
                Outcome::Normalized(_) => self.stats[i].normalizations += 1,
                Outcome::Violation(_) => self.stats[i].violations += 1,
            }

            let moved = self.apply(i, action, to_call, &mut current_bet, &mut min_raise);
            self.stats[i].actions += 1;
            match action {
                Action::Fold => self.stats[i].folds += 1,
                Action::Check => self.stats[i].checks += 1,
                Action::Call => self.stats[i].calls += 1,
                Action::Bet { .. } => self.stats[i].bets += 1,
                Action::Raise { .. } => self.stats[i].raises += 1,
                Action::AllIn => self.stats[i].all_ins += 1,
            }
            if self.seats[i].status == SeatStatus::AllIn && !matches!(action, Action::AllIn) {
                // Reached all-in via a call or raise that consumed the stack.
                self.stats[i].all_ins += 1;
            }

            self.hand_log.push(LoggedAction {
                seat: i as SeatId,
                street,
                action,
                amount: moved,
            });

            let ev = MatchEvent::ActionTaken {
                seat: i as SeatId,
                street,
                action,
                amount: moved,
                outcome: resolution,
                all_in: self.seats[i].status == SeatStatus::AllIn,
                stacks: self.stacks(),
                street_bets: self.street_bets(),
                pot: self.pot(),
                current_bet,
            };
            self.emit(ev);
        }
    }

    fn next_in_hand(&self, from: SeatId) -> SeatId {
        let n = self.n();
        for step in 1..=n {
            let i = (from as usize + step) % n;
            if self.seats[i].status.in_hand() {
                return i as SeatId;
            }
        }
        from
    }

    fn round_complete(&self, current_bet: Chips) -> bool {
        if self.in_hand_count() <= 1 {
            return true;
        }
        let actives: Vec<&Seat> = self.seats.iter().filter(|s| s.can_act()).collect();
        if actives.is_empty() {
            return true;
        }
        actives.iter().all(|s| s.acted && s.street_bet == current_bet)
    }

    /// `(to_call, min_raise_to, max_raise_to)` for one seat.
    ///
    /// `min_raise_to == 0` means raising is impossible: the seat cannot even
    /// cover the outstanding bet. Any value in `[min_raise_to, max_raise_to]`
    /// is otherwise a legal raise, including a short all-in below the normal
    /// minimum.
    fn limits(&self, i: usize, current_bet: Chips, min_raise: Chips) -> (Chips, Chips, Chips) {
        let s = &self.seats[i];
        let to_call = current_bet.saturating_sub(s.street_bet).min(s.stack);
        let max_raise_to = s.street_bet + s.stack;
        let min_raise_to = if max_raise_to <= current_bet {
            0
        } else {
            let target = if current_bet == 0 {
                self.cfg.big_blind
            } else {
                current_bet + min_raise
            };
            target.min(max_raise_to)
        };
        (to_call, min_raise_to, max_raise_to)
    }

    #[allow(clippy::too_many_arguments)]
    fn ask(
        &mut self,
        i: usize,
        street: Street,
        hand_number: u32,
        to_call: Chips,
        min_raise_to: Chips,
        max_raise_to: Chips,
        current_bet: Chips,
    ) -> Result<Action, Fault> {
        let snapshot: Vec<SeatPublic> = (0..self.n()).map(|k| self.seats[k].public(k as SeatId)).collect();
        let pot: Chips = self.seats.iter().map(|s| s.committed).sum();
        let view = PlayerView {
            me: i as SeatId,
            button: self.button,
            street,
            hand_number,
            hole: self.seats[i].hole,
            board: &self.board,
            pot,
            current_bet,
            to_call,
            min_raise_to,
            max_raise_to,
            big_blind: self.cfg.big_blind,
            seats: &snapshot,
            hand_log: &self.hand_log,
            time_remaining_ms: self.cfg.action_timeout_ms,
            time_bank_remaining_ms: self.cfg.time_bank_ms,
        };
        self.agents[i].on_action(&view)
    }

    /// The two-tier policy from `AGENT_SPEC.md` §5.2. Tier 1 corrects
    /// unambiguous near-misses; Tier 2 substitutes check-or-fold.
    fn resolve(
        requested: Action,
        to_call: Chips,
        min_raise_to: Chips,
        max_raise_to: Chips,
        current_bet: Chips,
    ) -> (Action, Outcome) {
        let fallback = if to_call == 0 { Action::Check } else { Action::Fold };
        let violation = |why: String| (fallback, Outcome::Violation(why));

        match requested {
            Action::Fold => (Action::Fold, Outcome::Clean),

            Action::Check => {
                if to_call == 0 {
                    (Action::Check, Outcome::Clean)
                } else {
                    violation(format!("check facing a bet of {to_call}"))
                }
            }

            Action::Call => {
                if to_call == 0 {
                    (
                        Action::Check,
                        Outcome::Normalized("call with nothing to call -> check".into()),
                    )
                } else {
                    (Action::Call, Outcome::Clean)
                }
            }

            Action::AllIn => {
                if max_raise_to > current_bet {
                    (Action::Raise { to: max_raise_to }, Outcome::Clean)
                } else {
                    (
                        Action::Call,
                        Outcome::Normalized("all-in for less than the bet -> all-in call".into()),
                    )
                }
            }

            Action::Bet { to } => {
                if current_bet > 0 {
                    return violation(format!(
                        "bet of {to} while {current_bet} is already outstanding; that is a raise"
                    ));
                }
                if to == 0 {
                    return violation("bet of zero".into());
                }
                if min_raise_to == 0 {
                    return violation(format!("bet of {to} but betting is not legal here"));
                }
                Self::resolve_amount(to, min_raise_to, max_raise_to, true, fallback)
            }

            Action::Raise { to } => {
                if min_raise_to == 0 {
                    return violation(format!(
                        "raise to {to} but raising is not legal here (cannot cover {current_bet})"
                    ));
                }
                let opening = current_bet == 0;
                let (action, res) =
                    Self::resolve_amount(to, min_raise_to, max_raise_to, opening, fallback);
                if opening {
                    if let Outcome::Clean = res {
                        return (
                            action,
                            Outcome::Normalized("raise with no bet outstanding -> bet".into()),
                        );
                    }
                }
                (action, res)
            }
        }
    }

    fn resolve_amount(
        mut to: Chips,
        min_raise_to: Chips,
        max_raise_to: Chips,
        opening: bool,
        fallback: Action,
    ) -> (Action, Outcome) {
        debug_assert!(min_raise_to > 0, "callers must reject an illegal raise first");
        let mut note: Option<String> = None;

        if to > max_raise_to {
            note = Some(format!("{to} exceeds the stack; clamped to all-in {max_raise_to}"));
            to = max_raise_to;
        }
        if to < min_raise_to {
            // The clamp above guarantees to <= max_raise_to, and
            // min_raise_to <= max_raise_to, so this is genuinely too small.
            return (
                fallback,
                Outcome::Violation(format!("{to} is below the minimum of {min_raise_to}")),
            );
        }

        let action = if opening { Action::Bet { to } } else { Action::Raise { to } };
        match note {
            Some(n) => (action, Outcome::Normalized(n)),
            None => (action, Outcome::Clean),
        }
    }

    /// Returns the chips this action moved.
    fn apply(
        &mut self,
        i: usize,
        action: Action,
        to_call: Chips,
        current_bet: &mut Chips,
        min_raise: &mut Chips,
    ) -> Chips {
        self.seats[i].acted = true;
        match action {
            Action::Fold => {
                self.seats[i].status = SeatStatus::Folded;
                0
            }
            Action::Check => 0,
            Action::Call => self.commit(i, to_call),
            Action::Bet { to } | Action::Raise { to } => {
                let delta = to.saturating_sub(self.seats[i].street_bet);
                let moved = self.commit(i, delta);
                let new_bet = self.seats[i].street_bet;
                if new_bet > *current_bet {
                    *min_raise = (new_bet - *current_bet).max(1);
                    *current_bet = new_bet;
                    // A raise reopens the action for everyone still able to act.
                    for (k, s) in self.seats.iter_mut().enumerate() {
                        if k != i && s.can_act() {
                            s.acted = false;
                        }
                    }
                }
                moved
            }
            // Normalised away before reaching here.
            Action::AllIn => {
                let all = self.seats[i].stack;
                self.commit(i, all)
            }
        }
    }

    // ------------------------------------------------------------ showdown

    fn settle(&mut self, hand_number: u32) {
        let n = self.n();
        let committed: Vec<Chips> = self.seats.iter().map(|s| s.committed).collect();
        let in_hand: Vec<bool> = self.seats.iter().map(|s| s.status.in_hand()).collect();
        let layers = build_layers(&committed, &in_hand);

        let showdown = self.in_hand_count() > 1;
        let ranks: Vec<Option<HandRank>> = (0..n)
            .map(|i| {
                if showdown && in_hand[i] && self.board.len() == 5 {
                    Some(eval_hand(self.seats[i].hole, &self.board))
                } else {
                    None
                }
            })
            .collect();

        let mut pots: Vec<PotResult> = Vec::new();
        for layer in &layers {
            let winners: Vec<SeatId> = if layer.eligible.len() == 1 {
                layer.eligible.clone()
            } else {
                let best = layer
                    .eligible
                    .iter()
                    .filter_map(|&s| ranks[s as usize])
                    .max();
                match best {
                    Some(b) => layer
                        .eligible
                        .iter()
                        .copied()
                        .filter(|&s| ranks[s as usize] == Some(b))
                        .collect(),
                    None => layer.eligible.clone(),
                }
            };

            let class = winners
                .first()
                .and_then(|&w| ranks[w as usize])
                .map(|r| r.class().to_string());

            for (seat, amount) in split(layer.amount, &winners, self.button, n) {
                self.seats[seat as usize].stack += amount;
            }
            pots.push(PotResult {
                amount: layer.amount,
                eligible: layer.eligible.clone(),
                winners,
                hand_class: if showdown { class } else { None },
            });
        }

        let deltas: Vec<(SeatId, i64, Chips)> = (0..n)
            .map(|i| {
                let before = self.seats[i].stack_at_hand_start as i64;
                let after = self.seats[i].stack as i64;
                (i as SeatId, after - before, self.seats[i].stack)
            })
            .collect();

        // "Won the hand" means finished it with more chips than it started.
        // Unambiguous, and it does not count an uncalled bet coming back as a win.
        for &(seat, delta, _) in &deltas {
            if delta > 0 {
                self.stats[seat as usize].hands_won += 1;
            }
        }

        let revealed: Vec<(SeatId, [Card; 2])> = if showdown {
            (0..n)
                .filter(|&i| in_hand[i])
                .map(|i| (i as SeatId, self.seats[i].hole))
                .collect()
        } else {
            Vec::new()
        };

        // Bust anyone who ran out of chips.
        for i in 0..n {
            if self.seats[i].stack == 0 && self.seats[i].status != SeatStatus::Disqualified {
                self.seats[i].status = SeatStatus::Busted;
            }
        }

        for p in &pots {
            let ev = MatchEvent::PotAwarded {
                amount: p.amount,
                winners: p.winners.clone(),
                hand_class: p.hand_class.clone(),
                showdown,
            };
            self.emit(ev);
        }
        if showdown {
            let ev = MatchEvent::Showdown {
                revealed: revealed.clone(),
            };
            self.emit(ev);
        }
        let ev = MatchEvent::HandEnd {
            hand: hand_number,
            stacks: self.stacks(),
            deltas: deltas.iter().map(|d| d.1).collect(),
        };
        self.emit(ev);

        let result = HandResult {
            hand_number,
            board: self.board.clone(),
            deltas,
            pots,
            revealed,
        };
        for i in 0..n {
            if self.agents[i].on_hand_end(&result).is_err() {
                self.kill_seat(i, "died during hand_end");
            }
        }
    }

    /// Chips are never created or destroyed. `PLAN.md` §5.4 makes this an
    /// invariant rather than a spot check, so it runs on every hand.
    fn assert_chips(&self) {
        let total: Chips = self.seats.iter().map(|s| s.stack).sum();
        assert_eq!(
            total, self.total_chips,
            "chip conservation violated after hand {}: {} != {}",
            self.hands_played, total, self.total_chips
        );
    }
}
