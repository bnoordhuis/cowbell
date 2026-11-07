//! Rotary knobs, drawn by the rack view and the synth panel. Continuous
//! params get a knurled amp-style knob with a ring of LEDs for the value;
//! switches (params with labels) get a chicken-head pointer with a label at
//! each stop.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Shape, Stroke, Vec2, vec2};

use super::color;
use crate::rack::{Curve, ParamSpec};

/// Radius of a knob's body.
pub const RADIUS: f32 = 20.0;
/// How close the pointer must be to a knob's center to grab it: the body
/// and its LED ring.
pub const HIT: f32 = RADIUS + 8.0;
/// Pointer travel for the whole range, in points; shift is finer.
pub const DRAG_RANGE: f32 = 150.0;
pub const FINE_DRAG_RANGE: f32 = 600.0;
/// Wheel travel for the whole range, in points; shift is finer.
const WHEEL_RANGE: f32 = 1000.0;
const FINE_WHEEL_RANGE: f32 = 4000.0;
/// Wheel travel per step of a stepped knob, like a switch: about a notch.
const WHEEL_NOTCH: f32 = 50.0;
/// Seconds without scrolling that end a wheel gesture. Each gesture is
/// one undo step.
const WHEEL_PAUSE: f64 = 0.6;
/// Narrowest cell a knob is laid out in.
const CELL: f32 = 76.0;

const LEDS: usize = 15;
const LED_RING: f32 = RADIUS + 8.0;
const LED_RADIUS: f32 = 2.2;
const LED_OFF: Color32 = Color32::from_rgb(0x3a, 0x3b, 0x40);
const HOLE: Color32 = Color32::from_rgb(0x0e, 0x0e, 0x10);
const BODY: Color32 = Color32::from_rgb(0x1f, 0x20, 0x24);
const CAP: Color32 = Color32::from_rgb(0x25, 0x26, 0x2a);
const CAP_EDGE: Color32 = Color32::from_rgb(0x30, 0x31, 0x36);
const CREAM: Color32 = Color32::from_rgb(0xe8, 0xe2, 0xd0);
const CREAM_DARK: Color32 = Color32::from_rgb(0xbd, 0xb6, 0xa2);
const POINTER_LINE: Color32 = Color32::from_rgb(0x2c, 0x2c, 0x2a);

/// Switch body radius; the pointer reaches past it.
const SWITCH_RADIUS: f32 = 15.0;
const SWITCH_POINTER: f32 = 27.0;
const STOP_LABEL: f32 = SWITCH_POINTER + 4.0;

/// This frame's mouse wheel motion, in points, up positive. Read from the
/// raw events: egui turns shift+wheel into sideways scrolling, and shift
/// is fine control here.
pub fn wheel_delta(input: &egui::InputState) -> f32 {
    input
        .raw
        .events
        .iter()
        .map(|e| match e {
            egui::Event::MouseWheel { unit, delta, .. } => {
                let d = delta.y + delta.x;
                match unit {
                    egui::MouseWheelUnit::Point => d,
                    egui::MouseWheelUnit::Line => d * WHEEL_NOTCH,
                    egui::MouseWheelUnit::Page => d * WHEEL_NOTCH * 10.0,
                }
            }
            _ => 0.0,
        })
        .sum()
}

/// Turning knobs with the mouse wheel. `K` names a knob, so a gesture
/// that moves to another knob starts over.
pub struct Wheel<K> {
    last: Option<(K, f64)>,
    /// Scrolling not yet turned into steps, for stepped knobs.
    acc: f32,
    /// The gesture hasn't changed anything yet, so its first change
    /// should start an undo step.
    fresh: bool,
}

impl<K> Default for Wheel<K> {
    fn default() -> Self {
        Wheel {
            last: None,
            acc: 0.0,
            fresh: true,
        }
    }
}

impl<K: Copy + PartialEq> Wheel<K> {
    /// Scroll `delta` points over knob `key` (up is positive) at time
    /// `now`. Returns the new value, and whether it's the gesture's first
    /// change, if the knob moves.
    pub fn turn(
        &mut self,
        key: K,
        spec: &ParamSpec,
        value: f32,
        delta: f32,
        fine: bool,
        now: f64,
    ) -> Option<(f32, bool)> {
        if delta == 0.0 {
            return None;
        }
        let same = matches!(self.last, Some((k, t)) if k == key && now - t < WHEEL_PAUSE);
        if !same {
            self.acc = 0.0;
            self.fresh = true;
        }
        self.last = Some((key, now));
        self.acc += delta;
        let v = if spec.curve == Curve::Steps {
            let steps = (self.acc / WHEEL_NOTCH).trunc();
            self.acc -= steps * WHEEL_NOTCH;
            spec.clamp(value.round() + steps)
        } else {
            let range = if fine { FINE_WHEEL_RANGE } else { WHEEL_RANGE };
            let k = spec.to_knob(value) + self.acc / range;
            self.acc = 0.0;
            spec.from_knob(k)
        };
        if v == value {
            return None;
        }
        let first = std::mem::replace(&mut self.fresh, false);
        Some((v, first))
    }
}

