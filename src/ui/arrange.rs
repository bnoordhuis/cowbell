//! The arrangement: one lane per track, its clips laid out on a timeline,
//! with a bar ruler on top and the track headers on the left.
//!
//! - Ruler: click to move the playhead, drag along it to set the loop.
//! - Clips: drag to move (to another lane too), drag the right edge to
//!   resize, right-click to duplicate or delete. Moves and resizes snap to
//!   [`ArrangeView::snap`].
//! - Empty lane: double-click for a new clip, drag to move the view.
//! - Headers: mute, solo and a volume fader; right-click or double-click
//!   the name to rename; right-click to duplicate or delete the track.
//! - Scroll to scroll the tracks, ctrl+scroll to zoom.
//!
//! The view never changes the song itself. It reports edits, and the
//! caller applies them (see [`Output`]).

use std::ops::Range;

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2,
};

use super::color;
use super::edit::Edit;
use crate::song::{Clip, ClipRef, Instrument, Song, TICKS_PER_BAR, TICKS_PER_BEAT, Tick};

pub const HEADER_W: f32 = 280.0;
pub const RULER_H: f32 = 32.0;
pub const LANE_H: f32 = 128.0;
const MIN_PX_PER_BEAT: f32 = 4.0;
const MAX_PX_PER_BEAT: f32 = 400.0;
/// Bar numbers closer than this skip some bars.
const LABEL_GAP: f32 = 44.0;
/// How close to a clip's right edge counts as grabbing the edge.
const EDGE_GRAB: f32 = 8.0;
/// How far the pointer must move before a press on a clip moves it.
const DRAG_SLOP: f32 = 4.0;
/// The volume fader's range, as on the track device.
const MAX_VOLUME: f32 = 1.5;

/// The grids moves and resizes snap to, with their names.
pub const SNAPS: [(Tick, &str); 4] = [
    (TICKS_PER_BAR, "Bar"),
    (TICKS_PER_BEAT, "Beat"),
    (TICKS_PER_BEAT / 4, "1/16"),
    (1, "Off"),
];

/// What happened in the arrangement this frame.
#[derive(Default, Debug, PartialEq)]
pub struct Output {
    /// Move the playhead here.
    pub locate: Option<Tick>,
    /// Set the loop range to this.
    pub loop_range: Option<Range<Tick>>,
    /// Edits to make now, each one undo step.
    pub edits: Vec<Edit>,
    /// Part of a mouse gesture (a drag): to be made on the song as it was
    /// when the gesture started, replacing the gesture's previous edit.
    pub live: Option<Edit>,
    /// The gesture is over: what `live` did is one undo step.
    pub gesture_done: bool,
    /// A clip was double-clicked: show it in the clip editor.
    pub open_clip: bool,
}

enum Drag {
    /// On the ruler: the beat it started on, where on screen, and whether
    /// it has turned into a drag yet.
    Ruler {
        from: Tick,
        x: f32,
        moved: bool,
    },
    /// Moving the view: where the press was, and the scroll offsets then.
    Pan {
        from: Pos2,
        scroll: f64,
        scroll_y: f32,
    },
    /// Moving a clip, held `grab` ticks in from its start.
    Move {
        clip: ClipRef,
        grab: f64,
        from: Pos2,
        moved: bool,
    },
    /// Stretching a clip's right edge.
    Resize {
        clip: ClipRef,
        start: Tick,
        ticks_per_row: Tick,
        moved: bool,
    },
    Volume {
        track: usize,
    },
}

impl Drag {
    /// Whether the drag has made edits that need an undo step.
    fn edited(&self) -> bool {
        match self {
            Drag::Move { moved, .. } | Drag::Resize { moved, .. } => *moved,
            Drag::Volume { .. } => true,
            Drag::Ruler { .. } | Drag::Pan { .. } => false,
        }
    }
}

/// What a right-click menu is for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Context {
    Clip(ClipRef),
    Track(usize),
}

/// A track name being edited.
struct Rename {
    track: usize,
    text: String,
    focused: bool,
}

pub struct ArrangeView {
    /// Leftmost tick in view.
    pub scroll: f64,
    /// Points the lanes are scrolled up by.
    pub scroll_y: f32,
    pub px_per_beat: f32,
    pub selected_track: usize,
    pub selected_clip: Option<ClipRef>,
    /// Moves and resizes land on multiples of this.
    pub snap: Tick,
    drag: Option<Drag>,
    context: Option<Context>,
    rename: Option<Rename>,
}

