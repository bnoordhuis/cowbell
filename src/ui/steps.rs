//! The step grid (F3): the cursor's track as a drum machine. Sample tracks
//! get a lane per slice, synth tracks a piano roll. It edits the same
//! patterns as the tracker, so the two views can be mixed freely.
//!
//! Mouse: click or drag over empty cells to paint hits, over hits to erase
//! them. On synth tracks a new note lasts one step; drag right to make it
//! longer, or drag inside a note to move its end. Right-click a hit to roll
//! it, scroll over a hit to change its velocity, scroll elsewhere to see
//! other lanes. Click a lane's label to hear it.

use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, pos2, vec2,
};

use super::{color, knob};
use crate::editor::{Editor, Snapshot};
use crate::pattern::{Cell, Fx, Instrument, MAX_FX, Trigger};
use crate::rack::{Curve, ParamSpec};
use crate::sample::Sample;
use crate::tuning::{HIGHEST_NOTE, LOWEST_NOTE, note_name};

const TABS: f32 = 26.0;
const HEADER: f32 = 18.0;
const LABEL_W: f32 = 92.0;
const MIN_LANE: f32 = 15.0;
const MAX_LANE: f32 = 28.0;
const NOTE_LANE: f32 = 15.0;
const MIN_COL: f32 = 10.0;
const MAX_COL: f32 = 44.0;
/// Right-click steps a hit through these retrigger counts, then back to a
/// single hit.
const ROLLS: [u8; 5] = [2, 3, 4, 6, 8];
/// Mouse wheel over a hit turns its velocity like a knob.
const VELOCITY: ParamSpec = ParamSpec::new("velocity", 1.0, 127.0, 127.0, Curve::Linear, "");
/// Columns in a slice's waveform glyph.
const PEAKS: usize = 12;

const CELL_A: Color32 = Color32::from_rgb(0x20, 0x21, 0x25);
const CELL_B: Color32 = Color32::from_rgb(0x26, 0x27, 0x2b);
const BLACK_KEY_A: Color32 = Color32::from_rgb(0x1b, 0x1c, 0x20);
const BLACK_KEY_B: Color32 = Color32::from_rgb(0x21, 0x22, 0x26);
const GLYPH: Color32 = Color32::from_rgba_premultiplied(0x10, 0x11, 0x13, 0xc0);

#[derive(Default)]
pub struct StepView {
    gesture: Option<Gesture>,
    wheel: knob::Wheel<(usize, usize)>,
    /// Lanes scrolled past at the top.
    lane_scroll: usize,
    /// The track and kind the scroll was set for: a new track gets its
    /// lanes centred again.
    scrolled_for: Option<(usize, bool)>,
    /// First row shown.
    col_scroll: usize,
    peaks: PeakCache,
}

/// A mouse gesture: what it does, applied afresh to the cells as they were
/// when it started, so dragging back undoes what it passed over.
struct Gesture {
    track: usize,
    before: Snapshot,
    base: Vec<Cell>,
    kind: Kind,
}

