//! Tests that exercise the real subprocess binding, including the two example
//! bots and several deliberately misbehaving ones.

use holdem_engine::{BotSpec, MatchConfig, Seated, SubprocessAgent, Table};
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/holdem-engine.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .to_str()
        .unwrap()
        .to_string()
}

fn tmp_stderr(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("holdem-tests");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{name}.stderr"))
}

fn spawn(spec: &BotSpec) -> Box<dyn Seated> {
    let path = tmp_stderr(&spec.name);
    Box::new(SubprocessAgent::spawn(spec, &path).expect("spawn bot"))
}

fn cfg(hands: u32, chips: u64) -> MatchConfig {
    MatchConfig {
        starting_stack: chips,
        small_blind: 50,
        big_blind: 100,
        ante: 0,
        max_hands: Some(hands),
        seed: 4242,
        action_timeout_ms: 20_000,
        time_bank_ms: None,
    }
}

fn python(script: String, name: &str) -> BotSpec {
    BotSpec::new(name, &["python3", &script])
}

// ----------------------------------------------------------------- the real bots

#[test]
fn the_two_example_bots_play_a_clean_match() {
    let root = workspace_root();
    let cpp = root.join("bots/cpp_halfpot/halfpot");
    if !cpp.exists() {
        eprintln!("skipping: build the C++ bot first with `make -C bots/cpp_halfpot`");
        return;
    }
    let py = root.join("bots/py_caller/bot.py").to_str().unwrap().to_string();

    let agents: Vec<Box<dyn Seated>> = vec![
        spawn(&BotSpec::new("cpp_halfpot", &[cpp.to_str().unwrap()])),
        spawn(&python(py, "py_caller")),
    ];
    let mut t = Table::new(cfg(30, 10_000), agents);
    t.run();

    assert_eq!(t.stack(0) + t.stack(1), 20_000, "chips conserved");
    assert!(t.hands_played > 0, "at least one hand should be played");
    assert_eq!(t.stats[0].violations, 0, "cpp_halfpot must play legally");
    assert_eq!(t.stats[1].violations, 0, "py_caller must play legally");
    // The bots report their own names in `ready`, which the engine adopts.
    assert_eq!(t.name(0), "cpp_halfpot");
    assert_eq!(t.name(1), "py_caller");
}

#[test]
fn the_python_bot_never_raises() {
    let root = workspace_root();
    let py = root.join("bots/py_caller/bot.py").to_str().unwrap().to_string();
    let agents: Vec<Box<dyn Seated>> = vec![
        spawn(&python(py.clone(), "py_a")),
        spawn(&python(py, "py_b")),
    ];
    let mut t = Table::new(cfg(20, 10_000), agents);
    t.run();

    for i in 0..2 {
        assert_eq!(t.stats[i].bets, 0, "py_caller never bets");
        assert_eq!(t.stats[i].raises, 0, "py_caller never raises");
        assert_eq!(t.stats[i].violations, 0);
    }
    // Two callers never put in more than the blinds, so the match goes the
    // distance and the stacks barely move.
    assert_eq!(t.hands_played, 20);
}

// ----------------------------------------------------------------- misbehaviour

#[test]
fn malformed_replies_are_penalised_and_the_match_continues() {
    let root = workspace_root();
    let py = root.join("bots/py_caller/bot.py").to_str().unwrap().to_string();
    let agents: Vec<Box<dyn Seated>> = vec![
        spawn(&python(fixture("garbage_bot.py"), "garbage")),
        spawn(&python(py, "py_caller")),
    ];
    let mut t = Table::new(cfg(10, 10_000), agents);
    t.run();

    assert!(t.stats[0].violations > 0, "unparseable replies are violations");
    assert_eq!(t.stack(0) + t.stack(1), 20_000, "chips still conserved");
    assert_eq!(t.hands_played, 10, "the match must run to completion");
}

#[test]
fn a_mismatched_reply_id_counts_as_no_answer() {
    let root = workspace_root();
    let py = root.join("bots/py_caller/bot.py").to_str().unwrap().to_string();
    let agents: Vec<Box<dyn Seated>> = vec![
        spawn(&python(fixture("wrong_id_bot.py"), "wrong_id")),
        spawn(&python(py, "py_caller")),
    ];
    let mut t = Table::new(cfg(10, 10_000), agents);
    t.run();

    assert!(
        t.stats[0].violations > 0,
        "a reply we cannot match to our request must not be acted on"
    );
    assert_eq!(t.stack(0) + t.stack(1), 20_000);
}

#[test]
fn a_bot_that_exits_mid_match_is_disqualified_not_a_crash() {
    let root = workspace_root();
    let py = root.join("bots/py_caller/bot.py").to_str().unwrap().to_string();
    let agents: Vec<Box<dyn Seated>> = vec![
        spawn(&python(fixture("quitter_bot.py"), "quitter")),
        spawn(&python(py, "py_caller")),
    ];
    let mut t = Table::new(cfg(50, 10_000), agents);
    t.run();

    assert_eq!(t.stack(0) + t.stack(1), 20_000, "chips conserved");
    assert!(
        t.hands_played < 50,
        "a disqualified seat leaves too few players, ending the match early"
    );
}

#[test]
fn a_bot_that_never_handshakes_does_not_hang_the_engine() {
    let root = workspace_root();
    let py = root.join("bots/py_caller/bot.py").to_str().unwrap().to_string();
    let agents: Vec<Box<dyn Seated>> = vec![
        spawn(&python(fixture("silent_bot.py"), "silent")),
        spawn(&python(py, "py_caller")),
    ];
    let mut t = Table::new(cfg(10, 10_000), agents);
    t.run();

    // The silent bot's pipe closes, so entry fails and the seat is
    // disqualified. With one live seat the match ends without playing a hand.
    assert_eq!(t.hands_played, 0);
    assert_eq!(t.stack(0) + t.stack(1), 20_000);
}
