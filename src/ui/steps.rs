//! The clip editor: the selected clip as a drum machine or piano roll.
//! Sample tracks get a lane per slice, synth tracks a piano roll.
//!
//! Click or drag over empty cells to paint hits, over hits to erase them.
//! On synth tracks a new note lasts one step; drag right to make it longer,
//! or drag inside a note to move its end. Right-click a hit to roll it,
//! scroll over a hit to change its velocity, scroll elsewhere to see other
//! lanes. Click a lane's label to hear it.
//!
//! Shift+drag over the grid to select steps in some lanes, or drag along
//! the beat numbers to select whole steps; the window copies, cuts, pastes
//! and clears them (see [`StepView::copy`]).
//!
//! Under a synth's piano roll is the sustain pedal: drag along it to hold
//! the pedal down, drag over where it's down to let it up, and right-click
//! where it's down to lift it and put it straight back (a pedal change).
//!
//! Like the arrangement, it doesn't change the song itself: it reports
//! edits in its [`Output`].

use std::ops::Range;
use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, pos2, vec2,
};

use super::edit::Edit;
use super::{color, knob};
use crate::rack::{Curve, ParamSpec};
use crate::sample::Sample;
use crate::selection::{self, Selection, Steps};
use crate::song::{
    Cell, ClipRef, Fx, Instrument, MAX_FX, Song, Trigger, pedal_rows, with_pedal_rows,
};
use crate::tuning::{HIGHEST_NOTE, LOWEST_NOTE, note_name};

const HEADER: f32 = 20.0;
const LABEL_W: f32 = 100.0;
const MIN_LANE: f32 = 16.0;
const MAX_LANE: f32 = 30.0;
const NOTE_LANE: f32 = 16.0;
/// The sustain pedal's lane, under a piano roll, and the gap above it.
const PEDAL_H: f32 = 18.0;
const PEDAL_GAP: f32 = 6.0;
/// How strongly the lane and step under the pointer light up.
const HOVER_ALPHA: u8 = 9;
const MIN_COL: f32 = 12.0;
const MAX_COL: f32 = 48.0;
/// Right-click steps a hit through these retrigger counts, then back to a
/// single hit.
const ROLLS: [u8; 5] = [2, 3, 4, 6, 8];
/// Mouse wheel over a hit turns its velocity like a knob.
const VELOCITY: ParamSpec = ParamSpec::new("velocity", 1.0, 127.0, 127.0, Curve::Linear, "");
/// Columns in a slice's waveform glyph.
const PEAKS: usize = 12;
/// Where a new synth clip's piano roll starts out, with no notes to go by:
/// around middle C.
const HOME_NOTE: u16 = 66;

const CELL_A: Color32 = Color32::from_rgb(0x20, 0x21, 0x25);
const CELL_B: Color32 = Color32::from_rgb(0x26, 0x27, 0x2b);
const BLACK_KEY_A: Color32 = Color32::from_rgb(0x1b, 0x1c, 0x20);
const BLACK_KEY_B: Color32 = Color32::from_rgb(0x21, 0x22, 0x26);
const GLYPH: Color32 = Color32::from_rgba_premultiplied(0x10, 0x11, 0x13, 0xc0);

pub struct StepView {
    gesture: Option<Gesture>,
    wheel: knob::Wheel<(ClipRef, usize)>,
    /// A run of wheel turns is one undo step; this one hasn't ended yet.
    wheeling: bool,
    /// Lanes scrolled past at the top.
    lane_scroll: usize,
    /// The clip and kind the scroll was set for: another clip gets its
    /// lanes centred again.
    scrolled_for: Option<(ClipRef, bool)>,
    /// The lane to keep in the middle until the lanes are scrolled by hand:
    /// the view's height can change after a clip opens (the window's first
    /// frames, a resize), and its notes should stay in sight.
    centre_lane: Option<usize>,
    /// First row shown.
    col_scroll: usize,
    /// The row last touched, outlined.
    cursor: Option<usize>,
    /// New hits get this velocity; the wheel sets it.
    velocity: u8,
    peaks: PeakCache,
    /// The steps selected, and in which clip.
    selection: Option<(ClipRef, Selection)>,
    /// Dragging out a selection: from which lane (index) and row, and
    /// whether along the beat numbers (every lane).
    selecting: Option<(ClipRef, usize, usize, bool)>,
    /// The row under the pointer, where a paste goes.
    hover_row: Option<(ClipRef, usize)>,
}

impl Default for StepView {
    fn default() -> Self {
        StepView {
            gesture: None,
            wheel: knob::Wheel::default(),
            wheeling: false,
            lane_scroll: 0,
            scrolled_for: None,
            centre_lane: None,
            col_scroll: 0,
            cursor: None,
            velocity: 110,
            peaks: PeakCache::default(),
            selection: None,
            selecting: None,
            hover_row: None,
        }
    }
}

/// What happened in the clip editor this frame.
#[derive(Default, Debug, PartialEq)]
pub struct Output {
    /// Edits to make now, each one undo step.
    pub edits: Vec<Edit>,
    /// The latest step of a gesture; see [`super::edit`].
    pub live: Option<Edit>,
    /// The previous gesture is over, before `live` starts a new one.
    pub end_first: bool,
    /// The gesture is over, after `live`.
    pub gesture_done: bool,
    /// Play this slice or note of this instrument.
    pub audition: Option<(usize, u16)>,
}

/// A mouse gesture: what it does, applied afresh to the cells as they were
/// when it started, so dragging back undoes what it passed over.
struct Gesture {
    clip: ClipRef,
    base: Vec<Cell>,
    kind: Kind,
}

#[derive(Debug, PartialEq)]
enum Kind {
    /// Put hits in these (lane, row) cells, in order.
    Paint(Vec<(u16, usize)>),
    /// Remove the hits in these (lane, row) cells.
    Erase(Vec<(u16, usize)>),
    /// A synth note from `start` through `end`. On a poly track that's its
    /// length; on a mono one, the length of its row's notes together.
    Note {
        note: u16,
        start: usize,
        end: usize,
        velocity: u8,
        poly: bool,
    },
    /// The pedal down (or up) from row `from` through `to`, either way.
    Pedal { from: usize, to: usize, down: bool },
}

/// Whether clip `at` plays a synth: its lanes are notes, not slices.
fn is_synth(song: &Song, at: ClipRef) -> bool {
    song.tracks
        .get(at.0)
        .and_then(|t| song.instruments.get(t.instrument))
        .is_some_and(Instrument::is_synth)
}

fn has_cut(c: &Cell) -> bool {
    c.fx().any(|f| matches!(f, Fx::Cut(_)))
}

fn without_cut(mut c: Cell) -> Cell {
    for f in &mut c.fx {
        if matches!(f, Some(Fx::Cut(_))) {
            *f = None;
        }
    }
    c
}

/// A synth note sounding over rows `start..=end`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    note: u16,
    start: usize,
    end: usize,
}

/// Where a synth track's notes sound, as the engine plays them. On a mono
/// track a row's notes last until the next row with notes, or a cut (or
/// the first note's length, if it has one); on a poly track each note
/// lasts its own length, or until a cut. A cut in a note's own row ends it
/// there.
fn spans(cells: &[Cell], poly: bool) -> Vec<Span> {
    let n = cells.len();
    let stops = |r: usize| {
        let c = &cells[r];
        (!poly && c.has_hit()) || (!c.has_hit() && has_cut(c))
    };
    let mut out = Vec::new();
    for (row, c) in cells.iter().enumerate() {
        let Some(first) = c.first() else {
            continue;
        };
        let open = if has_cut(c) {
            row
        } else {
            (row + 1..n).find(|&r| stops(r)).map_or(n - 1, |r| r - 1)
        };
        for t in c.hits() {
            let rows = if poly { t.rows } else { first.rows };
            let end = if rows > 0 {
                (row + rows as usize - 1).min(open)
            } else {
                open
            };
            out.push(Span {
                note: t.slice,
                start: row,
                end,
            });
        }
    }
    out
}

/// The row of the hit at (lane `v`, `row`): the cell itself on sample
/// tracks, the start of the note covering it on synth tracks.
fn hit_row(cells: &[Cell], synth: bool, poly: bool, v: u16, row: usize) -> Option<usize> {
    if synth {
        spans(cells, poly)
            .into_iter()
            .filter(|s| s.note == v && (s.start..=s.end).contains(&row))
            .map(|s| s.start)
            .max()
    } else {
        cells[row].note(v).map(|_| row)
    }
}