/// Direction for knob position `k` (0..=1): a 270 degree sweep from
/// bottom-left to bottom-right, clockwise.
fn dir(k: f32) -> Vec2 {
    let a = (135.0 + 270.0 * k).to_radians();
    vec2(a.cos(), a.sin())
}

/// Knob positions of a switch's stops, spread over the top of the dial.
fn stops(n: usize) -> impl Iterator<Item = f32> {
    let span = (0.3 * (n as f32 - 1.0)).min(0.6);
    (0..n).map(move |i| {
        if n < 2 {
            0.5
        } else {
            0.5 - span / 2.0 + span * i as f32 / (n - 1) as f32
        }
    })
}

/// Where a stop's label goes, and how it's anchored there.
fn stop_label(c: Pos2, k: f32) -> (Pos2, Align2) {
    let d = dir(k);
    let align = if d.x < -0.3 {
        Align2::RIGHT_CENTER
    } else if d.x > 0.3 {
        Align2::LEFT_CENTER
    } else {
        Align2::CENTER_BOTTOM
    };
    (c + d * STOP_LABEL, align)
}

/// Width of the cell a knob needs, so switch labels don't run into the
/// neighbours. `char_w` is the label font's glyph width.
pub fn width(spec: &ParamSpec, char_w: f32) -> f32 {
    let text = |s: &str| s.chars().count() as f32 * char_w;
    let mut half = (text(spec.name) / 2.0).max(LED_RING + LED_RADIUS);
    let stop_labels = if spec.is_switch() { spec.labels } else { &[] };
    for (k, label) in stops(stop_labels.len()).zip(stop_labels) {
        let (p, align) = stop_label(Pos2::ZERO, k);
        let reach = match align {
            Align2::CENTER_BOTTOM => p.x.abs() + text(label) / 2.0,
            _ => p.x.abs() + text(label),
        };
        half = half.max(reach);
    }
    (2.0 * half + 12.0).max(CELL)
}

/// Draw a knob at `c` showing `value`, with its name (and value) below.
/// `automated` marks the knob the cursor's track turns with `K` effects.
pub fn draw(
    p: &egui::Painter,
    c: Pos2,
    spec: &ParamSpec,
    value: f32,
    font: &FontId,
    automated: bool,
) {
    if !spec.is_switch() {
        draw_knob(p, c, spec, value, font);
    } else {
        draw_switch(p, c, spec, value, font);
    }
    if automated {
        p.circle_stroke(c, RADIUS + 3.5, Stroke::new(1.5, color::ACCENT));
        p.text(
            c + vec2(LED_RING, -LED_RING),
            Align2::CENTER_CENTER,
            "K",
            font.clone(),
            color::ACCENT,
        );
    }
}

fn draw_knob(p: &egui::Painter, c: Pos2, spec: &ParamSpec, value: f32, font: &FontId) {
    let k = spec.to_knob(value);
    // Bipolar params light up from their zero, the rest from the bottom.
    let origin = if spec.min < 0.0 && spec.max > 0.0 {
        spec.to_knob(0.0)
    } else {
        0.0
    };
    let (lo, hi) = (k.min(origin), k.max(origin));
    let step = 1.0 / (LEDS - 1) as f32;
    for i in 0..LEDS {
        let at = i as f32 * step;
        let lit = at >= lo - step / 2.0 && at <= hi + step / 2.0;
        let fill = if lit { color::ACCENT } else { LED_OFF };
        p.circle_filled(c + dir(at) * LED_RING, LED_RADIUS, fill);
    }
    // Knurled edge: bumps around a dark rim.
    p.circle_filled(c, RADIUS + 1.0, HOLE);
    for i in 0..28 {
        let a = i as f32 / 28.0 * std::f32::consts::TAU;
        p.circle_filled(c + vec2(a.cos(), a.sin()) * (RADIUS + 1.0), 2.2, HOLE);
    }
    p.circle_filled(c, RADIUS - 1.0, BODY);
    p.circle(c, RADIUS * 0.7, CAP, Stroke::new(2.0, CAP_EDGE));
    p.line_segment(
        [c + dir(k) * 5.0, c + dir(k) * (RADIUS - 3.0)],
        Stroke::new(3.0, CREAM),
    );
    labels(p, c, spec.name, Some(spec.format(value)), font);
}