#[derive(Debug, PartialEq)]
enum Kind {
    /// Put hits in these (lane, row) cells, in order.
    Paint(Vec<(u16, usize)>),
    /// Remove the hits in these (lane, row) cells.
    Erase(Vec<(u16, usize)>),
    /// A synth note from `start` through `end`.
    Note {
        note: u16,
        start: usize,
        end: usize,
        velocity: u8,
    },
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

/// For each row of a synth track: the note sounding there and the row it
/// started on. A note lasts until the next note, or a cut.
fn sounding(cells: &[Cell]) -> Vec<Option<(u16, usize)>> {
    let mut active = None;
    cells
        .iter()
        .enumerate()
        .map(|(i, c)| {
            match c.trigger {
                Some(t) => active = Some((t.slice, i)),
                None if has_cut(c) => active = None,
                None => {}
            }
            let here = active;
            if c.trigger.is_some() && has_cut(c) {
                active = None;
            }
            here
        })
        .collect()
}

fn apply(base: &[Cell], kind: &Kind, velocity: u8) -> Vec<Cell> {
    let mut cells = base.to_vec();
    match kind {
        Kind::Paint(cellsat) => {
            for &(v, r) in cellsat {
                let c = &mut cells[r];
                let velocity = c.trigger.map_or(velocity, |t| t.velocity);
                c.trigger = Some(Trigger { slice: v, velocity });
            }
        }
        Kind::Erase(cellsat) => {
            for &(v, r) in cellsat {
                if cells[r].trigger.is_some_and(|t| t.slice == v) {
                    cells[r] = Cell::default();
                }
            }
        }
        &Kind::Note {
            note,
            start,
            end,
            velocity,
        } => {
            cells[start] = Cell {
                trigger: Some(Trigger {
                    slice: note,
                    velocity,
                }),
                ..without_cut(cells[start])
            };
            for c in &mut cells[start + 1..=end] {
                *c = Cell {
                    trigger: None,
                    ..without_cut(*c)
                };
            }
            if let Some(next) = cells.get_mut(end + 1)
                && next.trigger.is_none()
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
    tabs: Vec<Rect>,
    labels: Rect,
    grid: Rect,
    /// Lane values, top to bottom: slices ascending, notes descending.
    lanes: Vec<u16>,
    first_lane: usize,
    lane_h: f32,
    first_col: usize,
    col_w: f32,
    rows: usize,
}

impl Geometry {
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
    /// Show the cursor's track of the current pattern. `playhead` is the
    /// row playing in this pattern, if any.
    pub fn show(&mut self, ui: &mut egui::Ui, e: &mut Editor, playhead: Option<usize>) {
        let area = ui.max_rect();
        let font = FontId::monospace(13.0);
        let small = FontId::monospace(11.0);
        let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let response = ui.allocate_rect(area, Sense::click_and_drag());

        let track = e.cursor.track;
        let instrument = e.pattern().tracks.get(track).map_or(0, |t| t.instrument);
        let (synth, sample) = match e.song.instruments.get(instrument) {
            Some(Instrument::Synth(_)) => (true, None),
            Some(Instrument::Sampler(s)) => (false, Some(s.clone())),
            None => (false, None),
        };
        let g = self.layout(e, area, synth, sample.as_deref(), char_w);

        self.interact(ui, &response, e, &g, synth, instrument);

        let p = ui.painter_at(area);
        let tint = |t: usize| color::TRACKS[t % color::TRACKS.len()];
        // Track tabs.
        for (t, r) in g.tabs.iter().enumerate() {
            let on = t == track;
            p.rect_filled(*r, 3.0, if on { color::CURSOR_ROW } else { color::PANEL });
            if on {
                p.rect_stroke(*r, 3.0, Stroke::new(1.0, tint(t)), StrokeKind::Inside);
            }
            let name = &e.pattern().tracks[t].name;
            p.text(
                r.left_center() + vec2(8.0, 0.0),
                Align2::LEFT_CENTER,
                name,
                font.clone(),
                if on { tint(t) } else { color::MUTED },
            );
        }

        let pattern = e.pattern();
        let Some(cells) = pattern.tracks.get(track).map(|t| t.cells.clone()) else {
            return;
        };
        let rpb = pattern.rows_per_beat.max(1) as usize;
        let color = tint(track);
        let (lanes, cols) = (g.visible_lanes(), g.visible_cols());
        let selection = e.selection();

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
                        r.left_top() + vec2(28.0, 2.0),
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
                let fill = if selection.as_ref().is_some_and(|s| s.contains(&row)) {
                    color::SELECTION
                } else if row == e.cursor.row {
                    color::CURSOR_ROW
                } else if synth && is_black_key(v) {
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
            let sound = sounding(&cells);
            for row in 0..cells.len() {
                let Some(t) = cells[row].trigger else {
                    continue;
                };
                let end = (row..cells.len())
                    .take_while(|&i| sound[i] == Some((t.slice, row)))
                    .last()
                    .unwrap_or(row);
                if end < g.first_col || row >= g.first_col + cols {
                    continue;
                }
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
                        draw_hit_marks(&p, ra, &cells[row], None, root && row == e.cursor.row);
                    }
                }
            }
        } else {
            for (row, cell) in cells.iter().enumerate().skip(g.first_col).take(cols) {
                let Some(t) = cell.trigger else {
                    continue;
                };
                let Some(lane) = g.lane_of(t.slice) else {
                    continue;
                };
                let Some(r) = g.cell_rect(lane, row) else {
                    above(&p, row, lane < g.first_lane);
                    continue;
                };
                let fill = hit_color(color, t.velocity);
                let pk = peaks.as_ref().and_then(|pk| pk.get(t.slice as usize));
                draw_hit(&p, r, cell, fill, pk, row == e.cursor.row);
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
    }

    fn layout(
        &mut self,
        e: &Editor,
        area: Rect,
        synth: bool,
        sample: Option<&Sample>,
        char_w: f32,
    ) -> Geometry {
        let track = e.cursor.track;
        let pattern = e.pattern();
        let mut x = area.left() + 8.0;
        let tabs = pattern
            .tracks
            .iter()
            .map(|t| {
                let w = (t.name.chars().count() as f32 + 2.0) * char_w + 4.0;
                let r = Rect::from_min_size(pos2(x, area.top() + 4.0), vec2(w, TABS - 6.0));
                x += w + 6.0;
                r
            })
            .collect();
        let top = area.top() + TABS + HEADER;
        let labels = Rect::from_min_max(
            pos2(area.left() + 8.0, top),
            pos2(area.left() + 8.0 + LABEL_W, area.bottom() - 6.0),
        );
        let grid = Rect::from_min_max(
            pos2(labels.right(), top),
            pos2(area.right() - 8.0, area.bottom() - 6.0),
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
        // A new track starts scrolled to its notes, or the editor's octave.
        if self.scrolled_for != Some((track, synth)) {
            self.scrolled_for = Some((track, synth));
            self.lane_scroll = 0;
            if synth {
                let cells = &pattern.tracks[track].cells;
                let mut notes: Vec<u16> = cells
                    .iter()
                    .filter_map(|c| c.trigger.map(|t| t.slice))
                    .collect();
                notes.sort_unstable();
                let centre = notes
                    .get(notes.len() / 2)
                    .copied()
                    .unwrap_or((e.octave as u16 + 1) * 12 + 6);
                let lane = lanes.iter().position(|&v| v == centre).unwrap_or(0);
                self.lane_scroll = lane.saturating_sub(fit / 2);
            }
        }
        self.lane_scroll = self.lane_scroll.min(lanes.len().saturating_sub(fit));

        let rows = pattern.rows.max(1);
        let col_w = (grid.width() / rows as f32).clamp(MIN_COL, MAX_COL);
        let visible = ((grid.width() / col_w).floor() as usize).clamp(1, rows);
        // Keep the cursor in view.
        let row = e.cursor.row;
        if row < self.col_scroll {
            self.col_scroll = row;
        } else if row >= self.col_scroll + visible {
            self.col_scroll = row + 1 - visible;
        }
        self.col_scroll = self.col_scroll.min(rows - visible);

        Geometry {
            tabs,
            labels,
            grid,
            lanes,
            first_lane: self.lane_scroll,
            lane_h,
            first_col: self.col_scroll,
            col_w,
            rows,
        }
    }

    fn interact(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        e: &mut Editor,
        g: &Geometry,
        synth: bool,
        instrument: usize,
    ) {
        let track = e.cursor.track;
        let (pressed, down, hover, shift) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.hover_pos(),
                i.modifiers.shift,
            )
        });
        let in_area = response.hovered();
        let cells = match e.pattern().tracks.get(track) {
            Some(t) => t.cells.clone(),
            None => return,
        };
        let in_grid = |p: Pos2| {
            let h = g.visible_lanes() as f32 * g.lane_h;
            let w = g.visible_cols() as f32 * g.col_w;
            Rect::from_min_size(g.grid.min, vec2(w, h)).contains(p)
        };

        // Pressing: tabs pick a track, labels play their lane, the grid
        // starts a gesture.
        if pressed
            && in_area
            && let Some(p) = hover
        {
            if let Some(t) = g.tabs.iter().position(|r| r.contains(p)) {
                e.select(t, e.cursor.row);
            } else if g.labels.contains(p) {
                let lane = g.cell_at(p).0;
                if g.lane_y(lane).is_some_and(|y| p.y < y + g.lane_h) {
                    e.audition = Some((instrument, g.lanes[lane]));
                }
            } else if in_grid(p) {
                let (lane, row) = g.cell_at(p);
                let v = g.lanes[lane];
                let kind = if cells[row].trigger.is_some_and(|t| t.slice == v) {
                    Kind::Erase(vec![(v, row)])
                } else if synth {
                    let sound = sounding(&cells);
                    match sound[row] {
                        // Inside a note: move its end.
                        Some((n, start)) if n == v && start < row => Kind::Note {
                            note: v,
                            start,
                            end: row - 1,
                            velocity: cells[start].trigger.map_or(e.last_velocity, |t| t.velocity),
                        },
                        _ => {
                            e.audition = Some((instrument, v));
                            Kind::Note {
                                note: v,
                                start: row,
                                end: row,
                                velocity: e.last_velocity,
                            }
                        }
                    }
                } else {
                    e.audition = Some((instrument, v));
                    Kind::Paint(vec![(v, row)])
                };
                e.select(track, row);
                self.gesture = Some(Gesture {
                    track,
                    before: e.capture(),
                    base: cells.clone(),
                    kind,
                });
            }
        }

        // Dragging.
        if let Some(gs) = &mut self.gesture
            && gs.track == track
        {
            if down && let Some(p) = hover {
                let (lane, row) = g.cell_at(p);
                let v = g.lanes[lane];
                match &mut gs.kind {
                    Kind::Paint(at) => {
                        if at.last() != Some(&(v, row)) {
                            at.push((v, row));
                            e.audition = Some((instrument, v));
                        }
                    }
                    Kind::Erase(at) => {
                        if at.last() != Some(&(v, row)) {
                            at.push((v, row));
                        }
                    }
                    Kind::Note { start, end, .. } => *end = row.max(*start),
                }
                if !matches!(gs.kind, Kind::Note { .. }) {
                    e.cursor.row = row;
                }
            }
            let out = apply(&gs.base, &gs.kind, e.last_velocity);
            e.set_track_cells(gs.track, &out);
            if !down {
                let gs = self.gesture.take().unwrap();
                if e.pattern()
                    .tracks
                    .get(gs.track)
                    .is_some_and(|t| t.cells != gs.base)
                {
                    e.record(gs.before);
                }
            }
        } else if !down {
            self.gesture = None;
        }

        // Right-click a hit to roll it.
        if response.secondary_clicked()
            && let Some(p) = hover.filter(|&p| in_grid(p))
        {
            let (lane, row) = g.cell_at(p);
            if let Some(start) = self.hit_row(&cells, synth, g.lanes[lane], row) {
                let before = e.capture();
                let mut out = cells.clone();
                out[start] = next_roll(out[start]);
                e.select(track, start);
                e.set_track_cells(track, &out);
                e.record(before);
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
                    self.hit_row(&cells, synth, g.lanes[lane], row)
                })
                .flatten();
            if let Some(row) = hit {
                let vel = cells[row].trigger.unwrap().velocity;
                let (delta, now) = ui.input(|i| (knob::wheel_delta(i), i.time));
                if let Some((v, first)) =
                    self.wheel
                        .turn((track, row), &VELOCITY, vel as f32, delta, shift, now)
                {
                    let velocity = v.round().clamp(1.0, 127.0) as u8;
                    if velocity != vel {
                        let before = first.then(|| e.capture());
                        let mut out = cells.clone();
                        if let Some(t) = &mut out[row].trigger {
                            t.velocity = velocity;
                        }
                        e.select(track, row);
                        e.set_track_cells(track, &out);
                        if let Some(b) = before {
                            e.record(b);
                        }
                        e.last_velocity = velocity;
                    }
                }
            } else {
                let dy = ui.input(|i| i.smooth_scroll_delta.y);
                if dy != 0.0 {
                    let lanes = (-dy / g.lane_h).round() as isize;
                    self.lane_scroll = (self.lane_scroll as isize + lanes).max(0) as usize;
                }
            }
        }
    }

