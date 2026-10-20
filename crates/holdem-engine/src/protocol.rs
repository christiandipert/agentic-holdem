//! The wire protocol: newline-delimited JSON on the bot's stdin/stdout.
//!
//! This is binding 2 from `PLAN.md` Addendum A. The engine builds messages here
//! rather than deriving them from the public view types, so `holdem-agent` stays
//! dependency-free and the wire format can change without touching the contract
//! every bot compiles against.

use holdem_agent::{Action, Card, Chips, HandResult, HandStart, MatchContext, PlayerView, SeatPublic};
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: u64 = 1;

fn cards_json(cards: &[Card]) -> Value {
    Value::Array(cards.iter().map(|c| json!(c.to_string())).collect())
}

fn seats_json(seats: &[SeatPublic]) -> Value {
    Value::Array(
        seats
            .iter()
            .map(|s| {
                json!({
                    "seat": s.seat,
                    "stack": s.stack,
                    "street_bet": s.street_bet,
                    "committed": s.committed,
                    "status": s.status.as_str(),
                })
            })
            .collect(),
    )
}

pub fn entry_msg(id: u64, ctx: &MatchContext) -> Value {
    json!({
        "v": PROTOCOL_VERSION,
        "type": "entry",
        "id": id,
        "me": ctx.me,
        "n_seats": ctx.n_seats,
        "starting_stack": ctx.starting_stack,
        "small_blind": ctx.small_blind,
        "big_blind": ctx.big_blind,
        "ante": ctx.ante,
        "max_hands": ctx.max_hands,
        "action_timeout_ms": ctx.action_timeout_ms,
        "time_bank_ms": ctx.time_bank_ms,
        "rng_seed": ctx.rng_seed,
        "hand_history_provided": true,
    })
}

pub fn hand_start_msg(s: &HandStart<'_>) -> Value {
    json!({
        "v": PROTOCOL_VERSION,
        "type": "hand_start",
        "id": 0,
        "hand_number": s.hand_number,
        "button": s.button,
        "hole": cards_json(&s.hole),
        "seats": seats_json(s.seats),
        "small_blind": s.small_blind,
        "big_blind": s.big_blind,
    })
}

pub fn action_request_msg(id: u64, v: &PlayerView<'_>) -> Value {
    json!({
        "v": PROTOCOL_VERSION,
        "type": "action_request",
        "id": id,
        "me": v.me,
        "hand_number": v.hand_number,
        "street": v.street.as_str(),
        "button": v.button,
        "hole": cards_json(&v.hole),
        "board": cards_json(v.board),
        "pot": v.pot,
        "current_bet": v.current_bet,
        "to_call": v.to_call,
        "min_raise_to": v.min_raise_to,
        "max_raise_to": v.max_raise_to,
        "big_blind": v.big_blind,
        "seats": seats_json(v.seats),
        "hand_log": Value::Array(v.hand_log.iter().map(|a| json!({
            "seat": a.seat,
            "street": a.street.as_str(),
            "action": a.action.as_str(),
            "amount": a.amount,
        })).collect()),
        "time_remaining_ms": v.time_remaining_ms,
        "time_bank_remaining_ms": v.time_bank_remaining_ms,
    })
}

pub fn hand_end_msg(r: &HandResult) -> Value {
    json!({
        "v": PROTOCOL_VERSION,
        "type": "hand_end",
        "id": 0,
        "hand_number": r.hand_number,
        "board": cards_json(&r.board),
        "deltas": Value::Array(r.deltas.iter().map(|(seat, delta, stack)| json!({
            "seat": seat, "delta": delta, "stack": stack,
        })).collect()),
        "pots": Value::Array(r.pots.iter().map(|p| json!({
            "amount": p.amount,
            "eligible": p.eligible,
            "winners": p.winners,
            "hand_class": p.hand_class,
        })).collect()),
        "revealed": Value::Array(r.revealed.iter().map(|(seat, hole)| json!({
            "seat": seat, "hole": cards_json(hole),
        })).collect()),
    })
}

pub fn shutdown_msg() -> Value {
    json!({ "v": PROTOCOL_VERSION, "type": "shutdown", "id": 0 })
}

// ------------------------------------------------------------ replies

/// Why a reply could not be used. Every variant maps to a Tier 2 penalty
/// (`AGENT_SPEC.md` §5.2): check if nothing is owed, otherwise fold.
#[derive(Debug, Clone)]
pub enum ReplyError {
    /// The line was not valid JSON, or not a JSON object.
    Malformed(String),
    /// Well-formed JSON, but not the message we asked for.
    WrongType { expected: &'static str, got: String },
    /// A stale or mismatched reply. Discarded, per Addendum A §A.3.2.
    IdMismatch { expected: u64, got: Option<u64> },
    /// Unknown action string, or a bet/raise with no usable amount.
    BadAction(String),
    /// Protocol major version we do not speak.
    VersionMismatch(u64),
}

impl std::fmt::Display for ReplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplyError::Malformed(s) => write!(f, "malformed reply: {s}"),
            ReplyError::WrongType { expected, got } => {
                write!(f, "expected type {expected:?}, got {got:?}")
            }
            ReplyError::IdMismatch { expected, got } => {
                write!(f, "id mismatch: expected {expected}, got {got:?}")
            }
            ReplyError::BadAction(s) => write!(f, "bad action: {s}"),
            ReplyError::VersionMismatch(v) => write!(f, "unsupported protocol version {v}"),
        }
    }
}