fn apply(base: &[Cell], kind: &Kind, velocity: u8) -> Vec<Cell> {
    let mut cells = base.to_vec();
    match kind {
        &Kind::Pedal { from, to, down } => {
            let (mut rows, mut change) = pedal_rows(base);
            for r in from.min(to)..=from.max(to).min(rows.len().saturating_sub(1)) {
                rows[r] = down;
                change[r] = false;
            }
            cells = with_pedal_rows(base, &rows, &change);
        }
        Kind::Paint(cells_at) => {
            for &(v, r) in cells_at {
                // Join what's in the row, if there's room.
                if cells[r].note(v).is_none() {
                    cells[r].add(Trigger::new(v, velocity));
                }
            }
        }
        Kind::Erase(cells_at) => {
            for &(v, r) in cells_at {
                if cells[r].remove(v) && !cells[r].has_hit() {
                    // Its effects were for it.
                    cells[r] = Cell::default();
                }
            }
        }
        &Kind::Note {
            note,
            start,
            end,
            velocity,
            poly: true,
        } => {
            let rows = (end - start + 1) as u16;
            cells[start].add(Trigger {
                slice: note,
                velocity,
                rows,
            });
        }
        &Kind::Note {
            note,
            start,
            end,
            velocity,
            poly: false,
        } => {
            // The row's notes last until the next row with notes or a cut:
            // clear the way to `end`, and cut after it.
            let mut c = without_cut(cells[start]);
            c.add(Trigger::new(note, velocity));
            for n in c.notes.iter_mut().flatten() {
                n.rows = 0;
            }
            cells[start] = c;
            for c in &mut cells[start + 1..=end] {
                c.clear_notes();
                *c = without_cut(*c);
            }
            if let Some(next) = cells.get_mut(end + 1)
                && !next.has_hit()
                && !has_cut(next)
            {
                *next = next.with_fx(Fx::Cut(0));
            }
        }
    }
    cells
}

/// The next roll for a cell: one more step through [`ROLLS`].
fn next_roll(cell: Cell) -> Cell {
    let now = cell
        .fx()
        .find_map(|f| match f {
            Fx::Retrig(n) => Some(n),
            _ => None,
        })
        .unwrap_or(1);
    let mut c = cell;
    for f in &mut c.fx {
        if matches!(f, Some(Fx::Retrig(_))) {
            *f = None;
        }
    }
    match ROLLS.iter().find(|&&n| n > now) {
        Some(&n) if c.fx.iter().any(Option::is_none) => c.with_fx(Fx::Retrig(n)),
        Some(&n) => {
            c.fx[MAX_FX - 1] = Some(Fx::Retrig(n));
            c
        }
        None => c,
    }
}

/// The audio and slice markers peaks were worked out for.
type PeakKey = (Arc<Vec<[f32; 2]>>, Vec<usize>);

/// Per slice: the loudest sample in each of [`PEAKS`] columns, 0..=1.
#[derive(Default)]
struct PeakCache {
    key: Option<PeakKey>,
    peaks: Vec<[f32; PEAKS]>,
}

impl PeakCache {
    fn get(&mut self, sample: &Sample) -> &[[f32; PEAKS]] {
        let fresh = self
            .key
            .as_ref()
            .is_some_and(|(f, s)| Arc::ptr_eq(f, &sample.frames) && *s == sample.slices);
        if !fresh {
            self.peaks = (0..sample.slices.len())
                .map(|i| {
                    let (a, b) = sample.slice_bounds(i).unwrap();
                    let frames = &sample.frames[a..b.max(a)];
                    let mut out = [0.0f32; PEAKS];
                    for (k, o) in out.iter_mut().enumerate() {
                        let (x, y) = (k * frames.len() / PEAKS, (k + 1) * frames.len() / PEAKS);
                        *o = frames[x..y]
                            .iter()
                            .map(|[l, r]| (0.5 * (l + r)).abs())
                            .fold(0.0, f32::max);
                    }
                    let top = out.iter().copied().fold(1e-6, f32::max);
                    out.map(|p| p / top)
                })
                .collect();
            self.key = Some((sample.frames.clone(), sample.slices.clone()));
        }
        &self.peaks
    }
}

/// Where everything goes, for drawing and hit testing alike.
struct Geometry {
    labels: Rect,
    grid: Rect,
    /// Lane values, top to bottom: slices ascending, notes descending.
    lanes: Vec<u16>,
    first_lane: usize,
    lane_h: f32,
    first_col: usize,
    col_w: f32,
    rows: usize,
    /// Synths have a pedal lane under the grid.
    pedal: bool,
}

impl Geometry {
    /// The pedal lane, labels and all, if there is one.
    fn pedal_rect(&self) -> Option<Rect> {
        self.pedal.then(|| {
            let top = self.grid.top() + self.visible_lanes() as f32 * self.lane_h + PEDAL_GAP;
            Rect::from_min_max(
                pos2(self.labels.left(), top),
                pos2(self.grid.right(), top + PEDAL_H),
            )
        })
    }

    /// For selecting: the lane under `p` (clamped to those shown), where
    /// below the lanes, from halfway into the gap, is the pedal lane,
    /// `lanes.len()`.
    fn select_lane(&self, p: Pos2) -> usize {
        match self.pedal_rect() {
            Some(r) if p.y >= r.top() - PEDAL_GAP / 2.0 => self.lanes.len(),
            _ => self.cell_at(p).0,
        }
    }

    /// The selection of `rows` from lane `from` to lane `to` (as
    /// [`Geometry::select_lane`] gives them). From the pedal lane up, it's
    /// every note from there down, and the pedal.
    fn selection(&self, rows: Range<usize>, from: usize, to: usize) -> Selection {
        let pedal_lane = self.lanes.len();
        let (top, bottom) = (from.min(to), from.max(to));
        let pedal = bottom == pedal_lane;
        let values = if top == pedal_lane {
            // Just the pedal: no notes.
            Some(crate::selection::NO_VALUES)
        } else {
            let (a, b) = (self.lanes[top], self.lanes[bottom.min(pedal_lane - 1)]);
            Some(a.min(b)..=a.max(b))
        };
        Selection {
            rows,
            values,
            pedal,
        }
    }

    /// The row of the pedal lane under `p`, if it's over it.
    fn pedal_row(&self, p: Pos2) -> Option<usize> {
        let r = self.pedal_rect()?;
        let right = self.col_x(self.first_col + self.visible_cols());
        (r.y_range().contains(p.y) && (self.grid.left()..right).contains(&p.x))
            .then(|| self.cell_at(pos2(p.x, self.grid.top())).1)
    }

    fn visible_lanes(&self) -> usize {
        ((self.grid.height() / self.lane_h).floor() as usize).min(self.lanes.len())
    }

    fn visible_cols(&self) -> usize {
        ((self.grid.width() / self.col_w).floor() as usize).min(self.rows)
    }

    /// The lane index (into `lanes`) and row at a point in the grid,
    /// clamped to the visible cells.
    fn cell_at(&self, p: Pos2) -> (usize, usize) {
        let lane = ((p.y - self.grid.top()) / self.lane_h).floor().max(0.0) as usize;
        let col = ((p.x - self.grid.left()) / self.col_w).floor().max(0.0) as usize;
        (
            (self.first_lane + lane).min(self.first_lane + self.visible_lanes().max(1) - 1),
            (self.first_col + col).min(self.first_col + self.visible_cols().max(1) - 1),
        )
    }

    /// The lane and row of the cell under `p`, if it's over one.
    fn hovered(&self, p: Pos2) -> Option<(usize, usize)> {
        let lanes = self.visible_lanes() as f32 * self.lane_h;
        let cols = self.visible_cols() as f32 * self.col_w;
        let over_grid = Rect::from_min_size(self.grid.min, vec2(cols, lanes));
        over_grid.contains(p).then(|| self.cell_at(p))
    }

    /// Screen y of a lane's top, if it's visible.
    fn lane_y(&self, lane: usize) -> Option<f32> {
        (lane >= self.first_lane && lane < self.first_lane + self.visible_lanes())
            .then(|| self.grid.top() + (lane - self.first_lane) as f32 * self.lane_h)
    }

    fn col_x(&self, row: usize) -> f32 {
        self.grid.left() + (row as f32 - self.first_col as f32) * self.col_w
    }

    fn lane_of(&self, value: u16) -> Option<usize> {
        self.lanes.iter().position(|&v| v == value)
    }

    fn cell_rect(&self, lane: usize, row: usize) -> Option<Rect> {
        let y = self.lane_y(lane)?;
        let cols = self.first_col..self.first_col + self.visible_cols();
        cols.contains(&row).then(|| {
            Rect::from_min_size(pos2(self.col_x(row), y), vec2(self.col_w, self.lane_h)).shrink(1.0)
        })
    }
}