fn draw_switch(p: &egui::Painter, c: Pos2, spec: &ParamSpec, value: f32, font: &FontId) {
    let choice = value.round() as usize;
    let mut pointer = 0.5;
    for (i, (k, label)) in stops(spec.labels.len()).zip(spec.labels).enumerate() {
        let on = i == choice;
        if on {
            pointer = k;
        }
        let tint = if on { color::ACCENT } else { color::MUTED };
        p.line_segment(
            [
                c + dir(k) * (SWITCH_RADIUS + 3.0),
                c + dir(k) * (SWITCH_RADIUS + 7.0),
            ],
            Stroke::new(2.0, tint),
        );
        let (at, align) = stop_label(c, k);
        p.text(at, align, *label, font.clone(), tint);
    }
    p.circle_filled(c, SWITCH_RADIUS + 2.0, HOLE);
    p.circle_filled(c, SWITCH_RADIUS, BODY);
    // Chicken head: a round base with a tapered pointer.
    let d = dir(pointer);
    let side = vec2(-d.y, d.x);
    p.circle_filled(c, 11.0, CREAM);
    p.add(Shape::convex_polygon(
        vec![
            c + d * SWITCH_POINTER,
            c + d * 4.0 + side * 8.5,
            c + d * 4.0 - side * 8.5,
        ],
        CREAM,
        Stroke::NONE,
    ));
    p.line_segment(
        [c + d * 7.0, c + d * (SWITCH_POINTER - 4.0)],
        Stroke::new(2.5, POINTER_LINE),
    );
    p.circle_filled(c, 3.5, CREAM_DARK);
    // The lit stop shows the value, so just the name below.
    labels(p, c, spec.name, None, font);
}

fn labels(p: &egui::Painter, c: Pos2, name: &str, value: Option<String>, font: &FontId) {
    let y = LED_RING + LED_RADIUS + 1.0;
    p.text(
        c + vec2(0.0, y),
        Align2::CENTER_TOP,
        name,
        font.clone(),
        color::MUTED,
    );
    if let Some(v) = value {
        p.text(
            c + vec2(0.0, y + 13.0),
            Align2::CENTER_TOP,
            v,
            font.clone(),
            color::TEXT,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_spread_over_the_top() {
        let near = |a: Vec<f32>, b: &[f32]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6);
        assert!(near(stops(3).collect(), &[0.2, 0.5, 0.8]));
        assert!(near(stops(2).collect(), &[0.35, 0.65]));
    }

    #[test]
    fn wheel_turns_knobs_and_steps_switches() {
        let plain = ParamSpec::new("mix", 0.0, 1.0, 0.5, Curve::Linear, "");
        let mut w = Wheel::default();
        let (v, first) = w.turn(1, &plain, 0.5, 50.0, false, 0.0).unwrap();
        assert!(first && (v - 0.55).abs() < 1e-4, "{v}");
        let (v, first) = w.turn(1, &plain, v, -100.0, false, 0.1).unwrap();
        assert!(!first && (v - 0.45).abs() < 1e-4, "{v}");
        // Shift is finer.
        let (v, _) = w.turn(1, &plain, 0.5, 40.0, true, 0.2).unwrap();
        assert!((v - 0.51).abs() < 1e-4, "{v}");
        // A pause, or another knob, starts a new undo step.
        assert!(w.turn(1, &plain, 0.5, 50.0, false, 5.0).unwrap().1);
        assert!(w.turn(2, &plain, 0.5, 50.0, false, 5.1).unwrap().1);

        // Switches step a notch at a time, and stop at the ends.
        let switch = ParamSpec::labelled("shape", 0.0, &["sine", "square", "s&h"]);
        let mut w = Wheel::default();
        assert_eq!(w.turn(0, &switch, 0.0, 30.0, false, 0.0), None);
        assert_eq!(
            w.turn(0, &switch, 0.0, 30.0, false, 0.05),
            Some((1.0, true))
        );
        assert_eq!(
            w.turn(0, &switch, 1.0, 50.0, false, 0.1),
            Some((2.0, false))
        );
        assert_eq!(w.turn(0, &switch, 2.0, 50.0, false, 0.15), None);
        assert_eq!(
            w.turn(0, &switch, 2.0, -60.0, false, 0.2),
            Some((1.0, false))
        );
    }

    #[test]
    fn switches_get_room_for_their_labels() {
        let plain = ParamSpec::new("cutoff", 40.0, 8000.0, 400.0, Curve::Exp, "Hz");
        let switch = ParamSpec::labelled("shape", 0.0, &["sine", "square", "s&h"]);
        assert_eq!(width(&plain, 7.0), CELL);
        let w = width(&switch, 7.0);
        // "sine" sits left of the dial, right-aligned at the label radius.
        assert!(w / 2.0 >= STOP_LABEL * dir(0.2).x.abs() + 4.0 * 7.0, "{w}");
    }
}