    /// The row of the hit at (lane `v`, `row`): the cell itself on sample
    /// tracks, the start of the note covering it on synth tracks.
    fn hit_row(&self, cells: &[Cell], synth: bool, v: u16, row: usize) -> Option<usize> {
        if synth {
            match sounding(cells)[row] {
                Some((n, start)) if n == v => Some(start),
                _ => None,
            }
        } else {
            cells[row]
                .trigger
                .is_some_and(|t| t.slice == v)
                .then_some(row)
        }
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
    let other = cell
        .fx()
        .any(|f| !matches!(f, Fx::Retrig(_) | Fx::Reverse | Fx::Cut(_) | Fx::Chord(_)));
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

    fn cells(n: usize) -> Vec<Cell> {
        vec![Cell::default(); n]
    }

    #[test]
    fn painting_replaces_the_row_and_erasing_only_its_lane() {
        let base = cells(8);
        let out = apply(&base, &Kind::Paint(vec![(2, 0), (2, 1), (3, 1)]), 100);
        assert_eq!(out[0].trigger.unwrap().slice, 2);
        assert_eq!(out[1].trigger.unwrap().slice, 3, "one hit per row");
        assert_eq!(out[0].trigger.unwrap().velocity, 100);
        let out2 = apply(&out, &Kind::Erase(vec![(2, 0), (2, 1)]), 100);
        assert_eq!(out2[0].trigger, None);
        assert_eq!(
            out2[1].trigger.unwrap().slice,
            3,
            "lane 2 had nothing in row 1"
        );
    }

    #[test]
    fn notes_last_until_the_next_note_or_a_cut() {
        let mut c = cells(8);
        c[1] = Cell::hit(60, 100);
        c[4] = Cell::default().with_fx(Fx::Cut(0));
        c[6] = Cell::hit(62, 100);
        let s = sounding(&c);
        assert_eq!(s[0], None);
        assert_eq!(&s[1..4], &[Some((60, 1)); 3]);
        assert_eq!(s[4], None);
        assert_eq!(s[7], Some((62, 6)));
    }

    #[test]
    fn a_note_gesture_sets_its_length() {
        let base = cells(8);
        let kind = |end| Kind::Note {
            note: 60,
            start: 1,
            end,
            velocity: 90,
        };
        let short = apply(&base, &kind(1), 0);
        assert_eq!(sounding(&short)[1..3], [Some((60, 1)), None]);
        assert!(has_cut(&short[2]));
        let long = apply(&short, &kind(4), 0);
        let s = sounding(&long);
        assert_eq!(
            s[1..6],
            [
                Some((60, 1)),
                Some((60, 1)),
                Some((60, 1)),
                Some((60, 1)),
                None
            ]
        );
        assert!(!has_cut(&long[2]), "the old end is gone");
        // To the end of the pattern: nothing to cut.
        let full = apply(&base, &kind(7), 0);
        assert!(full.iter().all(|c| !has_cut(c)));
    }

    mod mouse {
        use super::*;
        use crate::pattern::{Pattern, Song};
        use crate::synthvoice::{SynthKind, SynthPatch};
        use eframe::egui::{Event, Modifiers, PointerButton, RawInput};

        struct Harness {
            ctx: egui::Context,
            view: StepView,
            e: Editor,
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
                let e = Editor::new(Song {
                    bpm: 120.0,
                    instruments: vec![instrument],
                    patterns: vec![Pattern::new(4, 4, &[("a", 0)])],
                    order: vec![0],
                    rack: crate::rack::Rack::simple(1),
                    tuning: Default::default(),
                    automation: Vec::new(),
                });
                let mut h = Harness {
                    ctx: egui::Context::default(),
                    view: StepView::default(),
                    e,
                };
                h.frame(vec![]);
                h
            }

            fn frame(&mut self, events: Vec<Event>) {
                let input = RawInput {
                    screen_rect: Some(SCREEN),
                    events,
                    ..Default::default()
                };
                let (view, e) = (&mut self.view, &mut self.e);
                let mut full = self.ctx.run_ui(input, |ui| view.show(ui, e, None));
                full.textures_delta.clear();
            }

            /// The centre of the cell for lane value `v` in `row`.
            fn at(&mut self, v: u16, row: usize) -> Pos2 {
                let mut out = None;
                let (view, e) = (&mut self.view, &self.e);
                let input = RawInput {
                    screen_rect: Some(SCREEN),
                    ..Default::default()
                };
                let mut full = self.ctx.run_ui(input, |ui| {
                    let synth = e.on_synth();
                    let sample = e.track_sample().map(|(_, s)| s.clone());
                    let w = ui.fonts_mut(|f| f.glyph_width(&FontId::monospace(13.0), '0'));
                    let g = view.layout(e, ui.max_rect(), synth, sample.as_deref(), w);
                    let lane = g.lane_of(v).unwrap();
                    out = Some(g.cell_rect(lane, row).expect("cell in view").center());
                });
                full.textures_delta.clear();
                out.unwrap()
            }

            fn button(&mut self, pos: Pos2, pressed: bool) {
                self.frame(vec![Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
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

            fn triggers(&self) -> Vec<Option<u16>> {
                self.e.pattern().tracks[0]
                    .cells
                    .iter()
                    .map(|c| c.trigger.map(|t| t.slice))
                    .collect()
            }
        }

        #[test]
        fn click_adds_drag_paints_and_one_undo_takes_it_back() {
            let mut h = Harness::new(false);
            let a = h.at(2, 0);
            h.drag(&[a]);
            assert_eq!(h.triggers()[0], Some(2));
            let path = [h.at(1, 4), h.at(1, 5), h.at(1, 6)];
            h.drag(&path);
            assert_eq!(&h.triggers()[4..7], &[Some(1); 3]);
            assert_eq!(h.e.cursor.row, 6, "the cursor follows the paint");
            h.e.apply(crate::editor::Action::Undo);
            assert_eq!(&h.triggers()[4..7], &[None; 3], "one gesture, one undo");
            assert_eq!(h.triggers()[0], Some(2));
            // Clicking a hit removes it.
            let a = h.at(2, 0);
            h.drag(&[a]);
            assert_eq!(h.triggers()[0], None);
        }

        #[test]
        fn wheel_over_a_hit_turns_its_velocity() {
            let mut h = Harness::new(false);
            let a = h.at(3, 2);
            h.drag(&[a]);
            let before = h.e.pattern().tracks[0].cells[2].trigger.unwrap().velocity;
            h.frame(vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: vec2(0.0, -2.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: Modifiers::NONE,
            }]);
            let after = h.e.pattern().tracks[0].cells[2].trigger.unwrap().velocity;
            assert!(after < before, "{before} -> {after}");
        }

        #[test]
        fn dragging_a_new_synth_note_makes_it_longer() {
            let mut h = Harness::new(true);
            let path = [h.at(60, 2), h.at(60, 3), h.at(62, 5)];
            h.drag(&path);
            let s = sounding(&h.e.pattern().tracks[0].cells);
            assert_eq!(
                &s[2..7],
                &[Some((60, 2)); 4]
                    .iter()
                    .copied()
                    .chain([None])
                    .collect::<Vec<_>>()[..]
            );
            // Drag inside it to move its end back.
            let path = [h.at(60, 4), h.at(60, 3)];
            h.drag(&path);
            let s = sounding(&h.e.pattern().tracks[0].cells);
            assert_eq!(s[3], Some((60, 2)));
            assert_eq!(s[4], None);
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
