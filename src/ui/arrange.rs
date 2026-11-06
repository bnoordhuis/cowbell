//! The arrangement: one lane per track, its clips laid out on a timeline,
//! with a bar ruler on top and the track headers on the left.
//!
//! Click the ruler to move the playhead; drag along it to set the loop.
//! Click a clip or a lane to select it, drag there to move the view around.
//! Scroll to scroll the tracks, ctrl+scroll to zoom.

use std::ops::Range;

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2,
};

use super::color;
use crate::song::{Clip, Instrument, Song, TICKS_PER_BAR, TICKS_PER_BEAT, Tick};

pub const HEADER_W: f32 = 280.0;
pub const RULER_H: f32 = 32.0;
pub const LANE_H: f32 = 128.0;
const MIN_PX_PER_BEAT: f32 = 4.0;
const MAX_PX_PER_BEAT: f32 = 400.0;
/// Bar numbers closer than this skip some bars.
const LABEL_GAP: f32 = 44.0;

pub struct ArrangeView {
    /// Leftmost tick in view.
    pub scroll: f64,
    /// Points the lanes are scrolled up by.
    pub scroll_y: f32,
    pub px_per_beat: f32,
    pub selected_track: usize,
    /// (track, clip index).
    pub selected_clip: Option<(usize, usize)>,
    /// A press on the ruler: the beat it started on, where on screen, and
    /// whether it has turned into a drag yet.
    ruler_press: Option<(Tick, f32, bool)>,
    /// A press in the lanes, dragging the view: where it started, and the
    /// scroll offsets then.
    pan: Option<(Pos2, f64, f32)>,
}

impl Default for ArrangeView {
    fn default() -> Self {
        ArrangeView {
            scroll: 0.0,
            scroll_y: 0.0,
            px_per_beat: 24.0,
            selected_track: 0,
            selected_clip: None,
            ruler_press: None,
            pan: None,
        }
    }
}

/// What happened in the arrangement this frame.
#[derive(Default, Debug, PartialEq)]
pub struct Output {
    /// Move the playhead here.
    pub locate: Option<Tick>,
    /// Set the loop range to this.
    pub loop_range: Option<Range<Tick>>,
    /// Flip this track's mute.
    pub mute: Option<usize>,
    /// Flip this track's solo.
    pub solo: Option<usize>,
}

/// Where everything is, worked out before drawing so hit testing uses the
/// same numbers.
struct Geometry {
    ruler: Rect,
    headers: Rect,
    lanes: Rect,
    scroll: f64,
    scroll_y: f32,
    px_per_tick: f64,
}

#[derive(Debug, PartialEq)]
enum Hit {
    Ruler(f64),
    Clip(usize, usize),
    Lane(usize),
    Mute(usize),
    Solo(usize),
    Nothing,
}

/// A track header's mute (0) or solo (1) button.
fn header_button(header: Rect, which: usize) -> Rect {
    Rect::from_min_size(
        pos2(
            header.right() - 88.0 + which as f32 * 40.0,
            header.top() + 18.0,
        ),
        vec2(32.0, 28.0),
    )
}

impl Geometry {
    fn new(view: &ArrangeView, area: Rect) -> Self {
        let ruler = Rect::from_min_max(
            pos2(area.left() + HEADER_W, area.top()),
            pos2(area.right(), area.top() + RULER_H),
        );
        let headers = Rect::from_min_max(
            pos2(area.left(), area.top() + RULER_H),
            pos2(area.left() + HEADER_W, area.bottom()),
        );
        let lanes = Rect::from_min_max(pos2(ruler.left(), ruler.bottom()), area.right_bottom());
        Geometry {
            ruler,
            headers,
            lanes,
            scroll: view.scroll,
            scroll_y: view.scroll_y,
            px_per_tick: view.px_per_beat as f64 / TICKS_PER_BEAT as f64,
        }
    }

    fn x(&self, tick: f64) -> f32 {
        self.lanes.left() + ((tick - self.scroll) * self.px_per_tick) as f32
    }

    fn tick_at(&self, x: f32) -> f64 {
        (self.scroll + (x - self.lanes.left()) as f64 / self.px_per_tick).max(0.0)
    }

    fn lane_top(&self, track: usize) -> f32 {
        self.lanes.top() + track as f32 * LANE_H - self.scroll_y
    }

    fn lane_at(&self, y: f32) -> Option<usize> {
        let offset = y - self.lanes.top() + self.scroll_y;
        (offset >= 0.0).then(|| (offset / LANE_H) as usize)
    }