fn parse_object(line: &str) -> Result<Value, ReplyError> {
    let v: Value = serde_json::from_str(line.trim())
        .map_err(|e| ReplyError::Malformed(format!("{e} in {:?}", truncate(line, 120))))?;
    if !v.is_object() {
        return Err(ReplyError::Malformed("top level is not an object".into()));
    }
    if let Some(ver) = v.get("v").and_then(Value::as_u64) {
        if ver != PROTOCOL_VERSION {
            return Err(ReplyError::VersionMismatch(ver));
        }
    }
    Ok(v)
}

fn truncate(s: &str, n: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= n {
        t.to_string()
    } else {
        format!("{}...", t.chars().take(n).collect::<String>())
    }
}

/// Parse a `ready` reply, returning the name the bot calls itself.
pub fn parse_ready(line: &str, expect_id: u64) -> Result<Option<String>, ReplyError> {
    let v = parse_object(line)?;
    match v.get("type").and_then(Value::as_str) {
        Some("ready") => {}
        other => {
            return Err(ReplyError::WrongType {
                expected: "ready",
                got: other.unwrap_or("<missing>").to_string(),
            })
        }
    }
    let id = v.get("id").and_then(Value::as_u64);
    if id != Some(expect_id) {
        return Err(ReplyError::IdMismatch { expected: expect_id, got: id });
    }
    Ok(v.get("name").and_then(Value::as_str).map(str::to_string))
}

/// Parse an `action` reply into the action the bot *requested*. Whether that
/// action is legal is decided later, by the table (see `table::resolve`).
pub fn parse_action(line: &str, expect_id: u64) -> Result<Action, ReplyError> {
    let v = parse_object(line)?;
    match v.get("type").and_then(Value::as_str) {
        Some("action") => {}
        other => {
            return Err(ReplyError::WrongType {
                expected: "action",
                got: other.unwrap_or("<missing>").to_string(),
            })
        }
    }
    let id = v.get("id").and_then(Value::as_u64);
    if id != Some(expect_id) {
        return Err(ReplyError::IdMismatch { expected: expect_id, got: id });
    }

    let name = v
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| ReplyError::BadAction("missing \"action\" field".into()))?;

    // `amount` must be a non-negative integer when it is needed. A float, a
    // string, or a negative number is a violation rather than something to coerce.
    let amount = || -> Result<Chips, ReplyError> {
        match v.get("amount") {
            Some(Value::Number(n)) => n
                .as_u64()
                .ok_or_else(|| ReplyError::BadAction(format!("amount {n} is not a u64"))),
            Some(other) => Err(ReplyError::BadAction(format!("amount {other} is not a number"))),
            None => Err(ReplyError::BadAction(format!("{name:?} requires an amount"))),
        }
    };

    match name {
        "fold" => Ok(Action::Fold),
        "check" => Ok(Action::Check),
        "call" => Ok(Action::Call),
        "allin" | "all_in" => Ok(Action::AllIn),
        "bet" => Ok(Action::Bet { to: amount()? }),
        "raise" => Ok(Action::Raise { to: amount()? }),
        other => Err(ReplyError::BadAction(format!("unknown action {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_action() {
        assert_eq!(parse_action(r#"{"v":1,"type":"action","id":1,"action":"fold"}"#, 1).unwrap(), Action::Fold);
        assert_eq!(parse_action(r#"{"v":1,"type":"action","id":1,"action":"check"}"#, 1).unwrap(), Action::Check);
        assert_eq!(parse_action(r#"{"v":1,"type":"action","id":1,"action":"call"}"#, 1).unwrap(), Action::Call);
        assert_eq!(parse_action(r#"{"v":1,"type":"action","id":1,"action":"allin"}"#, 1).unwrap(), Action::AllIn);
        assert_eq!(
            parse_action(r#"{"v":1,"type":"action","id":1,"action":"raise","amount":900}"#, 1).unwrap(),
            Action::Raise { to: 900 }
        );
        assert_eq!(
            parse_action(r#"{"v":1,"type":"action","id":1,"action":"bet","amount":50}"#, 1).unwrap(),
            Action::Bet { to: 50 }
        );
    }

    #[test]
    fn tolerates_unknown_fields() {
        // The forward-compatibility rule cuts both ways: we ignore extras too.
        let line = r#"{"v":1,"type":"action","id":1,"action":"call","note":"hi","extra":[1,2]}"#;
        assert_eq!(parse_action(line, 1).unwrap(), Action::Call);
    }

    #[test]
    fn rejects_bad_replies() {
        let cases = [
            r#"not json at all"#,
            r#"[1,2,3]"#,
            r#"{"v":1,"type":"ready","id":1}"#,
            r#"{"v":1,"type":"action","id":99,"action":"call"}"#,
            r#"{"v":1,"type":"action","id":1,"action":"shove"}"#,
            r#"{"v":1,"type":"action","id":1,"action":"raise"}"#,
            r#"{"v":1,"type":"action","id":1,"action":"raise","amount":-5}"#,
            r#"{"v":1,"type":"action","id":1,"action":"raise","amount":1.5}"#,
            r#"{"v":1,"type":"action","id":1,"action":"raise","amount":"900"}"#,
            r#"{"v":2,"type":"action","id":1,"action":"call"}"#,
            r#"{}"#,
        ];
        for c in cases {
            assert!(parse_action(c, 1).is_err(), "should have rejected: {c}");
        }
    }

    #[test]
    fn ready_returns_the_name() {
        let line = r#"{"v":1,"type":"ready","id":1,"name":"py_caller","language":"python"}"#;
        assert_eq!(parse_ready(line, 1).unwrap().as_deref(), Some("py_caller"));
    }

    #[test]
    fn missing_v_is_accepted_as_version_1() {
        // `v` is required of bots by the spec, but omitting it is not worth a
        // penalty when the rest of the message is unambiguous.
        assert_eq!(parse_action(r#"{"type":"action","id":1,"action":"call"}"#, 1).unwrap(), Action::Call);
    }
}
