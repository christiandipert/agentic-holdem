//! A live viewer for the Agentic Poker Arena.
//!
//! This does not replay a recording: it runs a real match. The engine runs on a
//! worker thread and narrates itself through a bounded channel, so the GUI
//! controls the pace simply by deciding when to take the next event. Pausing is
//! just not receiving, which leaves the engine blocked on the channel rather
//! than racing ahead.

mod draw;
mod state;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Stroke, StrokeKind, Vec2,
};
use holdem_engine::{BotSpec, MatchConfig, MatchEvent, Seated, SubprocessAgent, Table};
use state::{GameState, LineKind};
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};

/// Where the example bots live. Resolved at compile time so the viewer works
/// regardless of the directory it is launched from.
fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

struct Args {
    chips: u64,
    sb: u64,
    bb: u64,
    hands: u32,
    seed: u64,
}

fn parse_args() -> Args {
    let mut a = Args { chips: 10_000, sb: 50, bb: 100, hands: 200, seed: 0 };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let val = |i: usize| -> Option<u64> { argv.get(i + 1).and_then(|v| v.parse().ok()) };
        match argv[i].as_str() {
            "--chips" => {
                if let Some(v) = val(i) {
                    a.chips = v;
                }
                i += 1;
            }
            "--sb" => {
                if let Some(v) = val(i) {
                    a.sb = v;
                }
                i += 1;
            }
            "--bb" => {
                if let Some(v) = val(i) {
                    a.bb = v;
                }
                i += 1;
            }
            "--hands" => {
                if let Some(v) = val(i) {
                    a.hands = v as u32;
                }
                i += 1;
            }
            "--seed" => {
                if let Some(v) = val(i) {
                    a.seed = v;
                }
                i += 1;
            }
            "-h" | "--help" => {
                println!(
                    "holdem-gui [--chips N] [--sb N] [--bb N] [--hands N] [--seed N]\n\
                     \n\
                     Runs a live match between the two example bots and draws it.\n\
                     Build the C++ bot first:  make -C bots/cpp_halfpot"
                );
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }
    if a.seed == 0 {
        use std::time::{SystemTime, UNIX_EPOCH};
        a.seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(99);
    }
    a
}

/// Start the match on a worker thread. The channel depth of 1 is what makes
/// pausing work: the engine blocks once the GUI stops taking events.
fn start_match(args: &Args) -> Result<Receiver<MatchEvent>, String> {
    let root = repo_root();
    let cpp = root.join("bots/cpp_halfpot/halfpot");
    let py = root.join("bots/py_caller/bot.py");

    if !cpp.exists() {
        return Err(format!(
            "C++ bot not built: {}\nRun:  make -C bots/cpp_halfpot",
            cpp.display()
        ));
    }
    if !py.exists() {
        return Err(format!("Python bot not found: {}", py.display()));
    }

    let run_dir = root.join("runs").join(format!("gui-seed-{}", args.seed));
    std::fs::create_dir_all(&run_dir).map_err(|e| format!("cannot create run dir: {e}"))?;

    let specs = [
        BotSpec::new("cpp_halfpot", &[cpp.to_str().unwrap()]),
        BotSpec::new("py_caller", &["python3", py.to_str().unwrap()]),
    ];

    let mut agents: Vec<Box<dyn Seated>> = Vec::new();
    for spec in &specs {
        let errpath = run_dir.join(format!("{}.stderr", spec.name));
        let agent = SubprocessAgent::spawn(spec, &errpath)
            .map_err(|e| format!("failed to launch {}: {e}", spec.argv.join(" ")))?;
        agents.push(Box::new(agent));
    }

    let cfg = MatchConfig {
        starting_stack: args.chips,
        small_blind: args.sb,
        big_blind: args.bb,
        ante: 0,
        max_hands: Some(args.hands),
        seed: args.seed,
        action_timeout_ms: 20_000,
        time_bank_ms: Some(60_000),
    };

    let (tx, rx) = sync_channel::<MatchEvent>(1);
    std::thread::Builder::new()
        .name("match".into())
        .spawn(move || {
            let observer = move |ev: &MatchEvent| {
                // A send error means the window closed; the match then unwinds
                // as the table is dropped.
                let _ = tx.send(ev.clone());
            };
            let mut table = Table::new(cfg, agents).with_observer(Box::new(observer));
            table.run();
        })
        .map_err(|e| format!("cannot start match thread: {e}"))?;

    Ok(rx)
}

struct App {
    rx: Option<Receiver<MatchEvent>>,
    state: GameState,
    error: Option<String>,
    playing: bool,
    /// Events consumed per second.
    speed: f32,
    accumulator: f32,
    /// Set by "step hand": consume until the current hand ends.
    run_to_hand_end: bool,
    channel_closed: bool,
}

impl App {
    fn new(args: &Args) -> Self {
        let (rx, error) = match start_match(args) {
            Ok(rx) => (Some(rx), None),
            Err(e) => (None, Some(e)),
        };
        App {
            rx,
            state: GameState::default(),
            error,
            playing: true,
            speed: 6.0,
            accumulator: 0.0,
            run_to_hand_end: false,
            channel_closed: false,
        }
    }

    /// Take one event if one is ready. Returns false when nothing was taken.
    fn pump_one(&mut self) -> bool {
        let Some(rx) = self.rx.as_ref() else {
            return false;
        };
        match rx.try_recv() {
            Ok(ev) => {
                if matches!(ev, MatchEvent::HandEnd { .. }) {
                    self.run_to_hand_end = false;
                }
                self.state.apply(&ev);
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.channel_closed = true;
                self.run_to_hand_end = false;
                false
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // --- advance the match
        if self.run_to_hand_end {
            // Bounded per frame so a stuck bot cannot freeze the window.
            for _ in 0..512 {
                if !self.run_to_hand_end || !self.pump_one() {
                    break;
                }
            }
        } else if self.playing {
            let dt = ctx.input(|i| i.stable_dt).min(0.25);
            self.accumulator += dt * self.speed;
            while self.accumulator >= 1.0 {
                self.accumulator -= 1.0;
                if !self.pump_one() {
                    break;
                }
            }
        }
        if self.playing || self.run_to_hand_end {
            ctx.request_repaint();
        }

        if let Some(err) = self.error.clone() {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Cannot start the match")
                            .size(20.0)
                            .color(Color32::from_rgb(235, 110, 110)),
                    );
                    ui.add_space(12.0);
                    ui.label(RichText::new(err).monospace());
                });
            });
            return;
        }

        self.top_bar(ctx);
        self.side_panel(ctx);
        self.table_view(ctx);
    }
}