    fn header_rect(&self, track: usize) -> Rect {
        let top = self.lane_top(track);
        Rect::from_min_max(
            pos2(self.headers.left(), top),
            pos2(self.headers.right(), top + LANE_H),
        )
    }

    fn clip_rect(&self, track: usize, clip: &Clip) -> Rect {
        let top = self.lane_top(track);
        Rect::from_min_max(
            pos2(self.x(clip.start as f64), top + 5.0),
            pos2(self.x(clip.end() as f64), top + LANE_H - 5.0),
        )
    }

    fn hit(&self, p: Pos2, song: &Song) -> Hit {
        if self.ruler.contains(p) {
            return Hit::Ruler(self.tick_at(p.x));
        }
        let in_headers = self.headers.contains(p);
        if !self.lanes.contains(p) && !in_headers {
            return Hit::Nothing;
        }
        let Some(t) = self.lane_at(p.y).filter(|&t| t < song.tracks.len()) else {
            return Hit::Nothing;
        };
        if in_headers {
            let header = self.header_rect(t);
            if header_button(header, 0).contains(p) {
                return Hit::Mute(t);
            }
            if header_button(header, 1).contains(p) {
                return Hit::Solo(t);
            }
        } else {
            let tick = self.tick_at(p.x) as Tick;
            if let Some(i) = song.tracks[t]
                .clips
                .iter()
                .position(|c| c.span().contains(&tick))
            {
                return Hit::Clip(t, i);
            }
        }
        Hit::Lane(t)
    }
}

fn snap(tick: f64, grid: Tick) -> Tick {
    (tick / grid as f64).round() as Tick * grid
}

/// Bar.beat.sixteenth, all counting from 1.
pub fn position_text(tick: Tick) -> String {
    format!(
        "{}.{}.{}",
        tick / TICKS_PER_BAR + 1,
        tick % TICKS_PER_BAR / TICKS_PER_BEAT + 1,
        tick % TICKS_PER_BEAT / (TICKS_PER_BEAT / 4) + 1
    )
}

impl ArrangeView {
    /// `playhead` is the engine's position; `follow` keeps it in view.
    pub fn show(&mut self, ui: &mut egui::Ui, song: &Song, playhead: Tick, follow: bool) -> Output {
        let mut out = Output::default();
        let area = ui.max_rect();
        let response = ui.allocate_rect(area, Sense::click_and_drag());
        let g = Geometry::new(self, area);
        let pointer = ui.input(|i| i.pointer.interact_pos());

        // Scrolling and zooming.
        if response.hovered() {
            let (delta, zoom) = ui.input(|i| {
                let (mut d, mut zoom) = (egui::Vec2::ZERO, false);
                for e in &i.raw.events {
                    if let egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } = e
                    {
                        zoom |= modifiers.command;
                        let scale = match unit {
                            egui::MouseWheelUnit::Point => 1.0,
                            egui::MouseWheelUnit::Line => 40.0,
                            egui::MouseWheelUnit::Page => 400.0,
                        };
                        d += *delta * scale;
                    }
                }
                (d, zoom)
            });
            if delta != egui::Vec2::ZERO {
                if zoom {
                    // The beat at the left edge stays put; the rest
                    // stretches out to the right.
                    let factor = (delta.y + delta.x).mul_add(0.002, 1.0).clamp(0.5, 2.0);
                    self.px_per_beat =
                        (self.px_per_beat * factor).clamp(MIN_PX_PER_BEAT, MAX_PX_PER_BEAT);
                } else {
                    // Sideways wheels and trackpads still move along.
                    self.scroll = (self.scroll - delta.x as f64 / g.px_per_tick).max(0.0);
                    self.scroll_y = self.clamp_y(self.scroll_y - delta.y, song, &g);
                }
            }
        }
        if follow {
            let x = g.x(playhead as f64);
            if x < g.lanes.left() || x > g.lanes.right() - 20.0 {
                self.scroll =
                    (playhead as f64 - 0.1 * g.lanes.width() as f64 / g.px_per_tick).max(0.0);
            }
        }
        let g = Geometry::new(self, area);

