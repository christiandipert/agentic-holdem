//! The match engine: hand loop, betting rules, and the agent bindings.

pub mod protocol;
pub mod seated;
pub mod subprocess;
pub mod table;

pub use seated::{Fault, Native, Seated};
pub use subprocess::{BotSpec, SubprocessAgent};
pub use table::{MatchConfig, SeatStats, Table};