impl App {
    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("controls").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Agentic Poker Arena").size(16.0).strong());
                ui.separator();

                let label = if self.playing { "\u{23F8} Pause" } else { "\u{25B6} Play" };
                if ui.button(label).clicked() {
                    self.playing = !self.playing;
                }
                if ui.button("\u{23ED} Step").clicked() {
                    self.playing = false;
                    self.pump_one();
                }
                if ui.button("Finish hand").clicked() {
                    self.playing = false;
                    self.run_to_hand_end = true;
                }
                ui.separator();
                ui.label("speed");
                ui.add(
                    egui::Slider::new(&mut self.speed, 0.5..=120.0)
                        .logarithmic(true)
                        .suffix(" ev/s"),
                );

                ui.separator();
                let hands = match self.state.max_hands {
                    Some(m) => format!("hand {} / {}", self.state.hand, m),
                    None => format!("hand {}", self.state.hand),
                };
                ui.label(RichText::new(hands).monospace());
                if let Some(st) = self.state.street {
                    ui.label(RichText::new(st.as_str()).monospace().color(draw::GOLD));
                }
                if self.state.finished {
                    ui.label(
                        RichText::new("MATCH OVER")
                            .strong()
                            .color(Color32::from_rgb(130, 215, 150)),
                    );
                } else if self.channel_closed {
                    ui.label(RichText::new("engine stopped").color(Color32::from_gray(140)));
                }
            });
            ui.add_space(4.0);
        });
    }

    fn side_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("feed")
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.label(RichText::new("Action feed").strong());
                ui.separator();
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .max_height(ui.available_height() * 0.62)
                    .show(ui, |ui| {
                        for line in &self.state.feed {
                            let indent = match line.kind {
                                LineKind::Info => "",
                                LineKind::Board | LineKind::Pot | LineKind::Blind => "  ",
                                _ => "    ",
                            };
                            ui.label(
                                RichText::new(format!("{indent}{}", line.text))
                                    .monospace()
                                    .size(11.5)
                                    .color(draw::line_color(line.kind)),
                            );
                        }
                    });

                ui.separator();
                ui.label(RichText::new("Chips over time").strong());
                let h = (ui.available_height() - 90.0).max(60.0);
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), h), egui::Sense::hover());
                draw::chip_chart(ui, rect, &self.state);

                ui.add_space(4.0);
                for (i, s) in self.state.seats.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let c = draw::SEAT_COLORS[i % draw::SEAT_COLORS.len()];
                        ui.label(RichText::new("\u{25CF}").color(c));
                        ui.label(RichText::new(&s.name).monospace().size(11.5));
                        ui.label(
                            RichText::new(format!("{}", s.stack))
                                .monospace()
                                .size(11.5)
                                .strong(),
                        );
                        if s.violations > 0 {
                            ui.label(
                                RichText::new(format!("{} viol", s.violations))
                                    .size(11.0)
                                    .color(Color32::from_rgb(235, 110, 110)),
                            );
                        }
                    });
                }
            });
    }

    fn table_view(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let full = ui.available_rect_before_wrap();
            let painter = ui.painter();

            // Felt
            let felt = full.shrink(10.0);
            painter.rect_filled(felt, CornerRadius::same(28), draw::FELT);
            painter.rect_stroke(
                felt,
                CornerRadius::same(28),
                Stroke::new(3.0, draw::FELT_EDGE),
                StrokeKind::Inside,
            );

            let card_h = (felt.height() * 0.17).clamp(44.0, 92.0);

            // Board and pot in the middle
            let center = felt.center();
            if self.state.board.is_empty() {
                painter.text(
                    center,
                    Align2::CENTER_CENTER,
                    "preflop",
                    FontId::proportional(15.0),
                    Color32::from_rgb(120, 160, 140),
                );
            } else {
                draw::card_row(ui, center, &self.state.board, card_h, 8.0);
            }

            let pot_pos = Pos2::new(center.x, center.y - card_h * 0.75 - 18.0);
            painter.text(
                pot_pos,
                Align2::CENTER_CENTER,
                format!("pot  {}", self.state.pot),
                FontId::proportional(17.0),
                draw::GOLD,
            );

            // Seats: first at the bottom, second at the top.
            let n = self.state.seats.len().max(1);
            for i in 0..self.state.seats.len() {
                let at_bottom = i == 0 || (n > 2 && i <= n / 2);
                let y = if at_bottom {
                    felt.bottom() - card_h * 0.9 - 54.0
                } else {
                    felt.top() + card_h * 0.9 + 26.0
                };
                let spread = if n <= 2 { 1 } else { n };
                let slot = if n <= 2 { 0.5 } else { (i as f32 + 0.5) / spread as f32 };
                let x = if n <= 2 { felt.center().x } else { felt.left() + felt.width() * slot };
                self.draw_seat(ui, i, Pos2::new(x, y), card_h);
            }
        });
    }

    fn draw_seat(&self, ui: &egui::Ui, seat: usize, anchor: Pos2, card_h: f32) {
        let s = &self.state.seats[seat];
        let color = draw::SEAT_COLORS[seat % draw::SEAT_COLORS.len()];

        // Cards. Shown face up while in the hand, face down once folded,
        // because a folded hand is mucked.
        match s.hole {
            Some(hole) if !s.folded => draw::card_row(ui, anchor, &hole, card_h, 6.0),
            Some(_) => {
                let size = draw::card_size(card_h * 0.86);
                for k in 0..2 {
                    let x = anchor.x - size.x - 3.0 + k as f32 * (size.x + 6.0);
                    draw::card_back(
                        ui,
                        Rect::from_min_size(Pos2::new(x, anchor.y - size.y / 2.0), size),
                    );
                }
            }
            None => {}
        }

        let p = ui.painter();
        // Clear the acting highlight (half of card_h * 1.22) as well as the
        // cards, or the name collides with both at large card sizes.
        let below = anchor.y + card_h * 0.61 + 18.0;

        // Name plate, highlighted while this seat is deciding.
        let mut title = s.name.clone();
        if self.state.button as usize == seat {
            title.push_str("  (BTN)");
        }
        p.text(
            Pos2::new(anchor.x, below),
            Align2::CENTER_CENTER,
            title,
            FontId::proportional(14.0),
            if s.acting { draw::GOLD } else { color },
        );

        let mut status = format!("{}", s.stack);
        if s.all_in {
            status.push_str("   ALL-IN");
        } else if s.folded {
            status.push_str("   folded");
        }
        p.text(
            Pos2::new(anchor.x, below + 17.0),
            Align2::CENTER_CENTER,
            status,
            FontId::monospace(13.0),
            if s.folded { Color32::from_gray(120) } else { Color32::from_gray(225) },
        );

        if s.street_bet > 0 {
            // Push the wager towards the middle of the table: seats on the
            // bottom show it above their cards, seats on top below.
            let table_mid = ui.available_rect_before_wrap().center().y;
            let towards_centre = if anchor.y > table_mid { -1.0 } else { 1.0 };
            p.text(
                Pos2::new(anchor.x, anchor.y + towards_centre * (card_h * 0.5 + 34.0)),
                Align2::CENTER_CENTER,
                format!("bet {}", s.street_bet),
                FontId::monospace(13.0),
                draw::GOLD,
            );
        }

        if s.acting {
            let r = Rect::from_center_size(
                Pos2::new(anchor.x, anchor.y),
                Vec2::new(card_h * 1.75, card_h * 1.22),
            );
            p.rect_stroke(
                r,
                CornerRadius::same(8),
                Stroke::new(2.0, draw::GOLD),
                StrokeKind::Outside,
            );
        }
    }
}

fn main() -> eframe::Result<()> {
    let args = parse_args();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([820.0, 560.0])
            .with_title("Agentic Poker Arena"),
        ..Default::default()
    };
    eframe::run_native(
        "Agentic Poker Arena",
        options,
        Box::new(move |_cc| Ok(Box::new(App::new(&args)))),
    )
}