        // Presses select, or start on the ruler.
        let pressed = ui.input(|i| i.pointer.primary_pressed());
        if pressed
            && response.contains_pointer()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            match g.hit(p, song) {
                Hit::Ruler(tick) => {
                    self.ruler_press = Some((snap(tick, TICKS_PER_BEAT), p.x, false))
                }
                Hit::Clip(t, i) => {
                    self.selected_track = t;
                    self.selected_clip = Some((t, i));
                }
                Hit::Lane(t) => {
                    self.selected_track = t;
                    self.selected_clip = None;
                }
                Hit::Mute(t) => out.mute = Some(t),
                Hit::Solo(t) => out.solo = Some(t),
                Hit::Nothing => {}
            }
            if g.lanes.contains(p) {
                self.pan = Some((p, self.scroll, self.scroll_y));
            }
        }
        if let (Some((from, scroll, scroll_y)), Some(p)) = (self.pan, pointer) {
            let d = p - from;
            self.scroll = (scroll - d.x as f64 / g.px_per_tick).max(0.0);
            self.scroll_y = self.clamp_y(scroll_y - d.y, song, &g);
        }
        if !ui.input(|i| i.pointer.primary_down()) {
            self.pan = None;
        }
        let g = Geometry::new(self, area);
        if let (Some((from, x, moved)), Some(p)) = (&mut self.ruler_press, pointer) {
            let to = g.tick_at(p.x);
            if (*x - p.x).abs() > 4.0 {
                *moved = true;
            }
            if *moved {
                let a = snap((*from as f64).min(to), TICKS_PER_BAR);
                let b = snap((*from as f64).max(to), TICKS_PER_BAR);
                if b > a {
                    out.loop_range = Some(a..b);
                }
            }
        }
        if !ui.input(|i| i.pointer.any_down())
            && let Some((from, _, moved)) = self.ruler_press.take()
            && !moved
        {
            out.locate = Some(from);
        }

        self.draw(ui, &g, song, playhead);
        out
    }

    /// Keep the tracks from scrolling away: no further than the last one
    /// at the bottom.
    fn clamp_y(&self, y: f32, song: &Song, g: &Geometry) -> f32 {
        let content = song.tracks.len() as f32 * LANE_H;
        y.clamp(0.0, (content - g.lanes.height()).max(0.0))
    }

    fn draw(&self, ui: &egui::Ui, g: &Geometry, song: &Song, playhead: Tick) {
        let p = ui.painter_at(ui.max_rect());
        let font = FontId::proportional(20.0);
        let small = FontId::proportional(15.0);
        p.rect_filled(ui.max_rect(), 0.0, color::BG);

        // Lanes, then the grid over them.
        let lanes = p.with_clip_rect(g.lanes);
        for t in 0..song.tracks.len() {
            let top = g.lane_top(t);
            let r = Rect::from_min_max(
                pos2(g.lanes.left(), top),
                pos2(g.lanes.right(), top + LANE_H),
            );
            let fill = if t == self.selected_track {
                color::CURSOR_ROW
            } else if t % 2 == 0 {
                color::PANEL
            } else {
                color::BEAT
            };
            lanes.rect_filled(r, 0.0, fill);
        }
        let first = g.tick_at(g.lanes.left()) as Tick / TICKS_PER_BEAT;
        let last = g.tick_at(g.lanes.right()) as Tick / TICKS_PER_BEAT + 1;
        let beat_px = TICKS_PER_BEAT as f64 * g.px_per_tick;
        for beat in first..=last {
            let x = g.x((beat * TICKS_PER_BEAT) as f64);
            let bar = (beat * TICKS_PER_BEAT).is_multiple_of(TICKS_PER_BAR);
            if !bar && beat_px < 10.0 {
                continue;
            }
            let c = if bar { color::BAR } else { color::BEAT };
            let c = if bar {
                c.gamma_multiply(1.6)
            } else {
                c.gamma_multiply(1.3)
            };
            lanes.vline(x, g.lanes.y_range(), Stroke::new(1.0, c));
        }
        let end = g.x(song.end() as f64);
        lanes.vline(end, g.lanes.y_range(), Stroke::new(1.0, color::DIM));

        // Clips.
        for (t, track) in song.tracks.iter().enumerate() {
            let tint = color::TRACKS[t % color::TRACKS.len()];
            let synth = song
                .instruments
                .get(track.instrument)
                .is_some_and(Instrument::is_synth);
            for (i, clip) in track.clips.iter().enumerate() {
                let r = g.clip_rect(t, clip);
                if r.right() < g.lanes.left() || r.left() > g.lanes.right() {
                    continue;
                }
                let selected = self.selected_clip == Some((t, i));
                lanes.rect_filled(r, 6.0, tint.gamma_multiply(0.25));
                let edge = if selected {
                    Stroke::new(3.0, color::ACCENT)
                } else {
                    Stroke::new(1.5, tint.gamma_multiply(0.8))
                };
                lanes.rect_stroke(r, 6.0, edge, StrokeKind::Inside);
                let inner = lanes.with_clip_rect(r.shrink(3.0).intersect(g.lanes));
                inner.text(
                    r.left_top() + vec2(8.0, 5.0),
                    Align2::LEFT_TOP,
                    &track.name,
                    small.clone(),
                    color::TEXT,
                );
                draw_hits(&inner, g, clip, r, tint, synth);
            }
        }

        // The ruler, with the loop range.
        p.rect_filled(g.ruler, 0.0, color::PANEL);
        let ruler = p.with_clip_rect(g.ruler);
        if let Some(range) = &song.loop_range {
            let band = Rect::from_min_max(
                pos2(g.x(range.start as f64), g.ruler.top()),
                pos2(g.x(range.end as f64), g.ruler.top() + 8.0),
            );
            ruler.rect_filled(band, 0.0, color::ACCENT);
        }
        let bar_px = TICKS_PER_BAR as f64 * g.px_per_tick;
        let every = (LABEL_GAP as f64 / bar_px).ceil().max(1.0) as Tick;
        let first_bar = g.tick_at(g.ruler.left()) as Tick / TICKS_PER_BAR;
        let last_bar = g.tick_at(g.ruler.right()) as Tick / TICKS_PER_BAR + 1;
        for bar in first_bar..=last_bar {
            let x = g.x((bar * TICKS_PER_BAR) as f64);
            ruler.vline(
                x,
                g.ruler.bottom() - 10.0..=g.ruler.bottom(),
                Stroke::new(1.0, color::DIM),
            );
            if bar.is_multiple_of(every) {
                ruler.text(
                    pos2(x + 5.0, g.ruler.bottom() - 4.0),
                    Align2::LEFT_BOTTOM,
                    (bar + 1).to_string(),
                    small.clone(),
                    color::MUTED,
                );
            }
        }

        // Track headers.
        p.rect_filled(g.headers, 0.0, color::PANEL);
        let headers = p.with_clip_rect(g.headers);
        for (t, track) in song.tracks.iter().enumerate() {
            let top = g.lane_top(t);
            let r = Rect::from_min_max(
                pos2(g.headers.left(), top),
                pos2(g.headers.right(), top + LANE_H),
            );
            if t == self.selected_track {
                headers.rect_filled(r, 0.0, color::CURSOR_ROW);
                headers.rect_filled(
                    Rect::from_min_size(r.min, vec2(5.0, LANE_H)),
                    0.0,
                    color::ACCENT,
                );
            }
            let tint = color::TRACKS[t % color::TRACKS.len()];
            headers.circle_filled(r.left_top() + vec2(24.0, 32.0), 8.0, tint);
            headers.text(
                r.left_top() + vec2(42.0, 32.0),
                Align2::LEFT_CENTER,
                &track.name,
                font.clone(),
                color::TEXT,
            );
            let instrument = song
                .instruments
                .get(track.instrument)
                .map_or("", Instrument::name);
            headers.text(
                r.left_top() + vec2(42.0, 64.0),
                Align2::LEFT_CENTER,
                instrument,
                small.clone(),
                color::MUTED,
            );
            for (k, (label, on, lit)) in [
                ("M", track.mute, color::ERROR),
                ("S", track.solo, color::ACCENT),
            ]
            .into_iter()
            .enumerate()
            {
                let b = header_button(r, k);
                let (fill, text) = if on {
                    (lit, color::BG)
                } else {
                    (color::BG, color::DIM)
                };
                headers.rect_filled(b, 5.0, fill);
                headers.text(
                    b.center(),
                    Align2::CENTER_CENTER,
                    label,
                    small.clone(),
                    text,
                );
            }
            headers.hline(
                g.headers.x_range(),
                top + LANE_H,
                Stroke::new(1.0, color::BG),
            );
        }
        p.vline(
            g.headers.right(),
            g.headers.y_range(),
            Stroke::new(1.0, color::BG),
        );

        // The playhead, over everything on the timeline.
        let x = g.x(playhead as f64);
        if x >= g.ruler.left() {
            p.vline(
                x,
                g.ruler.top()..=g.lanes.bottom(),
                Stroke::new(2.0, color::ACCENT),
            );
            let tri = vec![
                pos2(x - 8.0, g.ruler.top()),
                pos2(x + 8.0, g.ruler.top()),
                pos2(x, g.ruler.top() + 11.0),
            ];
            p.add(egui::Shape::convex_polygon(
                tri,
                color::ACCENT,
                Stroke::NONE,
            ));
        }
    }
}