fn is_black_key(note: u16) -> bool {
    matches!(note % 12, 1 | 3 | 6 | 8 | 10)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// A hit's fill: brighter for louder.
fn hit_color(track: Color32, velocity: u8) -> Color32 {
    mix(CELL_A, track, 0.35 + 0.65 * velocity as f32 / 127.0)
}

impl StepView {
    /// Show clip `at` of `song`. `playhead` is the row of it playing, if
    /// any.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        song: &Song,
        at: ClipRef,
        playhead: Option<usize>,
    ) -> Output {
        let mut out = Output::default();
        let area = ui.max_rect();
        let small = FontId::proportional(13.0);
        let response = ui.allocate_rect(area, Sense::click_and_drag());
        let (Some(track), Some(clip)) = (song.tracks.get(at.0), song.clip(at)) else {
            return out;
        };
        let (instrument, poly) = (track.instrument, track.poly);
        let (synth, sample) = match song.instruments.get(instrument) {
            Some(Instrument::Synth(_)) => (true, None),
            Some(Instrument::Sampler(s)) => (false, Some(s.clone())),
            None => (false, None),
        };
        let cells = clip.cells.clone();
        let rpb = clip.rows_per_beat.max(1) as usize;
        let g = self.layout(at, &cells, area, synth, sample.as_deref());

        self.interact(
            ui,
            &response,
            at,
            &cells,
            &g,
            (synth, poly),
            instrument,
            &mut out,
        );
        // Show the gesture's latest step straight away.
        let cells = match &out.live {
            Some(Edit::SetCells(_, c)) => c.clone(),
            _ => out
                .edits
                .iter()
                .rev()
                .find_map(|e| match e {
                    Edit::SetCells(_, c) => Some(c.clone()),
                    _ => None,
                })
                .unwrap_or(cells),
        };

        let p = ui.painter_at(area);
        let color = color::TRACKS[at.0 % color::TRACKS.len()];
        let (lanes, cols) = (g.visible_lanes(), g.visible_cols());

        // Beat numbers.
        for row in (g.first_col..g.first_col + cols).filter(|r| r % rpb == 0) {
            let beat = row / rpb;
            p.text(
                pos2(g.col_x(row) + 2.0, g.grid.top() - 3.0),
                Align2::LEFT_BOTTOM,
                format!("{}", beat + 1),
                small.clone(),
                if beat.is_multiple_of(4) {
                    color::TEXT
                } else {
                    color::DIM
                },
            );
        }

        // Lane labels.
        let peaks = sample.as_ref().map(|s| self.peaks.get(s).to_vec());
        for lane in g.first_lane..g.first_lane + lanes {
            let y = g.lane_y(lane).unwrap();
            let v = g.lanes[lane];
            let r = Rect::from_min_size(pos2(g.labels.left(), y), vec2(LABEL_W - 6.0, g.lane_h))
                .shrink(1.0);
            if synth {
                let black = is_black_key(v);
                p.rect_filled(r, 2.0, if black { BLACK_KEY_A } else { CELL_B });
                p.text(
                    r.left_center() + vec2(6.0, 0.0),
                    Align2::LEFT_CENTER,
                    note_name(v as u8),
                    small.clone(),
                    if v.is_multiple_of(12) {
                        color::TEXT
                    } else {
                        color::MUTED
                    },
                );
            } else {
                p.rect_filled(r, 2.0, CELL_A);
                p.text(
                    r.left_center() + vec2(6.0, 0.0),
                    Align2::LEFT_CENTER,
                    format!("{v:02X}"),
                    small.clone(),
                    color,
                );
                if let Some(pk) = peaks.as_ref().and_then(|pk| pk.get(v as usize)) {
                    let glyph = Rect::from_min_max(
                        r.left_top() + vec2(30.0, 2.0),
                        r.right_bottom() - vec2(4.0, 2.0),
                    );
                    draw_peaks(&p, glyph, pk, false, color::MUTED);
                }
            }
        }

        // Cells.
        for lane in g.first_lane..g.first_lane + lanes {
            let v = g.lanes[lane];
            for row in g.first_col..g.first_col + cols {
                let r = g.cell_rect(lane, row).unwrap();
                let even = (row / rpb).is_multiple_of(2);
                let fill = if synth && is_black_key(v) {
                    if even { BLACK_KEY_A } else { BLACK_KEY_B }
                } else if even {
                    CELL_A
                } else {
                    CELL_B
                };
                p.rect_filled(r, 2.0, fill);
            }
            // Bar lines.
            for row in (g.first_col..g.first_col + cols).filter(|r| r % (rpb * 4) == 0 && *r > 0) {
                let x = g.col_x(row);
                let y = g.lane_y(lane).unwrap();
                p.line_segment(
                    [pos2(x, y), pos2(x, y + g.lane_h)],
                    Stroke::new(1.0, color::DIM),
                );
            }
        }

        // The lane and step under the pointer, lit up faintly across the
        // grid, so it's plain which note or beat it's on.
        if let Some((lane, row)) = ui
            .input(|i| i.pointer.hover_pos())
            .and_then(|q| g.hovered(q))
        {
            let glow = Color32::from_white_alpha(HOVER_ALPHA);
            if let Some(y) = g.lane_y(lane) {
                let band = Rect::from_min_max(
                    pos2(g.labels.left(), y),
                    pos2(g.col_x(g.first_col + cols), y + g.lane_h),
                );
                p.rect_filled(band, 2.0, glow);
            }
            let x = g.col_x(row);
            let band = Rect::from_min_max(
                pos2(x, g.grid.top() - HEADER),
                pos2(x + g.col_w, g.grid.top() + lanes as f32 * g.lane_h),
            );
            p.rect_filled(band, 2.0, glow);
        }

        // The pedal lane.
        if let Some(lane) = g.pedal_rect() {
            let label = Rect::from_min_size(lane.min, vec2(LABEL_W - 6.0, PEDAL_H)).shrink(1.0);
            p.rect_filled(label, 2.0, CELL_A);
            p.text(
                label.left_center() + vec2(6.0, 0.0),
                Align2::LEFT_CENTER,
                "pedal",
                small.clone(),
                color::MUTED,
            );
            let hover_row = ui
                .input(|i| i.pointer.hover_pos())
                .and_then(|q| g.pedal_row(q).or_else(|| g.hovered(q).map(|(_, r)| r)));
            let cell = |row: usize| {
                Rect::from_min_size(pos2(g.col_x(row), lane.top()), vec2(g.col_w, PEDAL_H))
                    .shrink(1.0)
            };
            for row in g.first_col..g.first_col + cols {
                let even = (row / rpb).is_multiple_of(2);
                p.rect_filled(cell(row), 2.0, if even { CELL_A } else { CELL_B });
                if hover_row == Some(row) {
                    p.rect_filled(cell(row), 2.0, Color32::from_white_alpha(HOVER_ALPHA));
                }
            }
            // Where it's down: a bar per press, broken at each change.
            let (down, change) = pedal_rows(&cells);
            let shown = g.first_col..g.first_col + cols;
            let mut row = 0;
            while row < down.len() {
                if !down[row] {
                    row += 1;
                    continue;
                }
                let start = row;
                row += 1;
                while row < down.len() && down[row] && !change[row] {
                    row += 1;
                }
                let (a, b) = (start.max(shown.start), (row - 1).min(shown.end - 1));
                if a <= b {
                    let bar = cell(a).union(cell(b)).shrink2(vec2(0.0, 3.0));
                    p.rect_filled(bar, 3.0, color.gamma_multiply(0.8));
                }
            }
        }

        // Hits.
        let above = |p: &egui::Painter, row: usize, up: bool| {
            // A hit out of view: a small arrow at the edge of its column.
            let x = g.col_x(row) + g.col_w / 2.0;
            let (y, d) = if up {
                (g.grid.top() + 2.0, 5.0)
            } else {
                (g.grid.top() + lanes as f32 * g.lane_h - 2.0, -5.0)
            };
            p.add(Shape::convex_polygon(
                vec![pos2(x, y), pos2(x + 4.0, y + d), pos2(x - 4.0, y + d)],
                color,
                Stroke::NONE,
            ));
        };
        if synth {
            for span in spans(&cells, poly) {
                let (row, end) = (span.start, span.end);
                if end < g.first_col || row >= g.first_col + cols {
                    continue;
                }
                let Some(t) = cells[row].note(span.note) else {
                    continue;
                };
                let (a, b) = (row.max(g.first_col), end.min(g.first_col + cols - 1));
                let chord = cells[row].fx().find_map(|f| match f {
                    Fx::Chord(n) => Some(n),
                    _ => None,
                });
                let mut notes = vec![(t.slice, true)];
                notes.extend(
                    chord
                        .into_iter()
                        .flat_map(Fx::chord_intervals)
                        .map(|i| (t.slice + i as u16, false)),
                );
                for (note, root) in notes {
                    let Some(lane) = g.lane_of(note) else {
                        continue;
                    };
                    if lane < g.first_lane || lane >= g.first_lane + lanes {
                        if root && (a..=b).contains(&row) {
                            above(&p, row, lane < g.first_lane);
                        }
                        continue;
                    }
                    let (Some(ra), Some(rb)) = (g.cell_rect(lane, a), g.cell_rect(lane, b)) else {
                        continue;
                    };
                    let bar = ra.union(rb);
                    let fill = hit_color(color, t.velocity);
                    let fill = if root { fill } else { mix(CELL_A, fill, 0.45) };
                    p.rect_filled(bar, 3.0, fill);
                    if row >= g.first_col {
                        let cursor = root && self.cursor == Some(row);
                        draw_hit_marks(&p, ra, &cells[row], None, cursor);
                    }
                }
            }
        } else {
            for (row, cell) in cells.iter().enumerate().skip(g.first_col).take(cols) {
                for t in cell.hits() {
                    let Some(lane) = g.lane_of(t.slice) else {
                        continue;
                    };
                    let Some(r) = g.cell_rect(lane, row) else {
                        above(&p, row, lane < g.first_lane);
                        continue;
                    };
                    let fill = hit_color(color, t.velocity);
                    let pk = peaks.as_ref().and_then(|pk| pk.get(t.slice as usize));
                    draw_hit(&p, r, cell, fill, pk, self.cursor == Some(row));
                }
            }
        }

        // The selection, over the hits so they show through.
        if let Some(sel) = self.selection(at) {
            let rows = sel.rows.start.max(g.first_col)..sel.rows.end.min(g.first_col + cols);
            let shown = g.first_lane..g.first_lane + lanes;
            let in_it: Vec<usize> = shown
                .filter(|&l| sel.values.as_ref().is_none_or(|v| v.contains(&g.lanes[l])))
                .collect();
            // From its top note shown to its bottom one, or on down over
            // the pedal.
            let notes = in_it.first().zip(in_it.last()).map(|(&top, &bottom)| {
                (g.lane_y(top).unwrap(), g.lane_y(bottom).unwrap() + g.lane_h)
            });
            let pedal = g
                .pedal_rect()
                .filter(|_| sel.has_pedal())
                .map(|r| (r.top(), r.bottom()));
            let span = match (notes, pedal) {
                (Some((top, _)), Some((_, bottom))) => Some((top, bottom)),
                (n, p) => n.or(p),
            };
            if let Some((top, bottom)) = span
                && !rows.is_empty()
            {
                let r = Rect::from_min_max(
                    pos2(g.col_x(rows.start), top),
                    pos2(g.col_x(rows.end), bottom),
                );
                p.rect_filled(r, 2.0, color::ACCENT.gamma_multiply(0.12));
                p.rect_stroke(r, 2.0, Stroke::new(1.5, color::ACCENT), StrokeKind::Inside);
            }
        }

        // Playhead.
        if let Some(row) = playhead.filter(|r| (g.first_col..g.first_col + cols).contains(r)) {
            let r = Rect::from_min_size(
                pos2(g.col_x(row), g.grid.top()),
                vec2(g.col_w, lanes as f32 * g.lane_h),
            );
            p.rect_filled(r, 0.0, Color32::from_white_alpha(22));
        }
        out
    }

    fn layout(
        &mut self,
        at: ClipRef,
        cells: &[Cell],
        area: Rect,
        synth: bool,
        sample: Option<&Sample>,
    ) -> Geometry {
        let top = area.top() + HEADER;
        let labels = Rect::from_min_max(
            pos2(area.left() + 8.0, top),
            pos2(area.left() + 8.0 + LABEL_W, area.bottom() - 6.0),
        );
        let pedal = if synth { PEDAL_H + PEDAL_GAP } else { 0.0 };
        let grid = Rect::from_min_max(
            pos2(labels.right(), top),
            pos2(area.right() - 8.0, area.bottom() - 6.0 - pedal),
        );

        let lanes: Vec<u16> = if synth {
            (LOWEST_NOTE as u16..=HIGHEST_NOTE as u16).rev().collect()
        } else {
            (0..sample.map_or(1, |s| s.slices.len().max(1)) as u16).collect()
        };
        let lane_h = if synth {
            NOTE_LANE
        } else {
            (grid.height() / lanes.len() as f32).clamp(MIN_LANE, MAX_LANE)
        };
        let fit = ((grid.height() / lane_h).floor() as usize).max(1);
        // Another clip starts scrolled to its notes, and its first row.
        if self.scrolled_for != Some((at, synth)) {
            self.scrolled_for = Some((at, synth));
            self.lane_scroll = 0;
            self.col_scroll = 0;
            self.cursor = None;
            self.gesture = None;
            self.centre_lane = None;
            if synth {
                let mut notes: Vec<u16> = cells
                    .iter()
                    .flat_map(|c| c.hits().map(|t| t.slice))
                    .collect();
                notes.sort_unstable();
                let centre = notes.get(notes.len() / 2).copied().unwrap_or(HOME_NOTE);
                self.centre_lane = lanes.iter().position(|&v| v == centre);
            }
        }
        if let Some(lane) = self.centre_lane {
            self.lane_scroll = lane.saturating_sub(fit / 2);
        }
        self.lane_scroll = self.lane_scroll.min(lanes.len().saturating_sub(fit));

        let rows = cells.len().max(1);
        let col_w = (grid.width() / rows as f32).clamp(MIN_COL, MAX_COL);
        let visible = ((grid.width() / col_w).floor() as usize).clamp(1, rows);
        // Keep the row last touched in view.
        if let Some(row) = self.cursor {
            if row < self.col_scroll {
                self.col_scroll = row;
            } else if row >= self.col_scroll + visible {
                self.col_scroll = row + 1 - visible;
            }
        }
        self.col_scroll = self.col_scroll.min(rows - visible);

        Geometry {
            labels,
            grid,
            lanes,
            first_lane: self.lane_scroll,
            lane_h,
            first_col: self.col_scroll,
            col_w,
            rows,
            pedal: synth,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn interact(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        at: ClipRef,
        cells: &[Cell],
        g: &Geometry,
        (synth, poly): (bool, bool),
        instrument: usize,
        out: &mut Output,
    ) {
        let (pressed, down, hover, shift) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.hover_pos(),
                i.modifiers.shift,
            )
        });
        let in_area = response.contains_pointer();
        let in_grid = |p: Pos2| {
            let h = g.visible_lanes() as f32 * g.lane_h;
            let w = g.visible_cols() as f32 * g.col_w;
            Rect::from_min_size(g.grid.min, vec2(w, h)).contains(p)
        };
        self.hover_row = hover
            .filter(|&p| in_area && in_grid(p))
            .map(|p| (at, g.cell_at(p).1));
        let end_wheel = |this: &mut Self, out: &mut Output| {
            if std::mem::take(&mut this.wheeling) {
                out.end_first = true;
            }
        };

        // Pressing: labels play their lane, the grid starts a gesture.
        if pressed
            && in_area
            && let Some(p) = hover
        {
            end_wheel(self, out);
            let beats = Rect::from_min_max(
                pos2(g.grid.left(), g.grid.top() - HEADER),
                pos2(
                    g.grid.left() + g.visible_cols() as f32 * g.col_w,
                    g.grid.top(),
                ),
            );
            if let Some(row) = g.pedal_row(p).filter(|_| !shift) {
                self.selection = None;
                let down = !pedal_rows(cells).0[row];
                self.gesture = Some(Gesture {
                    clip: at,
                    base: cells.to_vec(),
                    kind: Kind::Pedal {
                        from: row,
                        to: row,
                        down,
                    },
                });
            } else if g.labels.contains(p) {
                let lane = g.cell_at(p).0;
                if g.lane_y(lane).is_some_and(|y| p.y < y + g.lane_h) {
                    out.audition = Some((instrument, g.lanes[lane]));
                }
            } else if beats.contains(p) || (shift && (in_grid(p) || g.pedal_row(p).is_some())) {
                // Selecting, along the beat numbers or with shift.
                let row = g.cell_at(p).1;
                self.selecting = Some((at, g.select_lane(p), row, beats.contains(p)));
            } else if in_grid(p) {
                self.selection = None;
                let (lane, row) = g.cell_at(p);
                let v = g.lanes[lane];
                let kind = if cells[row].note(v).is_some() {
                    Kind::Erase(vec![(v, row)])
                } else if synth {
                    match hit_row(cells, true, poly, v, row) {
                        // Inside a note: move its end.
                        Some(start) if start < row => Kind::Note {
                            note: v,
                            start,
                            end: row - 1,
                            velocity: cells[start].note(v).map_or(self.velocity, |t| t.velocity),
                            poly,
                        },
                        _ => {
                            out.audition = Some((instrument, v));
                            Kind::Note {
                                note: v,
                                start: row,
                                end: row,
                                velocity: self.velocity,
                                poly,
                            }
                        }
                    }
                } else {
                    out.audition = Some((instrument, v));
                    Kind::Paint(vec![(v, row)])
                };
                self.cursor = Some(row);
                self.gesture = Some(Gesture {
                    clip: at,
                    base: cells.to_vec(),
                    kind,
                });
            }
        }

        // Dragging out a selection.
        if let Some((clip, from_lane, from_row, whole)) = self.selecting {
            if clip != at || !down {
                self.selecting = None;
            } else if let Some(p) = hover {
                let row = g.cell_at(p).1;
                let rows = from_row.min(row)..from_row.max(row) + 1;
                let sel = if whole {
                    Selection {
                        rows,
                        values: None,
                        pedal: false,
                    }
                } else {
                    g.selection(rows, from_lane, g.select_lane(p))
                };
                self.selection = Some((at, sel));
            }
        }

        // Dragging.
        if let Some(gs) = &mut self.gesture
            && gs.clip == at
        {
            if down && let Some(p) = hover {
                let (lane, row) = g.cell_at(p);
                let v = g.lanes[lane];
                match &mut gs.kind {
                    Kind::Pedal { to, .. } => *to = row,
                    Kind::Paint(cells_at) => {
                        if cells_at.last() != Some(&(v, row)) {
                            cells_at.push((v, row));
                            out.audition = Some((instrument, v));
                        }
                    }
                    Kind::Erase(cells_at) => {
                        if cells_at.last() != Some(&(v, row)) {
                            cells_at.push((v, row));
                        }
                    }
                    Kind::Note { start, end, .. } => *end = row.max(*start),
                }
                if !matches!(gs.kind, Kind::Note { .. } | Kind::Pedal { .. }) {
                    self.cursor = Some(row);
                }
            }
            let cells_now = apply(&gs.base, &gs.kind, self.velocity);
            if cells_now != gs.base || !down {
                out.live = Some(Edit::SetCells(at, cells_now));
            }
            if !down {
                self.gesture = None;
                out.gesture_done = true;
            }
        } else if !down {
            self.gesture = None;
        }

        // Right-click where the pedal's down for a pedal change there.
        if response.secondary_clicked()
            && let Some(row) = hover.and_then(|p| g.pedal_row(p))
        {
            let (down, mut change) = pedal_rows(cells);
            if row > 0 && down[row] && down[row - 1] {
                end_wheel(self, out);
                change[row] = !change[row];
                out.edits
                    .push(Edit::SetCells(at, with_pedal_rows(cells, &down, &change)));
            }
        }

        // Right-click a hit to roll it.
        if response.secondary_clicked()
            && let Some(p) = hover.filter(|&p| in_grid(p))
        {
            let (lane, row) = g.cell_at(p);
            if let Some(start) = hit_row(cells, synth, poly, g.lanes[lane], row) {
                end_wheel(self, out);
                let mut rolled = cells.to_vec();
                rolled[start] = next_roll(rolled[start]);
                self.cursor = Some(start);
                out.edits.push(Edit::SetCells(at, rolled));
            }
        }

        // The wheel: velocity over a hit, other lanes elsewhere.
        if in_area
            && self.gesture.is_none()
            && let Some(p) = hover
        {
            let hit = in_grid(p)
                .then(|| {
                    let (lane, row) = g.cell_at(p);
                    let v = g.lanes[lane];
                    hit_row(cells, synth, poly, v, row).map(|r| (r, v))
                })
                .flatten();
            if let Some((row, note)) = hit {
                let vel = cells[row].note(note).unwrap().velocity;
                let (delta, now) = ui.input(|i| (knob::wheel_delta(i), i.time));
                if let Some((v, first)) =
                    self.wheel
                        .turn((at, row), &VELOCITY, vel as f32, delta, shift, now)
                {
                    let velocity = v.round().clamp(1.0, 127.0) as u8;
                    if velocity != vel {
                        if first {
                            end_wheel(self, out);
                        }
                        let mut turned = cells.to_vec();
                        if let Some(t) = turned[row].note_mut(note) {
                            t.velocity = velocity;
                        }
                        self.cursor = Some(row);
                        self.velocity = velocity;
                        self.wheeling = true;
                        out.live = Some(Edit::SetCells(at, turned));
                    }
                }
            } else {
                let dy = ui.input(|i| i.smooth_scroll_delta.y);
                if dy != 0.0 {
                    let lanes = (-dy / g.lane_h).round() as isize;
                    self.lane_scroll = (self.lane_scroll as isize + lanes).max(0) as usize;
                    self.centre_lane = None;
                }
            }
        }
    }

    /// The selection in clip `at`, if there is one.
    pub fn selection(&self, at: ClipRef) -> Option<&Selection> {
        self.selection
            .as_ref()
            .filter(|(c, _)| *c == at)
            .map(|(_, s)| s)
    }

    /// Select every step of clip `at`.
    pub fn select_all(&mut self, song: &Song, at: ClipRef) {
        if let Some(clip) = song.clip(at) {
            let all = Selection {
                rows: 0..clip.cells.len(),
                values: None,
                pedal: false,
            };
            self.selection = Some((at, all));
        }
    }

    /// Forget the selection. Returns whether there was one.
    pub fn clear_selection(&mut self) -> bool {
        self.selecting = None;
        self.selection.take().is_some()
    }

    /// The selected steps of clip `at`.
    pub fn copy(&self, song: &Song, at: ClipRef) -> Option<Steps> {
        let sel = self.selection(at)?;
        let clip = song.clip(at)?;
        Some(selection::copy(&clip.cells, sel, is_synth(song, at)))
    }

    /// Clear the selected steps of clip `at`: the edit that does it.
    pub fn delete(&self, song: &Song, at: ClipRef) -> Option<Edit> {
        let sel = self.selection(at)?;
        let clip = song.clip(at)?;
        Some(Edit::SetCells(at, selection::clear(&clip.cells, sel)))
    }

    /// Paste `steps` into clip `at`: at the step under the pointer, else
    /// the last one touched, else the selection's start. What's pasted is
    /// selected.
    pub fn paste(&mut self, song: &Song, at: ClipRef, steps: &Steps) -> Result<Edit, String> {
        let clip = song.clip(at).ok_or("no clip open")?;
        if steps.synth != is_synth(song, at) {
            return Err(if steps.synth {
                "can't paste notes into a sample clip".into()
            } else {
                "can't paste slices into a synth clip".into()
            });
        }
        let row = self
            .hover_row
            .filter(|(c, _)| *c == at)
            .map(|(_, r)| r)
            .or(self.cursor)
            .or_else(|| self.selection(at).map(|s| s.rows.start))
            .unwrap_or(0);
        let (cells, filled) = selection::paste(&clip.cells, steps, row);
        self.cursor = Some(row);
        self.selection = Some((at, filled));
        Ok(Edit::SetCells(at, cells))
    }

    /// Whether a run of wheel turns is waiting to be made one undo step.
    #[cfg(test)]
    fn wheeling(&self) -> bool {
        self.wheeling
    }

    /// The window made the open wheel gesture an undo step.
    pub fn end_wheel(&mut self) {
        self.wheeling = false;
    }
}