impl Default for ArrangeView {
    fn default() -> Self {
        ArrangeView {
            scroll: 0.0,
            scroll_y: 0.0,
            px_per_beat: 24.0,
            selected_track: 0,
            selected_clip: None,
            snap: TICKS_PER_BEAT,
            drag: None,
            context: None,
            rename: None,
        }
    }
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
    Clip(ClipRef),
    ClipEdge(ClipRef),
    Lane(usize),
    Header(usize),
    Mute(usize),
    Solo(usize),
    Volume(usize),
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

/// A track header's volume fader: the groove.
fn fader(header: Rect) -> Rect {
    Rect::from_min_max(
        pos2(header.left() + 42.0, header.top() + 97.0),
        pos2(header.right() - 24.0, header.top() + 103.0),
    )
}

/// Where the track name goes, for renaming in place.
fn name_rect(header: Rect) -> Rect {
    Rect::from_min_max(
        header.left_top() + vec2(36.0, 16.0),
        pos2(header.right() - 96.0, header.top() + 48.0),
    )
}

fn db(volume: f32) -> String {
    if volume <= 0.0 {
        "−∞ dB".to_owned()
    } else {
        format!("{:+.1} dB", 20.0 * volume.log10())
    }
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

    /// The add-track button, under the last header.
    fn add_track(&self, tracks: usize) -> Rect {
        let top = self.lane_top(tracks);
        Rect::from_min_max(
            pos2(self.headers.left() + 12.0, top + 12.0),
            pos2(self.headers.right() - 12.0, top + 52.0),
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
            if fader(header).expand2(vec2(8.0, 10.0)).contains(p) {
                return Hit::Volume(t);
            }
            return Hit::Header(t);
        }
        for (i, clip) in song.tracks[t].clips.iter().enumerate() {
            let r = self.clip_rect(t, clip);
            if (r.right() - p.x).abs() <= EDGE_GRAB && p.x > r.left() + EDGE_GRAB {
                return Hit::ClipEdge((t, i));
            }
            if r.left() <= p.x && p.x < r.right() {
                return Hit::Clip((t, i));
            }
        }
        Hit::Lane(t)
    }

    /// The volume a pointer at `x` sets on a fader.
    fn volume_at(&self, track: usize, x: f32) -> f32 {
        let f = fader(self.header_rect(track));
        ((x - f.left()) / f.width()).clamp(0.0, 1.0) * MAX_VOLUME
    }
}

/// `tick` rounded to the nearest multiple of `grid`.
fn snap(tick: f64, grid: Tick) -> Tick {
    (tick.max(0.0) / grid as f64).round() as Tick * grid
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

        self.wheel(ui, &response, song, &g);
        if follow {
            let x = g.x(playhead as f64);
            if x < g.lanes.left() || x > g.lanes.right() - 20.0 {
                self.scroll =
                    (playhead as f64 - 0.1 * g.lanes.width() as f64 / g.px_per_tick).max(0.0);
            }
        }
        let g = Geometry::new(self, area);

        let (pressed, secondary) =
            ui.input(|i| (i.pointer.primary_pressed(), i.pointer.secondary_pressed()));
        if pressed
            && self.rename.is_none()
            && response.contains_pointer()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            self.press(p, &g, song, &mut out);
        }
        if secondary
            && response.contains_pointer()
            && let Some(p) = pointer
        {
            self.context = match g.hit(p, song) {
                Hit::Clip(c) | Hit::ClipEdge(c) => {
                    self.selected_track = c.0;
                    self.selected_clip = Some(c);
                    Some(Context::Clip(c))
                }
                Hit::Header(t) | Hit::Volume(t) | Hit::Mute(t) | Hit::Solo(t) => {
                    self.selected_track = t;
                    Some(Context::Track(t))
                }
                _ => None,
            };
        }
        if let Some(p) = pointer {
            self.drag_to(p, &g, song, &mut out);
        }
        if !ui.input(|i| i.pointer.primary_down())
            && let Some(drag) = self.drag.take()
        {
            match drag {
                Drag::Ruler {
                    from, moved: false, ..
                } => out.locate = Some(from),
                d if d.edited() => out.gesture_done = true,
                _ => {}
            }
        }
        if response.double_clicked()
            && let Some(p) = pointer
        {
            match g.hit(p, song) {
                Hit::Clip(_) => out.open_clip = true,
                Hit::Lane(track) => {
                    let start = (g.tick_at(p.x) / self.snap as f64).floor() as Tick * self.snap;
                    out.edits.push(Edit::NewClip { track, start });
                }
                Hit::Header(track) if name_rect(g.header_rect(track)).contains(p) => {
                    self.start_rename(track, song);
                }
                _ => {}
            }
        }
        self.cursor(ui, pointer, &g, song);

