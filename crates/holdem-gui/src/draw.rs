//! All painting. Everything is drawn with the painter rather than pulled from
//! image assets, so the binary stays self-contained.

use crate::state::{GameState, LineKind};
use eframe::egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2,
};
use holdem_agent::Card;

pub const FELT: Color32 = Color32::from_rgb(22, 74, 54);
pub const FELT_EDGE: Color32 = Color32::from_rgb(12, 44, 32);
pub const CARD_BG: Color32 = Color32::from_rgb(248, 248, 245);
pub const CARD_BACK: Color32 = Color32::from_rgb(70, 90, 140);
pub const RED_SUIT: Color32 = Color32::from_rgb(190, 40, 40);
pub const BLACK_SUIT: Color32 = Color32::from_rgb(30, 30, 34);
pub const GOLD: Color32 = Color32::from_rgb(226, 182, 86);

/// Distinct, colour-blind-friendly-ish seat colours.
pub const SEAT_COLORS: [Color32; 6] = [
    Color32::from_rgb(86, 156, 214),
    Color32::from_rgb(226, 135, 67),
    Color32::from_rgb(120, 196, 120),
    Color32::from_rgb(198, 120, 200),
    Color32::from_rgb(224, 196, 90),
    Color32::from_rgb(140, 140, 150),
];

pub fn line_color(kind: LineKind) -> Color32 {
    match kind {
        LineKind::Info => Color32::from_gray(150),
        LineKind::Action => Color32::from_gray(225),
        LineKind::Blind => Color32::from_rgb(150, 190, 220),
        LineKind::Board => GOLD,
        LineKind::Pot => Color32::from_rgb(130, 215, 150),
        LineKind::Bad => Color32::from_rgb(235, 110, 110),
    }
}

/// Draw one card face up.
pub fn card(ui: &Ui, rect: Rect, c: Card) {
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(4), CARD_BG);
    p.rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(1.0, Color32::from_gray(90)),
        StrokeKind::Inside,
    );

    let suit = c.suit();
    // Suits 1 (hearts) and 2 (diamonds) are red.
    let color = if suit == 1 || suit == 2 { RED_SUIT } else { BLACK_SUIT };
    let rank = Card::RANKS[c.rank() as usize];
    let glyph = ['\u{2660}', '\u{2665}', '\u{2666}', '\u{2663}'][suit as usize];

    let h = rect.height();
    p.text(
        rect.center() - Vec2::new(0.0, h * 0.13),
        Align2::CENTER_CENTER,
        rank.to_string(),
        FontId::proportional(h * 0.42),
        color,
    );
    p.text(
        rect.center() + Vec2::new(0.0, h * 0.24),
        Align2::CENTER_CENTER,
        glyph.to_string(),
        FontId::proportional(h * 0.30),
        color,
    );
}

/// Draw a face-down card, for a seat whose cards are not being shown.
pub fn card_back(ui: &Ui, rect: Rect) {
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(4), CARD_BACK);
    p.rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(1.0, Color32::from_gray(40)),
        StrokeKind::Inside,
    );
}

pub fn card_size(height: f32) -> Vec2 {
    Vec2::new(height * 0.70, height)
}

/// Lay out a row of cards centred on `center`.
pub fn card_row(ui: &Ui, center: Pos2, cards: &[Card], height: f32, gap: f32) {
    let size = card_size(height);
    let total = cards.len() as f32 * size.x + (cards.len().max(1) - 1) as f32 * gap;
    let mut x = center.x - total / 2.0;
    for &c in cards {
        let r = Rect::from_min_size(Pos2::new(x, center.y - size.y / 2.0), size);
        card(ui, r, c);
        x += size.x + gap;
    }
}

/// The chip-stack chart: one polyline per seat over completed hands.
pub fn chip_chart(ui: &Ui, rect: Rect, state: &GameState) {
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(4), Color32::from_gray(24));

    let hands = state.chip_history.len();

    // Scale to the range actually occupied, not to the total chips in play:
    // against a fixed 0..total axis every line sits near the middle and real
    // swings are invisible. Always include the starting stack so "even" stays
    // on the chart, and pad so lines never touch the edges.
    let (mut lo, mut hi) = (state.starting_stack as f32, state.starting_stack as f32);
    for stacks in &state.chip_history {
        for &v in stacks {
            lo = lo.min(v as f32);
            hi = hi.max(v as f32);
        }
    }
    let pad = ((hi - lo) * 0.08).max(1.0);
    let (lo, hi) = (lo - pad, hi + pad);
    let span = (hi - lo).max(1.0);
    let y_of = |v: f32| rect.bottom() - rect.height() * ((v - lo) / span);

    // Axis: the starting-stack line, so "even" is visible at a glance.
    let even_y = y_of(state.starting_stack as f32);
    p.line_segment(
        [Pos2::new(rect.left(), even_y), Pos2::new(rect.right(), even_y)],
        Stroke::new(1.0, Color32::from_gray(60)),
    );

    if hands < 2 {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "chip history appears as hands complete",
            FontId::proportional(11.0),
            Color32::from_gray(90),
        );
        return;
    }

    let dx = rect.width() / (hands - 1) as f32;
    for seat in 0..state.seats.len() {
        let points: Vec<Pos2> = state
            .chip_history
            .iter()
            .enumerate()
            .map(|(i, stacks)| Pos2::new(rect.left() + i as f32 * dx, y_of(stacks[seat] as f32)))
            .collect();
        p.line(points, Stroke::new(1.8, SEAT_COLORS[seat % SEAT_COLORS.len()]));
    }
}