fn draw_peaks(p: &egui::Painter, r: Rect, peaks: &[f32; PEAKS], reverse: bool, color: Color32) {
    if r.width() < 10.0 || r.height() < 6.0 {
        return;
    }
    let step = r.width() / PEAKS as f32;
    let width = (step * 0.6).max(1.0);
    for k in 0..PEAKS {
        let v = if reverse {
            peaks[PEAKS - 1 - k]
        } else {
            peaks[k]
        };
        let x = r.left() + (k as f32 + 0.5) * step;
        let half = (0.1 + 0.9 * v) * r.height() / 2.0;
        p.line_segment(
            [pos2(x, r.center().y - half), pos2(x, r.center().y + half)],
            Stroke::new(width, color),
        );
    }
}

/// A sample hit: its slice's waveform, split up for rolls, mirrored when
/// reversed.
fn draw_hit(
    p: &egui::Painter,
    r: Rect,
    cell: &Cell,
    fill: Color32,
    peaks: Option<&[f32; PEAKS]>,
    cursor: bool,
) {
    let rolls = cell
        .fx()
        .find_map(|f| match f {
            Fx::Retrig(n) if n > 1 => Some(n.min(8) as usize),
            _ => None,
        })
        .unwrap_or(1);
    let reverse = cell.fx().any(|f| f == Fx::Reverse);
    let w = r.width() / rolls as f32;
    for i in 0..rolls {
        let part = Rect::from_min_size(pos2(r.left() + i as f32 * w, r.top()), vec2(w, r.height()));
        let part = if rolls > 1 {
            part.shrink2(vec2(0.5, 0.0))
        } else {
            part
        };
        p.rect_filled(part, 2.0, fill);
        if let Some(pk) = peaks {
            draw_peaks(p, part.shrink2(vec2(2.0, 3.0)), pk, reverse, GLYPH);
        }
    }
    draw_hit_marks(p, r, cell, Some(reverse), cursor);
}

