//! The seam between the table and whatever is actually making decisions.
//!
//! `holdem_agent::Agent` is infallible by design: a native Rust bot always
//! returns an `Action`. A bot behind a pipe can instead fail to answer at all,
//! so the table talks to this slightly wider interface and both bindings
//! implement it. When deadline enforcement and CPU pinning land (`PLAN.md` §4,
//! Addendum B) they become new `Fault` variants and a new implementor here,
//! without the table or the agent contract changing.

use holdem_agent::{Action, Agent, HandResult, HandStart, MatchContext, PlayerView};

/// A bot that could not be consulted, as opposed to one that answered badly.
#[derive(Debug, Clone)]
pub enum Fault {
    /// Answered, but the answer was unusable. Penalised; the bot stays seated.
    Protocol(crate::protocol::ReplyError),
    /// Unreachable: the process exited or the pipe closed. The seat is
    /// disqualified and auto-folds for the rest of the match.
    Dead(String),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Protocol(e) => write!(f, "{e}"),
            Fault::Dead(m) => write!(f, "unreachable: {m}"),
        }
    }
}

impl Fault {
    /// Whether this fault removes the bot from the match for good.
    pub fn is_fatal(&self) -> bool {
        matches!(self, Fault::Dead(_))
    }
}

/// What the table requires of a seat, regardless of binding.
///
/// `Send` so a whole match can be moved onto a worker thread, which is how the
/// GUI runs a live match without blocking its own render loop.
pub trait Seated: Send {
    fn name(&self) -> &str;
    fn entry(&mut self, ctx: &MatchContext) -> Result<(), Fault>;
    fn on_action(&mut self, view: &PlayerView<'_>) -> Result<Action, Fault>;
    fn on_hand_start(&mut self, _start: &HandStart<'_>) -> Result<(), Fault> {
        Ok(())
    }
    fn on_hand_end(&mut self, _result: &HandResult) -> Result<(), Fault> {
        Ok(())
    }
    fn shutdown(&mut self) {}
}

/// Adapts an in-process Rust `Agent` to `Seated`. It can never fault.
pub struct Native<A: Agent>(pub A);

impl<A: Agent> Seated for Native<A> {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn entry(&mut self, ctx: &MatchContext) -> Result<(), Fault> {
        self.0.entry(ctx);
        Ok(())
    }
    fn on_action(&mut self, view: &PlayerView<'_>) -> Result<Action, Fault> {
        Ok(self.0.on_action(view))
    }
    fn on_hand_start(&mut self, start: &HandStart<'_>) -> Result<(), Fault> {
        self.0.on_hand_start(start);
        Ok(())
    }
    fn on_hand_end(&mut self, result: &HandResult) -> Result<(), Fault> {
        self.0.on_hand_end(result);
        Ok(())
    }
    fn shutdown(&mut self) {
        self.0.shutdown();
    }
}
