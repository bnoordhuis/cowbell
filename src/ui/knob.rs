//! A rotary knob: drawn by the rack view and the synth panel.

use eframe::egui::{self, Align2, FontId, Pos2, Stroke, vec2};

use super::color;
use crate::rack::ParamSpec;

pub const RADIUS: f32 = 15.0;
/// How close the pointer must be to a knob's center to grab it.
pub const HIT: f32 = RADIUS + 3.0;
/// Pointer travel for the whole range, in points; shift is finer.
pub const DRAG_RANGE: f32 = 150.0;
pub const FINE_DRAG_RANGE: f32 = 600.0;

const EDGE: egui::Color32 = egui::Color32::from_rgb(0x3a, 0x3b, 0x40);

/// Draw a knob at `c` showing `value`, with its name and value below.
/// `automated` marks the knob the cursor's track turns with `K` effects.
pub fn draw(
    p: &egui::Painter,
    c: Pos2,
    spec: &ParamSpec,
    value: f32,
    font: &FontId,
    automated: bool,
) {
    let k = spec.to_knob(value);
    p.circle_filled(c, RADIUS, color::BG);
    let edge = if automated {
        Stroke::new(2.0, color::ACCENT)
    } else {
        Stroke::new(1.0, EDGE)
    };
    p.circle_stroke(c, RADIUS, edge);
    if automated {
        p.text(
            c + vec2(RADIUS, -RADIUS),
            Align2::CENTER_CENTER,
            "K",
            font.clone(),
            color::ACCENT,
        );
    }
    // 270 degree sweep, from bottom-left to bottom-right.
    let angle = |k: f32| (135.0 + 270.0 * k).to_radians();
    let point = |k: f32, r: f32| c + vec2(angle(k).cos(), angle(k).sin()) * r;
    if k > 0.0 {
        let arc: Vec<Pos2> = (0..=24)
            .map(|s| point(k * s as f32 / 24.0, RADIUS - 3.0))
            .collect();
        p.add(egui::Shape::line(arc, Stroke::new(2.5, color::ACCENT)));
    }
    p.line_segment([c, point(k, RADIUS - 2.0)], Stroke::new(2.0, color::TEXT));
    p.text(
        c + vec2(0.0, RADIUS + 3.0),
        Align2::CENTER_TOP,
        spec.name,
        font.clone(),
        color::MUTED,
    );
    p.text(
        c + vec2(0.0, RADIUS + 16.0),
        Align2::CENTER_TOP,
        spec.format(value),
        font.clone(),
        color::TEXT,
    );
}