/// The clip's hits as small marks: across by time, up by slice or note.
fn draw_hits(p: &egui::Painter, g: &Geometry, clip: &Clip, r: Rect, tint: Color32, synth: bool) {
    let hits: Vec<(usize, u16)> = clip
        .cells
        .iter()
        .enumerate()
        .filter_map(|(row, c)| c.trigger.map(|t| (row, t.slice)))
        .collect();
    let Some(lo) = hits.iter().map(|h| h.1).min() else {
        return;
    };
    let hi = hits.iter().map(|h| h.1).max().unwrap_or(lo);
    let top = r.top() + 30.0;
    let height = (r.bottom() - 8.0 - top).max(6.0);
    let row_w = (clip.ticks_per_row() as f64 * g.px_per_tick) as f32;
    for (row, value) in hits {
        // Higher notes higher up; slices top to bottom in order.
        let f = if hi == lo {
            0.5
        } else {
            (value - lo) as f32 / (hi - lo) as f32
        };
        let f = if synth { 1.0 - f } else { f };
        let x = g.x(clip.row_tick(row) as f64);
        let y = top + f * (height - 5.0);
        let w = (row_w - 1.0).clamp(3.0, 16.0);
        p.rect_filled(Rect::from_min_size(pos2(x, y), vec2(w, 5.0)), 1.5, tint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song::{Cell, Track};
    use eframe::egui::{Event, Modifiers, PointerButton, RawInput};

    fn song() -> Song {
        let mut a = Clip::beats(0, 4, 8);
        a.cells[0] = Cell::hit(0, 100);
        let b = Clip::beats(3 * TICKS_PER_BAR, 4, 4);
        Song::new(
            120.0,
            Vec::new(),
            vec![
                Track::new("a", 0).with_clip(a),
                Track::new("b", 0).with_clip(b),
            ],
        )
    }

    struct Harness {
        ctx: egui::Context,
        view: ArrangeView,
        song: Song,
        out: Vec<Output>,
    }

    impl Harness {
        fn new() -> Self {
            let mut h = Harness {
                ctx: egui::Context::default(),
                view: ArrangeView::default(),
                song: song(),
                out: Vec::new(),
            };
            h.frame(vec![]);
            h
        }

        fn frame(&mut self, events: Vec<Event>) {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 600.0))),
                events,
                ..Default::default()
            };
            let (view, song, out) = (&mut self.view, &self.song, &mut self.out);
            let mut full = self.ctx.run_ui(input, |ui| {
                let o = view.show(ui, song, 0, false);
                if o != Output::default() {
                    out.push(o);
                }
            });
            full.textures_delta.clear();
        }

        /// Where a tick is on screen, at the current zoom.
        fn x(&self, tick: Tick) -> f32 {
            HEADER_W
                + ((tick as f64 - self.view.scroll) * self.view.px_per_beat as f64
                    / TICKS_PER_BEAT as f64) as f32
        }

        fn press(&mut self, at: Pos2, pressed: bool) {
            self.frame(vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            }]);
        }

        fn click(&mut self, at: Pos2) {
            self.frame(vec![Event::PointerMoved(at)]);
            self.press(at, true);
            self.press(at, false);
            self.frame(vec![]);
        }

        fn drag(&mut self, from: Pos2, to: Pos2) {
            self.frame(vec![Event::PointerMoved(from)]);
            self.press(from, true);
            for i in 1..=4 {
                self.frame(vec![Event::PointerMoved(
                    from + (to - from) * (i as f32 / 4.0),
                )]);
            }
            self.press(to, false);
            self.frame(vec![]);
        }

        fn wheel(&mut self, at: Pos2, dy: f32, modifiers: Modifiers) {
            self.frame(vec![Event::PointerMoved(at)]);
            self.frame(vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, dy),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers,
            }]);
        }
    }

    #[test]
    fn clicking_the_ruler_moves_the_playhead_to_the_nearest_beat() {
        let mut h = Harness::new();
        let x = h.x(5 * TICKS_PER_BEAT + 30);
        h.click(pos2(x, 10.0));
        assert_eq!(
            h.out,
            vec![Output {
                locate: Some(5 * TICKS_PER_BEAT),
                ..Default::default()
            }]
        );
    }

    #[test]
    fn dragging_along_the_ruler_sets_the_loop_in_bars() {
        let mut h = Harness::new();
        let (a, b) = (h.x(TICKS_PER_BAR + 20), h.x(3 * TICKS_PER_BAR - 30));
        h.drag(pos2(a, 10.0), pos2(b, 10.0));
        let last = h.out.last().unwrap();
        assert_eq!(last.loop_range, Some(TICKS_PER_BAR..3 * TICKS_PER_BAR));
        assert!(
            h.out.iter().all(|o| o.locate.is_none()),
            "a drag doesn't locate"
        );
    }

    #[test]
    fn clicks_select_clips_and_tracks() {
        let mut h = Harness::new();
        let lane = |t: usize| RULER_H + LANE_H * (t as f32 + 0.5);
        h.click(pos2(h.x(3 * TICKS_PER_BAR + 10), lane(1)));
        assert_eq!(
            (h.view.selected_track, h.view.selected_clip),
            (1, Some((1, 0)))
        );
        h.click(pos2(h.x(TICKS_PER_BAR), lane(1)));
        assert_eq!(
            (h.view.selected_track, h.view.selected_clip),
            (1, None),
            "the gap"
        );
        h.click(pos2(40.0, lane(0)));
        assert_eq!(h.view.selected_track, 0, "the header");
    }

    #[test]
    fn ctrl_scroll_zooms_from_the_left_edge() {
        let mut h = Harness::new();
        h.view.scroll = (2 * TICKS_PER_BAR) as f64;
        let at = pos2(800.0, 200.0);
        h.wheel(at, 200.0, Modifiers::COMMAND);
        assert!(
            h.view.px_per_beat > 24.0,
            "zoomed in to {}",
            h.view.px_per_beat
        );
        assert_eq!(
            h.view.scroll,
            (2 * TICKS_PER_BAR) as f64,
            "the left edge stays"
        );
    }

    #[test]
    fn scrolling_moves_the_tracks_up_and_down() {
        let mut h = Harness::new();
        let at = pos2(800.0, 200.0);
        h.wheel(at, -300.0, Modifiers::NONE);
        assert_eq!(h.view.scroll_y, 0.0, "two tracks fit: nothing to scroll");
        for i in 0..8 {
            h.song.tracks.push(Track::new(&format!("t{i}"), 0));
        }
        h.wheel(at, -300.0, Modifiers::NONE);
        assert_eq!(h.view.scroll_y, 300.0);
        assert_eq!(h.view.scroll, 0.0, "not sideways");
        h.wheel(at, -100_000.0, Modifiers::NONE);
        let lanes = 600.0 - RULER_H;
        assert_eq!(
            h.view.scroll_y,
            10.0 * LANE_H - lanes,
            "the last track at the bottom"
        );
    }

    #[test]
    fn dragging_the_lanes_moves_the_view() {
        let mut h = Harness::new();
        for i in 0..8 {
            h.song.tracks.push(Track::new(&format!("t{i}"), 0));
        }
        h.view.scroll = (4 * TICKS_PER_BAR) as f64;
        // Drag left and up by a bar (96 points at 24 per beat) and 100.
        h.drag(pos2(900.0, 300.0), pos2(804.0, 200.0));
        assert_eq!(h.view.scroll, (5 * TICKS_PER_BAR) as f64);
        assert_eq!(h.view.scroll_y, 100.0);
        // Not past the start.
        h.drag(pos2(400.0, 300.0), pos2(1100.0, 300.0));
        assert_eq!(h.view.scroll, 0.0);
        assert!(h.out.is_empty(), "panning doesn't move the playhead");
    }

    #[test]
    fn header_buttons_flip_mute_and_solo() {
        let mut h = Harness::new();
        let header = |t: usize| {
            Rect::from_min_size(
                pos2(0.0, RULER_H + LANE_H * t as f32),
                vec2(HEADER_W, LANE_H),
            )
        };
        h.click(header_button(header(1), 0).center());
        h.click(header_button(header(0), 1).center());
        let flips: Vec<_> = h.out.iter().map(|o| (o.mute, o.solo)).collect();
        assert_eq!(flips, vec![(Some(1), None), (None, Some(0))]);
        assert_eq!(h.view.selected_track, 0, "buttons don't select");
    }

    #[test]
    fn positions_read_as_bars_beats_and_sixteenths() {
        assert_eq!(position_text(0), "1.1.1");
        assert_eq!(
            position_text(TICKS_PER_BAR + 2 * TICKS_PER_BEAT + 96),
            "2.3.3"
        );
    }
}
