//! Command-line arena: prompt for the starting stack, run a match, report.

use holdem_engine::{BotSpec, MatchConfig, Seated, SubprocessAgent, Table};
use std::io::{self, Write};
use std::path::PathBuf;

const USAGE: &str = "\
agentic-holdem arena

USAGE:
    holdem [OPTIONS]

OPTIONS:
    --chips <N>       Starting chips per bot (otherwise you will be prompted)
    --sb <N>          Small blind            [default: 50]
    --bb <N>          Big blind              [default: 100]
    --hands <N>       Hand limit             [default: 200]
    --seed <N>        Match seed; same seed replays the same cards [default: random]
    --bot \"<argv>\"    Add a bot by command line; repeatable. Defaults to the
                      two example bots when not given.
    --quiet           Only print the final report
    -h, --help        Show this message
";

struct Args {
    chips: Option<u64>,
    sb: u64,
    bb: u64,
    hands: u32,
    seed: Option<u64>,
    bots: Vec<String>,
    quiet: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        chips: None,
        sb: 50,
        bb: 100,
        hands: 200,
        seed: None,
        bots: Vec::new(),
        quiet: false,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let next = |i: usize| -> Result<String, String> {
            argv.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{} needs a value", argv[i]))
        };
        match argv[i].as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--quiet" => a.quiet = true,
            "--chips" => {
                a.chips = Some(next(i)?.parse().map_err(|e| format!("--chips: {e}"))?);
                i += 1;
            }
            "--sb" => {
                a.sb = next(i)?.parse().map_err(|e| format!("--sb: {e}"))?;
                i += 1;
            }
            "--bb" => {
                a.bb = next(i)?.parse().map_err(|e| format!("--bb: {e}"))?;
                i += 1;
            }
            "--hands" => {
                a.hands = next(i)?.parse().map_err(|e| format!("--hands: {e}"))?;
                i += 1;
            }
            "--seed" => {
                a.seed = Some(next(i)?.parse().map_err(|e| format!("--seed: {e}"))?);
                i += 1;
            }
            "--bot" => {
                a.bots.push(next(i)?);
                i += 1;
            }
            other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
        }
        i += 1;
    }
    Ok(a)
}

fn prompt_chips() -> u64 {
    loop {
        print!("Starting chips per bot [10000]: ");
        io::stdout().flush().ok();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return 10_000;
        }
        let t = line.trim();
        if t.is_empty() {
            return 10_000;
        }
        match t.parse::<u64>() {
            Ok(n) if n > 0 => return n,
            _ => println!("  please enter a positive whole number of chips"),
        }
    }
}

/// Seed from the clock when the user did not pin one, and always print it so
/// any match can be replayed exactly.
fn random_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5EED)
}

fn default_bots(root: &std::path::Path) -> Vec<BotSpec> {
    let py = root.join("bots/py_caller/bot.py");
    let cpp = root.join("bots/cpp_halfpot/halfpot");
    vec![
        BotSpec::new("cpp_halfpot", &[cpp.to_str().unwrap()]),
        BotSpec::new("py_caller", &["python3", py.to_str().unwrap()]),
    ]
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let specs: Vec<BotSpec> = if args.bots.is_empty() {
        default_bots(&root)
    } else {
        args.bots
            .iter()
            .enumerate()
            .map(|(i, cmd)| {
                let parts: Vec<&str> = cmd.split_whitespace().collect();
                let name = parts
                    .last()
                    .and_then(|p| p.rsplit('/').next())
                    .unwrap_or("bot")
                    .to_string();
                BotSpec::new(&format!("{name}#{i}"), &parts)
            })
            .collect()
    };

    for s in &specs {
        let prog = &s.argv[0];
        let exists = std::path::Path::new(prog).exists();
        let on_path = !prog.contains('/');
        if !exists && !on_path {
            eprintln!("error: bot executable not found: {prog}");
            if prog.ends_with("halfpot") {
                eprintln!("hint: build it with  make -C bots/cpp_halfpot");
            }
            std::process::exit(1);
        }
    }

    let chips = args.chips.unwrap_or_else(prompt_chips);
    let seed = args.seed.unwrap_or_else(random_seed);

    let cfg = MatchConfig {
        starting_stack: chips,
        small_blind: args.sb,
        big_blind: args.bb,
        ante: 0,
        max_hands: Some(args.hands),
        seed,
        action_timeout_ms: 20_000,
        time_bank_ms: Some(60_000),
        verbose: !args.quiet,
    };

    // Bot stderr is captured, never treated as a protocol violation.
    let run_dir = root.join("runs").join(format!("seed-{seed}"));
    if let Err(e) = std::fs::create_dir_all(&run_dir) {
        eprintln!("error: cannot create {}: {e}", run_dir.display());
        std::process::exit(1);
    }

    let mut agents: Vec<Box<dyn Seated>> = Vec::new();
    for spec in &specs {
        let errpath = run_dir.join(format!("{}.stderr", spec.name));
        match SubprocessAgent::spawn(spec, &errpath) {
            Ok(a) => agents.push(Box::new(a)),
            Err(e) => {
                eprintln!("error: failed to launch {}: {e}", spec.argv.join(" "));
                std::process::exit(1);
            }
        }
    }

    println!("=== agentic-holdem ===");
    println!(
        "seats {}   chips {}   blinds {}/{}   hands {}   seed {}",
        agents.len(),
        chips,
        cfg.small_blind,
        cfg.big_blind,
        args.hands,
        seed
    );
    println!("(replay this match exactly with --seed {seed})");

    let mut table = Table::new(cfg.clone(), agents);
    table.run();

    report(&table, &cfg);
    println!("\nbot stderr captured in {}", run_dir.display());
}

fn report(table: &Table, cfg: &MatchConfig) {
    println!("\n=== match report ===========================================");
    println!(
        "{:<16} {:>9} {:>9} {:>9} {:>7} {:>7} {:>6} {:>5} {:>5}",
        "agent", "chips", "net", "bb/100", "hands", "actions", "won", "viol", "norm"
    );

    let hands = table.hands_played.max(1) as f64;
    let mut rows: Vec<usize> = (0..table.n()).collect();
    rows.sort_by_key(|&i| std::cmp::Reverse(table.stack(i)));

    for i in rows {
        let st = &table.stats[i];
        let net = table.stack(i) as i64 - cfg.starting_stack as i64;
        let bb100 = (net as f64 / cfg.big_blind as f64) / hands * 100.0;
        println!(
            "{:<16} {:>9} {:>+9} {:>9.1} {:>7} {:>7} {:>6} {:>5} {:>5}",
            table.name(i),
            table.stack(i),
            net,
            bb100,
            st.hands_played,
            st.actions,
            st.hands_won,
            st.violations,
            st.normalizations,
        );
    }

    let total: u64 = (0..table.n()).map(|i| table.stack(i)).sum();
    let expected = cfg.starting_stack * table.n() as u64;
    println!(
        "\nhands played: {}   chip conservation: {} == {}  {}",
        table.hands_played,
        total,
        expected,
        if total == expected { "OK" } else { "FAILED" }
    );
}