/// Marks on a hit: reversed, other effects, the cursor.
fn draw_hit_marks(p: &egui::Painter, r: Rect, cell: &Cell, reverse: Option<bool>, cursor: bool) {
    if reverse.unwrap_or_else(|| cell.fx().any(|f| f == Fx::Reverse)) {
        let (x, y) = (r.left() + 2.0, r.top() + 2.0);
        p.add(Shape::convex_polygon(
            vec![pos2(x, y + 3.0), pos2(x + 5.0, y), pos2(x + 5.0, y + 6.0)],
            GLYPH,
            Stroke::NONE,
        ));
    }
    let other = cell.fx().any(|f| {
        !matches!(
            f,
            Fx::Retrig(_) | Fx::Reverse | Fx::Cut(_) | Fx::Chord(_) | Fx::Pedal(_)
        )
    });
    if other {
        p.circle_filled(r.right_top() + vec2(-3.5, 3.5), 2.2, color::ACCENT);
    }
    if cursor {
        p.rect_stroke(
            r.expand(1.0),
            3.0,
            Stroke::new(1.5, color::TEXT),
            StrokeKind::Outside,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song::{PEDAL_CHANGE, PEDAL_DOWN, PEDAL_UP};

    fn cells(n: usize) -> Vec<Cell> {
        vec![Cell::default(); n]
    }

    fn slices(c: &Cell) -> Vec<u16> {
        c.hits().map(|t| t.slice).collect()
    }

    #[test]
    fn only_a_visible_cell_counts_as_hovered() {
        // 4 lanes of 10 shown out of 8, from the second; 5 rows of 20 shown
        // out of 16, from the third.
        let g = Geometry {
            labels: Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 40.0)),
            grid: Rect::from_min_size(pos2(100.0, 0.0), vec2(100.0, 40.0)),
            lanes: (0..8).collect(),
            first_lane: 1,
            lane_h: 10.0,
            first_col: 2,
            col_w: 20.0,
            rows: 16,
            pedal: true,
        };
        assert_eq!(g.hovered(pos2(105.0, 5.0)), Some((1, 2)));
        assert_eq!(g.hovered(pos2(195.0, 35.0)), Some((4, 6)));
        assert_eq!(g.hovered(pos2(50.0, 5.0)), None, "a lane's name");
        assert_eq!(g.hovered(pos2(105.0, 45.0)), None, "below the lanes");
        assert_eq!(g.hovered(pos2(205.0, 5.0)), None, "past the rows");
        // The pedal lane: under the 4 lanes shown, after a gap.
        let y = 40.0 + PEDAL_GAP + PEDAL_H / 2.0;
        assert_eq!(g.pedal_row(pos2(105.0, y)), Some(2));
        assert_eq!(g.pedal_row(pos2(50.0, y)), None, "its name");
        assert_eq!(g.pedal_row(pos2(105.0, 35.0)), None, "the lanes");
    }

    fn pedals(cells: &[Cell]) -> Vec<Option<u8>> {
        cells.iter().map(Cell::pedal).collect()
    }

    #[test]
    fn dragging_along_the_pedal_lane_puts_it_down_and_up() {
        let base = cells(8);
        let down = |from, to| Kind::Pedal {
            from,
            to,
            down: true,
        };
        let c = apply(&base, &down(5, 2), 100);
        let d = PEDAL_DOWN;
        assert_eq!(
            pedals(&c),
            [None, None, Some(d), None, None, None, Some(PEDAL_UP), None]
        );
        // Up in the middle splits it in two.
        let up = Kind::Pedal {
            from: 3,
            to: 3,
            down: false,
        };
        let c = apply(&c, &up, 100);
        assert_eq!(
            pedals(&c),
            [
                None,
                None,
                Some(d),
                Some(PEDAL_UP),
                Some(d),
                None,
                Some(PEDAL_UP),
                None
            ]
        );
        // Down again over the gap joins them, and down to the end needs no up.
        let c = apply(&c, &down(3, 7), 100);
        assert_eq!(
            pedals(&c),
            [None, None, Some(d), None, None, None, None, None]
        );
    }

    #[test]
    fn a_pedal_change_survives_and_full_effect_slots_make_room() {
        let mut base = cells(6);
        base[3] = Cell::hit(60, 100)
            .with_fx(Fx::Glide(4))
            .with_fx(Fx::Chord(0x47));
        let (mut down, mut change) = pedal_rows(&base);
        down[1..5].fill(true);
        change[3] = true;
        let c = with_pedal_rows(&base, &down, &change);
        assert_eq!(
            pedals(&c),
            [
                None,
                Some(PEDAL_DOWN),
                None,
                Some(PEDAL_CHANGE),
                None,
                Some(PEDAL_UP)
            ]
        );
        assert_eq!(c[3].fx[0], Some(Fx::Glide(4)), "kept the first");
        assert_eq!(pedal_rows(&c), (down, change), "reads back the same");
    }

    #[test]
    fn painting_adds_to_the_row_and_erasing_takes_one_out() {
        let base = cells(8);
        let out = apply(&base, &Kind::Paint(vec![(2, 0), (2, 1), (3, 1)]), 100);
        assert_eq!(slices(&out[0]), [2]);
        assert_eq!(slices(&out[1]), [2, 3], "both in the row");
        assert_eq!(out[0].first().unwrap().velocity, 100);
        let out2 = apply(&out, &Kind::Erase(vec![(2, 0), (2, 1)]), 100);
        assert_eq!(out2[0], Cell::default());
        assert_eq!(slices(&out2[1]), [3], "lane 3 stays");
        // A full row takes no more.
        let full = apply(&base, &Kind::Paint((0..6).map(|v| (v, 0)).collect()), 100);
        assert_eq!(full[0].hits().count(), crate::song::MAX_NOTES);
    }

    fn span(note: u16, start: usize, end: usize) -> Span {
        Span { note, start, end }
    }

    #[test]
    fn mono_notes_last_until_the_next_notes_or_a_cut() {
        let mut c = cells(8);
        c[1] = Cell::hit(60, 100).with_note(Trigger::new(64, 100));
        c[4] = Cell::default().with_fx(Fx::Cut(0));
        c[6] = Cell::hit(62, 100);
        assert_eq!(
            spans(&c, false),
            [span(60, 1, 3), span(64, 1, 3), span(62, 6, 7)]
        );
    }

    #[test]
    fn poly_notes_last_their_own_lengths() {
        let mut c = cells(8);
        let note = |slice, rows| Trigger {
            slice,
            velocity: 100,
            rows,
        };
        c[0] = Cell::default()
            .with_note(note(60, 6))
            .with_note(note(64, 2));
        c[2] = Cell::hit(67, 100);
        c[5] = Cell::default().with_fx(Fx::Cut(0));
        assert_eq!(
            spans(&c, true),
            [span(60, 0, 4), span(64, 0, 1), span(67, 2, 4)],
            "the cut ends the C and the G; the E ends first"
        );
        assert_eq!(hit_row(&c, true, true, 60, 3), Some(0));
        assert_eq!(hit_row(&c, true, true, 64, 3), None);
    }

    #[test]
    fn a_mono_note_gesture_sets_the_rows_length() {
        let base = cells(8);
        let kind = |end| Kind::Note {
            note: 60,
            start: 1,
            end,
            velocity: 90,
            poly: false,
        };
        let short = apply(&base, &kind(1), 0);
        assert_eq!(spans(&short, false), [span(60, 1, 1)]);
        assert!(has_cut(&short[2]));
        let long = apply(&short, &kind(4), 0);
        assert_eq!(spans(&long, false), [span(60, 1, 4)]);
        assert!(!has_cut(&long[2]), "the old end is gone");
        // To the end of the clip: nothing to cut.
        let full = apply(&base, &kind(7), 0);
        assert!(full.iter().all(|c| !has_cut(c)));
        // A second note in the row joins the first, and they end together.
        let e = Kind::Note {
            note: 64,
            start: 1,
            end: 2,
            velocity: 90,
            poly: false,
        };
        let both = apply(&long, &e, 0);
        assert_eq!(spans(&both, false), [span(60, 1, 2), span(64, 1, 2)]);
    }

    #[test]
    fn a_poly_note_gesture_sets_only_its_own_length() {
        let base = cells(8);
        let kind = |note, end| Kind::Note {
            note,
            start: 1,
            end,
            velocity: 90,
            poly: true,
        };
        let one = apply(&base, &kind(60, 5), 0);
        let two = apply(&one, &kind(64, 2), 0);
        assert_eq!(spans(&two, true), [span(60, 1, 5), span(64, 1, 2)]);
        assert!(two.iter().all(|c| !has_cut(c)), "no cuts needed");
    }

    mod mouse {
        use super::*;
        use crate::song::{Clip, Track};
        use crate::synthvoice::{SynthKind, SynthPatch};
        use crate::ui::edit;
        use eframe::egui::{Event, Modifiers, PointerButton, RawInput};

        /// The clip editor, with its edits made the way the window makes
        /// them: live ones on the song as it was when the gesture began.
        struct Harness {
            ctx: egui::Context,
            view: StepView,
            song: Song,
            gesture: Option<Song>,
            undo: Vec<Song>,
            auditions: Vec<(usize, u16)>,
            /// Shift held for the next frames.
            shift: bool,
        }

        const SCREEN: Rect = Rect::from_min_max(Pos2::ZERO, pos2(1400.0, 800.0));

        impl Harness {
            fn new(synth: bool) -> Self {
                let sample = Sample::new("s", 48_000, vec![[0.5; 2]; 1000])
                    .with_slices(vec![0, 250, 500, 750]);
                let instrument = if synth {
                    Instrument::Synth(Arc::new(SynthPatch::new(SynthKind::Acid, "acid")))
                } else {
                    Instrument::Sampler(Arc::new(sample))
                };
                let track = Track::new("a", 0).with_clip(Clip::beats(0, 4, 4));
                let mut h = Harness {
                    ctx: egui::Context::default(),
                    view: StepView::default(),
                    song: Song::new(120.0, vec![instrument], vec![track]),
                    gesture: None,
                    undo: Vec::new(),
                    auditions: Vec::new(),
                    shift: false,
                };
                h.frame(vec![]);
                h
            }

            /// A point on the beat numbers, over `row`.
            fn beat(&mut self, row: usize) -> Pos2 {
                let cells = self.song.tracks[0].clips[0].cells.clone();
                let synth = self.song.instruments[0].is_synth();
                let sample = match &self.song.instruments[0] {
                    Instrument::Sampler(s) => Some(s.clone()),
                    Instrument::Synth(_) => None,
                };
                let g = self
                    .view
                    .layout((0, 0), &cells, SCREEN, synth, sample.as_deref());
                pos2(g.col_x(row) + g.col_w / 2.0, g.grid.top() - HEADER / 2.0)
            }

            fn frame(&mut self, events: Vec<Event>) {
                let modifiers = if self.shift {
                    Modifiers::SHIFT
                } else {
                    Modifiers::NONE
                };
                let mut events = events;
                events.insert(0, Event::ModifiersChanged(modifiers));
                let input = RawInput {
                    screen_rect: Some(SCREEN),
                    events,
                    ..Default::default()
                };
                let (view, song) = (&mut self.view, &self.song);
                let mut out = None;
                let mut full = self.ctx.run_ui(input, |ui| {
                    out = Some(view.show(ui, song, (0, 0), None));
                });
                full.textures_delta.clear();
                let out = out.unwrap();
                self.auditions.extend(out.audition);
                if out.end_first {
                    self.end();
                }
                for e in &out.edits {
                    self.end();
                    let before = self.song.clone();
                    edit::apply(&mut self.song, e).unwrap();
                    self.undo.push(before);
                }
                if let Some(e) = &out.live {
                    let before = self.gesture.get_or_insert_with(|| self.song.clone());
                    let mut song = before.clone();
                    edit::apply(&mut song, e).unwrap();
                    self.song = song;
                }
                if out.gesture_done {
                    self.end();
                }
            }

            fn end(&mut self) {
                if let Some(before) = self.gesture.take() {
                    self.undo.push(before);
                }
            }

            fn undo(&mut self) {
                self.end();
                self.song = self.undo.pop().expect("something to undo");
            }

            /// The centre of the cell for lane value `v` in `row`.
            fn at(&mut self, v: u16, row: usize) -> Pos2 {
                let cells = self.song.tracks[0].clips[0].cells.clone();
                let synth = self.song.instruments[0].is_synth();
                let sample = match &self.song.instruments[0] {
                    Instrument::Sampler(s) => Some(s.clone()),
                    Instrument::Synth(_) => None,
                };
                let g = self
                    .view
                    .layout((0, 0), &cells, SCREEN, synth, sample.as_deref());
                let lane = g.lane_of(v).unwrap();
                g.cell_rect(lane, row).expect("cell in view").center()
            }

            fn button(&mut self, pos: Pos2, pressed: bool) {
                self.frame(vec![Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: if self.shift {
                        Modifiers::SHIFT
                    } else {
                        Modifiers::NONE
                    },
                }]);
            }

            fn drag(&mut self, path: &[Pos2]) {
                self.frame(vec![Event::PointerMoved(path[0])]);
                self.button(path[0], true);
                for &p in &path[1..] {
                    self.frame(vec![Event::PointerMoved(p)]);
                }
                self.button(*path.last().unwrap(), false);
                self.frame(vec![]);
            }

            /// The middle of the pedal lane over `row`.
            fn pedal(&mut self, row: usize) -> Pos2 {
                let cells = self.song.tracks[0].clips[0].cells.clone();
                let g = self.view.layout((0, 0), &cells, SCREEN, true, None);
                pos2(
                    g.col_x(row) + g.col_w / 2.0,
                    g.pedal_rect().expect("a pedal lane").center().y,
                )
            }

            fn right_click(&mut self, pos: Pos2) {
                for pressed in [true, false] {
                    self.frame(vec![Event::PointerButton {
                        pos,
                        button: PointerButton::Secondary,
                        pressed,
                        modifiers: Modifiers::NONE,
                    }]);
                }
            }

            fn cells(&self) -> &[Cell] {
                &self.song.tracks[0].clips[0].cells
            }

            fn triggers(&self) -> Vec<Option<u16>> {
                self.cells()
                    .iter()
                    .map(|c| c.first().map(|t| t.slice))
                    .collect()
            }
        }

        #[test]
        fn shift_drag_selects_lanes_and_steps_without_painting() {
            let mut h = Harness::new(false);
            let (from, to) = (h.at(1, 2), h.at(2, 5));
            h.shift = true;
            h.drag(&[from, to]);
            h.shift = false;
            assert!(h.cells().iter().all(|c| !c.has_hit()), "nothing painted");
            assert_eq!(
                h.view.selection((0, 0)),
                Some(&Selection {
                    rows: 2..6,
                    values: Some(1..=2),
                    pedal: false,
                })
            );
            // A plain click paints again, and lets go of the selection.
            let at = h.at(0, 0);
            h.drag(&[at]);
            assert!(h.cells()[0].has_hit());
            assert_eq!(h.view.selection((0, 0)), None);
        }

        #[test]
        fn dragging_along_the_beat_numbers_selects_whole_steps() {
            let mut h = Harness::new(false);
            let (from, to) = (h.beat(6), h.beat(3));
            h.drag(&[from, to]);
            assert_eq!(
                h.view.selection((0, 0)),
                Some(&Selection {
                    rows: 3..7,
                    values: None,
                    pedal: false,
                })
            );
        }

        #[test]
        fn steps_paste_where_the_pointer_is() {
            let mut h = Harness::new(false);
            for (v, row) in [(1, 0), (2, 1), (3, 1)] {
                let at = h.at(v, row);
                h.drag(&[at]);
            }
            h.view.selection = Some((
                (0, 0),
                Selection {
                    rows: 0..2,
                    values: Some(1..=2),
                    pedal: false,
                },
            ));
            let steps = h.view.copy(&h.song, (0, 0)).unwrap();
            assert_eq!(steps.cells.len(), 2);
            let over = h.at(0, 8);
            h.frame(vec![Event::PointerMoved(over)]);
            let edit = h.view.paste(&h.song, (0, 0), &steps).unwrap();
            edit::apply(&mut h.song, &edit).unwrap();
            assert_eq!(h.triggers()[8], Some(1));
            let row9: Vec<u16> = h.cells()[9].hits().map(|t| t.slice).collect();
            assert_eq!(row9, [2], "slice 3 wasn't selected");
            assert_eq!(
                h.view.selection((0, 0)).unwrap().rows,
                8..10,
                "what's pasted is selected"
            );
            // Not into a synth clip.
            let synth = Harness::new(true);
            assert!(h.view.paste(&synth.song, (0, 0), &steps).is_err());
            // Clearing takes the selected notes out.
            let edit = h.view.delete(&h.song, (0, 0)).unwrap();
            edit::apply(&mut h.song, &edit).unwrap();
            assert!(!h.cells()[8].has_hit() && !h.cells()[9].has_hit());
            assert!(h.cells()[1].has_hit(), "the original stays");
        }

        #[test]
        fn click_adds_drag_paints_and_one_undo_takes_it_back() {
            let mut h = Harness::new(false);
            let a = h.at(2, 0);
            h.drag(&[a]);
            assert_eq!(h.triggers()[0], Some(2));
            assert_eq!(h.auditions, vec![(0, 2)], "you hear what you put in");
            let path = [h.at(1, 4), h.at(1, 5), h.at(1, 6)];
            h.drag(&path);
            assert_eq!(&h.triggers()[4..7], &[Some(1); 3]);
            assert_eq!(h.view.cursor, Some(6), "the cursor follows the paint");
            h.undo();
            assert_eq!(&h.triggers()[4..7], &[None; 3], "one gesture, one undo");
            assert_eq!(h.triggers()[0], Some(2));
            // Clicking a hit removes it.
            let a = h.at(2, 0);
            h.drag(&[a]);
            assert_eq!(h.triggers()[0], None);
        }

        #[test]
        fn wheel_over_a_hit_turns_its_velocity_as_one_undo_step() {
            let mut h = Harness::new(false);
            let a = h.at(3, 2);
            h.drag(&[a]);
            let undo = h.undo.len();
            let before = h.cells()[2].first().unwrap().velocity;
            let wheel = || Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: vec2(0.0, -2.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: Modifiers::NONE,
            };
            h.frame(vec![wheel()]);
            h.frame(vec![wheel()]);
            let after = h.cells()[2].first().unwrap().velocity;
            assert!(after < before, "{before} -> {after}");
            assert!(h.view.wheeling());
            h.undo();
            assert_eq!(h.undo.len(), undo, "both turns in one step");
            assert_eq!(h.cells()[2].first().unwrap().velocity, before);
        }

        #[test]
        fn dragging_a_new_synth_note_makes_it_longer() {
            let mut h = Harness::new(true);
            let path = [h.at(60, 2), h.at(60, 3), h.at(62, 5)];
            h.drag(&path);
            assert_eq!(spans(h.cells(), false), [span(60, 2, 5)]);
            // Drag inside it to move its end back.
            let path = [h.at(60, 4), h.at(60, 3)];
            h.drag(&path);
            assert_eq!(spans(h.cells(), false), [span(60, 2, 3)]);
        }

        #[test]
        fn a_second_note_on_a_step_joins_the_first() {
            // C3, then E3, on the same step of an acid line.
            let mut h = Harness::new(true);
            let c = h.at(48, 0);
            h.drag(&[c]);
            let e = h.at(52, 0);
            h.drag(&[e]);
            assert_eq!(slices(&h.cells()[0]), [48, 52]);
            assert_eq!(spans(h.cells(), false), [span(48, 0, 0), span(52, 0, 0)]);
            // Clicking one takes just that one out.
            let c = h.at(48, 0);
            h.drag(&[c]);
            assert_eq!(slices(&h.cells()[0]), [52]);
        }

        #[test]
        fn hits_share_a_step_on_a_sample_track() {
            let mut h = Harness::new(false);
            let (kick, hat) = (h.at(0, 4), h.at(2, 4));
            h.drag(&[kick]);
            h.drag(&[hat]);
            assert_eq!(slices(&h.cells()[4]), [0, 2]);
        }

        #[test]
        fn poly_notes_get_their_own_lengths() {
            let mut h = Harness::new(true);
            h.song.tracks[0].poly = true;
            let path = [h.at(60, 2), h.at(60, 3), h.at(60, 5)];
            h.drag(&path);
            let e = h.at(64, 2);
            h.drag(&[e]);
            assert_eq!(spans(h.cells(), true), [span(60, 2, 5), span(64, 2, 2)]);
            assert!(h.cells().iter().all(|c| !has_cut(c)));
        }

        #[test]
        fn the_pedal_lane_puts_the_pedal_down_and_changes_it() {
            let mut h = Harness::new(true);
            let pedals = |h: &Harness| h.cells().iter().map(Cell::pedal).collect::<Vec<_>>();
            let (a, b) = (h.pedal(2), h.pedal(9));
            h.drag(&[a, b]);
            let mut want = vec![None; 16];
            want[2] = Some(PEDAL_DOWN);
            want[10] = Some(PEDAL_UP);
            assert_eq!(pedals(&h), want);
            assert!(h.triggers().iter().all(Option::is_none), "no notes");
            let c = h.pedal(6);
            h.right_click(c);
            want[6] = Some(PEDAL_CHANGE);
            assert_eq!(pedals(&h), want);
            // Dragging where it's down lets it up.
            let (a, b) = (h.pedal(5), h.pedal(6));
            h.drag(&[a, b]);
            want[5] = Some(PEDAL_UP);
            want[6] = None;
            want[7] = Some(PEDAL_DOWN);
            assert_eq!(pedals(&h), want);
            h.undo();
            h.undo();
            h.undo();
            assert_eq!(pedals(&h), vec![None; 16]);
        }

        #[test]
        fn shift_drag_selects_the_pedal_alone_or_with_notes() {
            let mut h = Harness::new(true);
            h.shift = true;
            let (a, b) = (h.pedal(5), h.pedal(2));
            h.drag(&[a, b]);
            assert_eq!(
                h.view.selection((0, 0)),
                Some(&Selection {
                    rows: 2..6,
                    values: Some(crate::selection::NO_VALUES),
                    pedal: true,
                }),
                "nothing painted, nothing put down"
            );
            assert!(h.cells().iter().all(|c| *c == Cell::default()));
            // Up into the notes: those from there down, with the pedal.
            let (a, b) = (h.pedal(1), h.at(60, 3));
            h.drag(&[a, b]);
            let lowest = crate::tuning::LOWEST_NOTE as u16;
            assert_eq!(
                h.view.selection((0, 0)),
                Some(&Selection {
                    rows: 1..4,
                    values: Some(lowest..=60),
                    pedal: true,
                })
            );
        }

        #[test]
        fn right_click_rolls_a_hit() {
            let mut h = Harness::new(false);
            let a = h.at(1, 3);
            h.drag(&[a]);
            h.frame(vec![Event::PointerButton {
                pos: a,
                button: PointerButton::Secondary,
                pressed: true,
                modifiers: Modifiers::NONE,
            }]);
            h.frame(vec![Event::PointerButton {
                pos: a,
                button: PointerButton::Secondary,
                pressed: false,
                modifiers: Modifiers::NONE,
            }]);
            assert_eq!(h.cells()[3].fx[0], Some(Fx::Retrig(2)));
        }
    }

    #[test]
    fn right_click_steps_through_rolls() {
        let mut c = Cell::hit(0, 100);
        let mut seen = vec![];
        for _ in 0..6 {
            c = next_roll(c);
            seen.push(c.fx().find_map(|f| match f {
                Fx::Retrig(n) => Some(n),
                _ => None,
            }));
        }
        assert_eq!(seen, [Some(2), Some(3), Some(4), Some(6), Some(8), None]);
        // With both effect slots taken, the roll replaces the second.
        let full = Cell::hit(0, 100).with_fx(Fx::Reverse).with_fx(Fx::Pitch(3));
        assert_eq!(next_roll(full).fx, [Some(Fx::Reverse), Some(Fx::Retrig(2))]);
    }
}