        self.draw(ui, &g, song, playhead);
        self.context_menu(&response, song, &mut out);
        self.rename_box(ui, &g, &mut out);
        self.add_track_button(ui, &g, song, &mut out);
        out
    }

    fn wheel(&mut self, ui: &egui::Ui, response: &egui::Response, song: &Song, g: &Geometry) {
        if !response.hovered() {
            return;
        }
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
        if delta == egui::Vec2::ZERO {
            return;
        }
        if zoom {
            // The beat at the left edge stays put; the rest stretches out
            // to the right.
            let factor = (delta.y + delta.x).mul_add(0.002, 1.0).clamp(0.5, 2.0);
            self.px_per_beat = (self.px_per_beat * factor).clamp(MIN_PX_PER_BEAT, MAX_PX_PER_BEAT);
        } else {
            // Sideways wheels and trackpads still move along.
            self.scroll = (self.scroll - delta.x as f64 / g.px_per_tick).max(0.0);
            self.scroll_y = self.clamp_y(self.scroll_y - delta.y, song, g);
        }
    }

    fn press(&mut self, p: Pos2, g: &Geometry, song: &Song, out: &mut Output) {
        match g.hit(p, song) {
            Hit::Ruler(tick) => {
                self.drag = Some(Drag::Ruler {
                    from: snap(tick, TICKS_PER_BEAT),
                    x: p.x,
                    moved: false,
                });
            }
            Hit::Clip(c) => {
                self.selected_track = c.0;
                self.selected_clip = Some(c);
                let start = song.clip(c).map_or(0, |clip| clip.start);
                self.drag = Some(Drag::Move {
                    clip: c,
                    grab: g.tick_at(p.x) - start as f64,
                    from: p,
                    moved: false,
                });
            }
            Hit::ClipEdge(c) => {
                self.selected_track = c.0;
                self.selected_clip = Some(c);
                if let Some(clip) = song.clip(c) {
                    self.drag = Some(Drag::Resize {
                        clip: c,
                        start: clip.start,
                        ticks_per_row: clip.ticks_per_row(),
                        moved: false,
                    });
                }
            }
            Hit::Lane(t) => {
                self.selected_track = t;
                self.selected_clip = None;
                self.drag = Some(Drag::Pan {
                    from: p,
                    scroll: self.scroll,
                    scroll_y: self.scroll_y,
                });
            }
            Hit::Header(t) => {
                self.selected_track = t;
                self.selected_clip = None;
            }
            Hit::Mute(t) => out.edits.push(Edit::Mute(t)),
            Hit::Solo(t) => out.edits.push(Edit::Solo(t)),
            Hit::Volume(t) => {
                self.selected_track = t;
                self.drag = Some(Drag::Volume { track: t });
                out.live = Some(Edit::Volume(t, g.volume_at(t, p.x)));
            }
            Hit::Nothing => {}
        }
    }

    fn drag_to(&mut self, p: Pos2, g: &Geometry, song: &Song, out: &mut Output) {
        let snap_grid = self.snap;
        let tracks = song.tracks.len();
        match &mut self.drag {
            Some(Drag::Ruler { from, x, moved }) => {
                if (*x - p.x).abs() > DRAG_SLOP {
                    *moved = true;
                }
                if *moved {
                    let to = g.tick_at(p.x);
                    let a = snap((*from as f64).min(to), TICKS_PER_BAR);
                    let b = snap((*from as f64).max(to), TICKS_PER_BAR);
                    if b > a {
                        out.loop_range = Some(a..b);
                    }
                }
            }
            Some(Drag::Pan {
                from,
                scroll,
                scroll_y,
            }) => {
                let d = p - *from;
                let (scroll, scroll_y) = (*scroll, *scroll_y);
                self.scroll = (scroll - d.x as f64 / g.px_per_tick).max(0.0);
                self.scroll_y = self.clamp_y(scroll_y - d.y, song, g);
            }
            Some(Drag::Move {
                clip,
                grab,
                from,
                moved,
            }) => {
                if from.distance(p) > DRAG_SLOP {
                    *moved = true;
                }
                if *moved && tracks > 0 {
                    let start = snap(g.tick_at(p.x) - *grab, snap_grid);
                    let track = g.lane_at(p.y).unwrap_or(0).min(tracks - 1);
                    out.live = Some(Edit::MoveClip {
                        clip: *clip,
                        track,
                        start,
                    });
                }
            }
            Some(Drag::Resize {
                clip,
                start,
                ticks_per_row,
                moved,
            }) => {
                let end = snap(g.tick_at(p.x), snap_grid).max(*start + *ticks_per_row);
                let rows = ((end - *start) as f64 / *ticks_per_row as f64)
                    .round()
                    .max(1.0);
                let rows = rows as usize;
                if song.clip(*clip).is_some_and(|c| c.cells.len() != rows) {
                    *moved = true;
                }
                if *moved {
                    out.live = Some(Edit::ResizeClip { clip: *clip, rows });
                }
            }
            Some(Drag::Volume { track }) => {
                out.live = Some(Edit::Volume(*track, g.volume_at(*track, p.x)));
            }
            None => {}
        }
    }

    fn cursor(&self, ui: &egui::Ui, pointer: Option<Pos2>, g: &Geometry, song: &Song) {
        let icon = match &self.drag {
            Some(Drag::Move { moved: true, .. } | Drag::Pan { .. }) => CursorIcon::Grabbing,
            Some(Drag::Resize { .. }) => CursorIcon::ResizeHorizontal,
            Some(_) => return,
            None => match pointer.map(|p| g.hit(p, song)) {
                Some(Hit::ClipEdge(_)) => CursorIcon::ResizeHorizontal,
                Some(Hit::Clip(_)) => CursorIcon::Grab,
                _ => return,
            },
        };
        ui.ctx().set_cursor_icon(icon);
    }

    fn start_rename(&mut self, track: usize, song: &Song) {
        if let Some(t) = song.tracks.get(track) {
            self.rename = Some(Rename {
                track,
                text: t.name.clone(),
                focused: false,
            });
        }
    }

    fn context_menu(&mut self, response: &egui::Response, song: &Song, out: &mut Output) {
        let Some(context) = self.context else {
            return;
        };
        let mut rename = None;
        response.context_menu(|ui| match context {
            Context::Clip(c) => {
                if ui.button("Duplicate clip").clicked() {
                    out.edits.push(Edit::DuplicateClip(c));
                }
                if ui.button("Delete clip").clicked() {
                    out.edits.push(Edit::DeleteClip(c));
                }
            }
            Context::Track(t) => {
                if ui.button("Rename track").clicked() {
                    rename = Some(t);
                }
                let playing = song.tracks.get(t).map(|tr| tr.instrument);
                ui.menu_button("Plays", |ui| {
                    for (i, inst) in song.instruments.iter().enumerate() {
                        if ui.radio(playing == Some(i), inst.name()).clicked() && playing != Some(i)
                        {
                            out.edits.push(Edit::SetInstrument(t, i));
                        }
                    }
                });
                if ui.button("Duplicate track").clicked() {
                    out.edits.push(Edit::DuplicateTrack(t));
                }
                if ui.button("Delete track").clicked() {
                    out.edits.push(Edit::DeleteTrack(t));
                }
            }
        });
        if let Some(t) = rename {
            self.start_rename(t, song);
        }
    }

    fn rename_box(&mut self, ui: &mut egui::Ui, g: &Geometry, out: &mut Output) {
        let Some(r) = &mut self.rename else {
            return;
        };
        let edit = ui.put(
            name_rect(g.header_rect(r.track)),
            egui::TextEdit::singleline(&mut r.text).font(FontId::proportional(18.0)),
        );
        if !r.focused {
            edit.request_focus();
            r.focused = true;
        } else if edit.lost_focus() {
            let r = self.rename.take().unwrap();
            let name = r.text.trim();
            let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if !cancelled && !name.is_empty() {
                out.edits.push(Edit::RenameTrack(r.track, name.to_owned()));
            }
        }
    }

    fn add_track_button(&mut self, ui: &mut egui::Ui, g: &Geometry, song: &Song, out: &mut Output) {
        let rect = g.add_track(song.tracks.len());
        if !g.headers.intersects(rect) {
            return;
        }
        let builder = egui::UiBuilder::new().max_rect(rect);
        ui.scope_builder(builder, |ui| {
            ui.set_clip_rect(g.headers);
            let button = egui::Button::new(egui::RichText::new("+ Add track").size(15.0))
                .min_size(rect.size());
            egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
                ui.label(egui::RichText::new("Plays").color(color::MUTED));
                for (i, inst) in song.instruments.iter().enumerate() {
                    if ui.button(inst.name()).clicked() {
                        out.edits.push(Edit::AddTrack(i));
                    }
                }
                if song.instruments.is_empty() {
                    ui.label("no instruments yet");
                }
            });
        });
    }

    /// Keep the tracks from scrolling away: no further than the last one
    /// at the bottom.
    fn clamp_y(&self, y: f32, song: &Song, g: &Geometry) -> f32 {
        // Room for the add-track button under the last track.
        let content = song.tracks.len() as f32 * LANE_H + 64.0;
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
            let c = if bar {
                color::BAR.gamma_multiply(1.6)
            } else {
                color::BEAT.gamma_multiply(1.3)
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
                // A grip on the right edge, for resizing.
                let grip = Rect::from_min_max(
                    pos2(r.right() - 6.0, r.center().y - 12.0),
                    pos2(r.right() - 3.0, r.center().y + 12.0),
                );
                lanes.rect_filled(grip, 1.5, tint.gamma_multiply(0.7));
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
            let r = g.header_rect(t);
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
            if self.rename.as_ref().is_none_or(|n| n.track != t) {
                headers
                    .with_clip_rect(name_rect(r).intersect(g.headers))
                    .text(
                        r.left_top() + vec2(42.0, 32.0),
                        Align2::LEFT_CENTER,
                        &track.name,
                        font.clone(),
                        color::TEXT,
                    );
            }
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
            // The volume fader, with a mark at full (unity) volume.
            let volume = song.volume(t);
            let f = fader(r);
            headers.rect_filled(f, 3.0, color::BG);
            let x = f.left() + volume / MAX_VOLUME * f.width();
            headers.rect_filled(
                Rect::from_min_max(f.min, pos2(x, f.bottom())),
                3.0,
                tint.gamma_multiply(0.7),
            );
            let unity = f.left() + f.width() / MAX_VOLUME;
            headers.vline(
                unity,
                f.top() - 5.0..=f.bottom() + 5.0,
                Stroke::new(1.0, color::DIM),
            );
            headers.circle_filled(pos2(x, f.center().y), 8.0, color::TEXT);
            headers.text(
                pos2(f.right(), f.top() - 10.0),
                Align2::RIGHT_BOTTOM,
                db(volume),
                FontId::proportional(13.0),
                color::MUTED,
            );
            headers.hline(g.headers.x_range(), r.bottom(), Stroke::new(1.0, color::BG));
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
    use crate::song::{Cell, TRACK_VOLUME, Track};
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

        fn lane(t: usize) -> f32 {
            RULER_H + LANE_H * (t as f32 + 0.5)
        }

        fn header(t: usize) -> Rect {
            Rect::from_min_size(
                pos2(0.0, RULER_H + LANE_H * t as f32),
                vec2(HEADER_W, LANE_H),
            )
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

        fn lives(&self) -> Vec<Edit> {
            self.out.iter().filter_map(|o| o.live.clone()).collect()
        }

        fn edits(&self) -> Vec<Edit> {
            self.out.iter().flat_map(|o| o.edits.clone()).collect()
        }

        fn done(&self) -> usize {
            self.out.iter().filter(|o| o.gesture_done).count()
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
        h.click(pos2(h.x(3 * TICKS_PER_BAR + 10), Harness::lane(1)));
        assert_eq!(
            (h.view.selected_track, h.view.selected_clip),
            (1, Some((1, 0)))
        );
        h.click(pos2(h.x(TICKS_PER_BAR), Harness::lane(1)));
        assert_eq!(
            (h.view.selected_track, h.view.selected_clip),
            (1, None),
            "the gap"
        );
        h.click(pos2(40.0, Harness::lane(0)));
        assert_eq!(h.view.selected_track, 0, "the header");
        assert!(h.out.is_empty(), "clicks change nothing");
    }

    #[test]
    fn dragging_a_clip_moves_it_on_the_grid() {
        let mut h = Harness::new();
        // Grab the first clip a little in, drop it 2 beats and a bit later.
        let from = pos2(h.x(50), Harness::lane(0));
        let to = pos2(h.x(50 + 2 * TICKS_PER_BEAT + 40), Harness::lane(0));
        h.drag(from, to);
        assert_eq!(
            h.lives().last(),
            Some(&Edit::MoveClip {
                clip: (0, 0),
                track: 0,
                start: 2 * TICKS_PER_BEAT
            })
        );
        assert_eq!(h.done(), 1, "one undo step");
        // To the lane below, on a bar grid.
        h.out.clear();
        h.view.snap = TICKS_PER_BAR;
        h.drag(from, pos2(h.x(50 + TICKS_PER_BAR + 100), Harness::lane(1)));
        assert_eq!(
            h.lives().last(),
            Some(&Edit::MoveClip {
                clip: (0, 0),
                track: 1,
                start: TICKS_PER_BAR
            })
        );
    }

    #[test]
    fn dragging_the_right_edge_resizes() {
        let mut h = Harness::new();
        // The first clip is 8 beats of 4 rows; pull its end to beat 10.
        let edge = pos2(h.x(8 * TICKS_PER_BEAT) - 2.0, Harness::lane(0));
        h.drag(edge, pos2(h.x(10 * TICKS_PER_BEAT) + 5.0, Harness::lane(0)));
        assert_eq!(
            h.lives().last(),
            Some(&Edit::ResizeClip {
                clip: (0, 0),
                rows: 40
            })
        );
        assert_eq!(h.done(), 1);
    }

    #[test]
    fn double_click_on_an_empty_lane_makes_a_clip() {
        let mut h = Harness::new();
        let at = pos2(
            h.x(TICKS_PER_BAR + 3 * TICKS_PER_BEAT + 50),
            Harness::lane(1),
        );
        h.click(at);
        h.click(at);
        assert_eq!(
            h.edits(),
            vec![Edit::NewClip {
                track: 1,
                start: TICKS_PER_BAR + 3 * TICKS_PER_BEAT
            }],
            "starts on the beat it was clicked in"
        );
    }

    #[test]
    fn the_fader_sets_volume() {
        let mut h = Harness::new();
        let f = fader(Harness::header(1));
        h.drag(f.center(), pos2(f.left() + f.width() / 3.0, f.center().y));
        let last = h.lives().last().cloned();
        let Some(Edit::Volume(1, v)) = last else {
            panic!("{last:?}")
        };
        assert!((v - 0.5).abs() < 0.01, "{v}");
        assert_eq!(h.done(), 1);
    }

    #[test]
    fn header_buttons_flip_mute_and_solo() {
        let mut h = Harness::new();
        h.click(header_button(Harness::header(1), 0).center());
        h.click(header_button(Harness::header(0), 1).center());
        assert_eq!(h.edits(), vec![Edit::Mute(1), Edit::Solo(0)]);
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
            10.0 * LANE_H + 64.0 - lanes,
            "the last track and the add button at the bottom"
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
        assert!(h.out.is_empty(), "panning changes nothing");
    }

    #[test]
    fn positions_read_as_bars_beats_and_sixteenths() {
        assert_eq!(position_text(0), "1.1.1");
        assert_eq!(
            position_text(TICKS_PER_BAR + 2 * TICKS_PER_BEAT + 96),
            "2.3.3"
        );
        assert_eq!(db(1.0), "+0.0 dB");
        assert_eq!(db(0.5), "-6.0 dB");
    }

    #[test]
    fn volume_marks_unity() {
        // The fader's full-volume mark matches the track device's default.
        let spec = &crate::rack::Kind::Track(0).params()[TRACK_VOLUME];
        assert_eq!((spec.default, spec.max), (1.0, MAX_VOLUME));
    }
}
