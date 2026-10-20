//! Binding 2: a bot as a child process speaking JSON lines.
//!
//! No deadline is enforced in this build (see `PLAN.md` §4 for the design that
//! will): reads block. A bot that *exits* is handled cleanly, because that is
//! both the common failure and free to detect — the pipe closes and the read
//! returns EOF. A bot that hangs forever will wedge the match until Ctrl-C.

use crate::protocol::{self, ReplyError};
use crate::seated::{Fault, Seated};
use holdem_agent::{Action, HandResult, HandStart, MatchContext, PlayerView};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// How to launch a bot.
#[derive(Clone, Debug)]
pub struct BotSpec {
    /// Name used in reports until the bot tells us its own in `ready`.
    pub name: String,
    /// Program and arguments, e.g. `["python3", "bots/py_caller/bot.py"]`.
    pub argv: Vec<String>,
    pub cwd: Option<String>,
}

impl BotSpec {
    pub fn new(name: &str, argv: &[&str]) -> Self {
        BotSpec {
            name: name.to_string(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            cwd: None,
        }
    }
}

pub struct SubprocessAgent {
    name: String,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<BufReader<ChildStdout>>,
    next_id: u64,
    /// Set once the bot becomes unreachable, so we stop attempting I/O.
    dead: Option<String>,
}

impl SubprocessAgent {
    /// Spawn the bot. `stderr_path` captures the bot's own logging; writing to
    /// stderr is never a protocol violation.
    pub fn spawn(spec: &BotSpec, stderr_path: &Path) -> std::io::Result<Self> {
        let (program, args) = spec
            .argv
            .split_first()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty argv"))?;

        let errlog = std::fs::File::create(stderr_path)?;
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(errlog));
        if let Some(cwd) = &spec.cwd {
            cmd.current_dir(cwd);
        }

        // This is where `pre_exec` will pin the child to its core once
        // Addendum B lands; affinity is inherited, so it covers anything the
        // bot itself spawns.
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));

        Ok(SubprocessAgent {
            name: spec.name.clone(),
            child: Some(child),
            stdin: Some(stdin),
            stdout: Some(stdout),
            next_id: 1,
            dead: None,
        })
    }

    fn send(&mut self, msg: &Value) -> Result<(), Fault> {
        if let Some(why) = &self.dead {
            return Err(Fault::Dead(why.clone()));
        }
        let stdin = self.stdin.as_mut().ok_or_else(|| Fault::Dead("no stdin".into()))?;
        let line = format!("{msg}\n");
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| {
                let why = format!("write failed ({e})");
                self.dead = Some(why.clone());
                Fault::Dead(why)
            })
    }

    /// Read one reply line. EOF means the bot exited.
    fn recv(&mut self) -> Result<String, Fault> {
        if let Some(why) = &self.dead {
            return Err(Fault::Dead(why.clone()));
        }
        let stdout = self.stdout.as_mut().ok_or_else(|| Fault::Dead("no stdout".into()))?;
        let mut line = String::new();
        match stdout.read_line(&mut line) {
            Ok(0) => {
                let why = "process closed its stdout (exited?)".to_string();
                self.dead = Some(why.clone());
                Err(Fault::Dead(why))
            }
            Ok(_) => Ok(line),
            Err(e) => {
                let why = format!("read failed ({e})");
                self.dead = Some(why.clone());
                Err(Fault::Dead(why))
            }
        }
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Messages that expect no reply must not be allowed to kill a match just
    /// because the bot has gone away; the next `on_action` will report it.
    fn notify(&mut self, msg: &Value) -> Result<(), Fault> {
        match self.send(msg) {
            Ok(()) | Err(Fault::Dead(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn is_dead(&self) -> bool {
        self.dead.is_some()
    }
}

impl Seated for SubprocessAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn entry(&mut self, ctx: &MatchContext) -> Result<(), Fault> {
        let id = self.take_id();
        self.send(&protocol::entry_msg(id, ctx))?;
        let line = self.recv()?;
        match protocol::parse_ready(&line, id) {
            Ok(Some(reported)) => {
                // The bot's own name wins, so reports show what it calls itself.
                self.name = reported;
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(e) => Err(Fault::Protocol(e)),
        }
    }

    fn on_action(&mut self, view: &PlayerView<'_>) -> Result<Action, Fault> {
        let id = self.take_id();
        self.send(&protocol::action_request_msg(id, view))?;
        let line = self.recv()?;
        protocol::parse_action(&line, id).map_err(|e| match e {
            // A reply we cannot line up with our request leaves the stream
            // ambiguous, but the bot is still alive, so penalise and continue.
            other @ ReplyError::IdMismatch { .. } => Fault::Protocol(other),
            other => Fault::Protocol(other),
        })
    }

    fn on_hand_start(&mut self, start: &HandStart<'_>) -> Result<(), Fault> {
        let msg = protocol::hand_start_msg(start);
        self.notify(&msg)
    }

    fn on_hand_end(&mut self, result: &HandResult) -> Result<(), Fault> {
        self.notify(&protocol::hand_end_msg(result))
    }

    fn shutdown(&mut self) {
        let _ = self.notify(&protocol::shutdown_msg());
        // Close stdin so a bot blocked reading stdin sees EOF and can exit.
        self.stdin = None;
        if let Some(child) = self.child.as_mut() {
            let _ = child.wait();
        }
    }
}

impl Drop for SubprocessAgent {
    fn drop(&mut self) {
        self.stdin = None;
        if let Some(child) = self.child.as_mut() {
            // Do not leave orphans behind if a match ends abnormally.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
