//! Tracker editing: cursor movement, hex entry, selection, undo. No UI code,
//! so all of this is testable without a window.
//!
//! Entry works like the classic trackers: each hex digit of a field is its
//! own cursor position. Typing a digit overwrites it and moves to the next;
//! after the last digit of a field the cursor drops down by the edit step.

use std::ops::Range;

use crate::pattern::{Cell, Fx, MAX_FX, Pattern, Song, Trigger};
use crate::transform::Roll;

const UNDO_DEPTH: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Slice,
    Velocity,
    Fx(usize),
}

impl Field {
    pub const ALL: [Field; 2 + MAX_FX] =
        [Field::Slice, Field::Velocity, Field::Fx(0), Field::Fx(1)];

    /// Cursor positions in this field: two hex digits, plus the command
    /// letter for effects.
    pub fn digits(self) -> usize {
        match self {
            Field::Fx(_) => 3,
            _ => 2,
        }
    }

    fn index(self) -> usize {
        Field::ALL.iter().position(|&f| f == self).unwrap()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Position in the song's order list.
    pub order: usize,
    pub row: usize,
    pub track: usize,
    pub field: Field,
    pub digit: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Top,
    Bottom,
    NextTrack,
    PrevTrack,
    NextOrder,
    PrevOrder,
    /// Move up or down, extending the selection.
    SelectUp,
    SelectDown,
    /// A typed character: hex digits, or effect letters in an effect column.
    Char(char),
    /// Put this slice in the cell under the cursor, then step down.
    EnterSlice(u16),
    /// Clear the selection, or the field under the cursor.
    Clear,
    Roll,
    Undo,
    Redo,
    Copy,
    Paste,
    AddTrack,
    EditStep(isize),
}

impl Fx {
    pub fn letter(self) -> char {
        match self {
            Fx::Offset(_) => 'O',
            Fx::Retrig(_) => 'R',
            Fx::Reverse => 'V',
            Fx::Pitch(_) => 'P',
            Fx::Crush(_) => 'B',
            Fx::Stutter(_) => 'S',
            Fx::Cut(_) => 'C',
        }
    }

    pub fn param(self) -> u8 {
        match self {
            Fx::Offset(n) | Fx::Retrig(n) | Fx::Crush(n) | Fx::Stutter(n) | Fx::Cut(n) => n,
            Fx::Pitch(n) => n as u8,
            Fx::Reverse => 1,
        }
    }

    pub fn from_letter(letter: char, param: u8) -> Option<Fx> {
        Some(match letter.to_ascii_uppercase() {
            'O' => Fx::Offset(param),
            'R' => Fx::Retrig(param),
            'V' => Fx::Reverse,
            'P' => Fx::Pitch(param as i8),
            'B' => Fx::Crush(param),
            'S' => Fx::Stutter(param),
            'C' => Fx::Cut(param),
            _ => return None,
        })
    }

    /// A plain-words description, for the status bar.
    pub fn describe(self) -> String {
        match self {
            Fx::Offset(n) => format!("start {:.0}% into the slice", n as f32 / 2.56),
            Fx::Retrig(n) => format!("retrigger {n} times in the row"),
            Fx::Reverse => "play backwards".into(),
            Fx::Pitch(n) => format!("pitch {n:+} semitones"),
            Fx::Crush(n) => format!("crush to {} bits", n.clamp(1, 16)),
            Fx::Stutter(n) => format!("loop the first {n} ms"),
            Fx::Cut(n) => format!("cut after {n}/16 of the row"),
        }
    }
}

pub struct Editor {
    pub song: Song,
    pub cursor: Cursor,
    /// Selection anchor row, in the cursor's track.
    pub anchor: Option<usize>,
    pub edit_step: usize,
    pub last_slice: u16,
    pub last_velocity: u8,
    /// Set when the song changed and the engine needs the new version.
    pub dirty: bool,
    /// Set when a slice should be previewed: `(instrument, slice)`.
    pub audition: Option<(usize, u16)>,
    clipboard: Vec<Cell>,
    undo: Vec<Vec<Pattern>>,
    redo: Vec<Vec<Pattern>>,
    /// The field being typed into. Typing into the same field again is part
    /// of the same undo step.
    typing_in: Option<(usize, usize, usize, Field)>,
}

fn hex(c: char) -> Option<u8> {
    c.to_digit(16).map(|d| d as u8)
}

fn set_nibble(value: u8, digit: usize, d: u8) -> u8 {
    if digit == 0 {
        (d << 4) | (value & 0x0f)
    } else {
        (value & 0xf0) | d
    }
}

impl Editor {
    pub fn new(song: Song) -> Self {
        assert!(
            !song.order.is_empty(),
            "song needs at least one order entry"
        );
        Editor {
            song,
            cursor: Cursor {
                order: 0,
                row: 0,
                track: 0,
                field: Field::Slice,
                digit: 0,
            },
            anchor: None,
            edit_step: 1,
            last_slice: 0,
            last_velocity: 0x7f,
            dirty: false,
            audition: None,
            clipboard: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            typing_in: None,
        }
    }

    pub fn pattern_index(&self) -> usize {
        self.song.order[self.cursor.order]
    }

    pub fn pattern(&self) -> &Pattern {
        &self.song.patterns[self.pattern_index()]
    }

    pub fn cell(&self) -> Cell {
        self.pattern()
            .tracks
            .get(self.cursor.track)
            .map_or_else(Cell::default, |t| t.cells[self.cursor.row])
    }

    /// The selected rows in the cursor's track.
    pub fn selection(&self) -> Option<Range<usize>> {
        let a = self.anchor?;
        let r = self.cursor.row;
        Some(a.min(r)..a.max(r) + 1)
    }

    /// Move the cursor to a row, e.g. to follow the playhead.
    pub fn go_to(&mut self, order: usize, row: usize) {
        self.cursor.order = order.min(self.song.order.len() - 1);
        self.cursor.row = row;
        self.anchor = None;
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        let p = self.pattern();
        let (rows, tracks) = (p.rows.max(1), p.tracks.len().max(1));
        self.cursor.row = self.cursor.row.min(rows - 1);
        self.cursor.track = self.cursor.track.min(tracks - 1);
        self.anchor = self.anchor.map(|a| a.min(rows - 1));
    }

    fn snapshot(&mut self) {
        self.undo.push(self.song.patterns.clone());
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    fn cell_mut(&mut self, row: usize) -> Option<&mut Cell> {
        let p = self.song.order[self.cursor.order];
        self.song.patterns[p]
            .tracks
            .get_mut(self.cursor.track)?
            .cells
            .get_mut(row)
    }

    fn instrument(&self) -> usize {
        self.pattern()
            .tracks
            .get(self.cursor.track)
            .map_or(0, |t| t.instrument)
    }

    fn move_rows(&mut self, delta: isize) {
        let rows = self.pattern().rows.max(1) as isize;
        self.cursor.row = (self.cursor.row as isize + delta).rem_euclid(rows) as usize;
    }

    fn step_down(&mut self) {
        self.cursor.digit = 0;
        if self.edit_step > 0 {
            self.move_rows(self.edit_step as isize);
        }
    }

    fn move_horizontal(&mut self, right: bool) {
        let tracks = self.pattern().tracks.len();
        let c = &mut self.cursor;
        if right {
            if c.digit + 1 < c.field.digits() {
                c.digit += 1;
            } else if c.field.index() + 1 < Field::ALL.len() {
                c.field = Field::ALL[c.field.index() + 1];
                c.digit = 0;
            } else if c.track + 1 < tracks {
                c.track += 1;
                c.field = Field::Slice;
                c.digit = 0;
            }
        } else if c.digit > 0 {
            c.digit -= 1;
        } else if c.field.index() > 0 {
            c.field = Field::ALL[c.field.index() - 1];
            c.digit = c.field.digits() - 1;
        } else if c.track > 0 {
            c.track -= 1;
            c.field = Field::Fx(MAX_FX - 1);
            c.digit = 2;
        }
    }

    pub fn apply(&mut self, action: Action) {
        let page = (self.pattern().rows_per_beat * 4) as isize;
        let selecting = matches!(action, Action::SelectUp | Action::SelectDown);
        let typing_in = self.typing_in.take();
        if let Action::Char(_) = action {
            self.typing_in = typing_in;
        }
        if selecting && self.anchor.is_none() {
            self.anchor = Some(self.cursor.row);
        }
        match action {
            Action::Up => self.move_rows(-1),
            Action::Down => self.move_rows(1),
            Action::SelectUp => self.cursor.row = self.cursor.row.saturating_sub(1),
            Action::SelectDown => {
                self.cursor.row = (self.cursor.row + 1).min(self.pattern().rows.saturating_sub(1))
            }
            Action::Left => self.move_horizontal(false),
            Action::Right => self.move_horizontal(true),
            Action::PageUp => self.cursor.row = self.cursor.row.saturating_sub(page as usize),
            Action::PageDown => self.cursor.row += page as usize,
            Action::Top => self.cursor.row = 0,
            Action::Bottom => self.cursor.row = usize::MAX,
            Action::NextTrack | Action::PrevTrack => {
                let n = self.pattern().tracks.len().max(1);
                let d = if action == Action::NextTrack {
                    1
                } else {
                    n - 1
                };
                self.cursor.track = (self.cursor.track + d) % n;
                self.cursor.field = Field::Slice;
                self.cursor.digit = 0;
            }
            Action::NextOrder => {
                self.cursor.order = (self.cursor.order + 1) % self.song.order.len()
            }
            Action::PrevOrder => {
                let n = self.song.order.len();
                self.cursor.order = (self.cursor.order + n - 1) % n;
            }
            Action::Char(c) => {
                let before = self.cursor;
                self.type_char(c);
                // Typing continues in this field until the cursor leaves it.
                self.typing_in = (before.field == self.cursor.field
                    && before.row == self.cursor.row)
                    .then_some((before.order, before.row, before.track, before.field));
            }
            Action::EnterSlice(slice) => self.enter_slice(slice),
            Action::Clear => self.clear(),
            Action::Roll => self.roll(),
            Action::Undo => self.undo_redo(true),
            Action::Redo => self.undo_redo(false),
            Action::Copy => self.copy(),
            Action::Paste => self.paste(),
            Action::AddTrack => self.add_track(),
            Action::EditStep(d) => {
                self.edit_step = (self.edit_step as isize + d).clamp(0, 16) as usize
            }
        }
        // Moving without shift drops the selection; editing keeps it so you
        // can, say, roll and then clear the same rows.
        let moved = matches!(
            action,
            Action::Up
                | Action::Down
                | Action::Left
                | Action::Right
                | Action::PageUp
                | Action::PageDown
                | Action::Top
                | Action::Bottom
                | Action::NextTrack
                | Action::PrevTrack
                | Action::NextOrder
                | Action::PrevOrder
        );
        if moved {
            self.anchor = None;
        }
        self.clamp_cursor();
    }

    fn type_char(&mut self, c: char) {
        let Cursor {
            row, field, digit, ..
        } = self.cursor;
        let (last_slice, last_velocity) = (self.last_slice, self.last_velocity);
        let current = self.cell();
        let new = match field {
            Field::Slice | Field::Velocity => {
                let Some(d) = hex(c) else { return };
                let mut t = current.trigger.unwrap_or(Trigger {
                    slice: last_slice,
                    velocity: last_velocity,
                });
                if field == Field::Slice {
                    t.slice = set_nibble(t.slice.min(0xff) as u8, digit, d) as u16;
                } else {
                    t.velocity = set_nibble(t.velocity, digit, d).min(0x7f);
                }
                Cell {
                    trigger: Some(t),
                    ..current
                }
            }
            Field::Fx(i) => {
                let mut cell = current;
                let old = cell.fx[i];
                cell.fx[i] = if digit == 0 {
                    let param = old.map_or(0, Fx::param);
                    match Fx::from_letter(c, param) {
                        Some(fx) => Some(fx),
                        None => return,
                    }
                } else {
                    let (Some(fx), Some(d)) = (old, hex(c)) else {
                        return;
                    };
                    Fx::from_letter(fx.letter(), set_nibble(fx.param(), digit - 1, d))
                };
                cell
            }
        };
        if new == current {
            // Still advance, so typing over a value with itself moves on.
            self.advance_after_entry();
            return;
        }
        let c = self.cursor;
        let typing_in = Some((c.order, c.row, c.track, c.field));
        if self.typing_in != typing_in {
            self.snapshot();
        }
        self.dirty = true;
        *self.cell_mut(row).unwrap() = new;
        if let Some(t) = new.trigger {
            self.last_slice = t.slice;
            self.last_velocity = t.velocity;
            if field == Field::Slice {
                self.audition = Some((self.instrument(), t.slice));
            }
        }
        self.advance_after_entry();
    }

    fn enter_slice(&mut self, slice: u16) {
        let row = self.cursor.row;
        let current = self.cell();
        let velocity = current.trigger.map_or(self.last_velocity, |t| t.velocity);
        self.snapshot();
        *self.cell_mut(row).unwrap() = Cell {
            trigger: Some(Trigger { slice, velocity }),
            ..current
        };
        self.last_slice = slice;
        self.audition = Some((self.instrument(), slice));
        self.step_down();
    }

    fn advance_after_entry(&mut self) {
        if self.cursor.digit + 1 < self.cursor.field.digits() {
            self.cursor.digit += 1;
        } else {
            self.step_down();
        }
    }

    fn clear(&mut self) {
        if let Some(rows) = self.selection() {
            self.snapshot();
            for row in rows {
                *self.cell_mut(row).unwrap() = Cell::default();
            }
            return;
        }
        let row = self.cursor.row;
        let mut cell = self.cell();
        match self.cursor.field {
            Field::Slice => cell = Cell::default(),
            Field::Velocity => {
                if let Some(t) = &mut cell.trigger {
                    t.velocity = 0x7f;
                }
            }
            Field::Fx(i) => cell.fx[i] = None,
        }
        if cell != self.cell() {
            self.snapshot();
            *self.cell_mut(row).unwrap() = cell;
        }
        self.step_down();
    }

    fn roll(&mut self) {
        let rpb = self.pattern().rows_per_beat as usize;
        let rows = self
            .selection()
            .unwrap_or(self.cursor.row..self.cursor.row + rpb);
        let slice = self.cell().trigger.map_or(self.last_slice, |t| t.slice);
        self.snapshot();
        let p = self.pattern_index();
        let Some(track) = self.song.patterns[p].tracks.get_mut(self.cursor.track) else {
            return;
        };
        Roll {
            slice,
            hits: (1, 6),
            pitch: (0, 12),
            velocity: (70, 127),
            curve: 2.0,
        }
        .apply(track, rows);
    }

    fn copy(&mut self) {
        let rows = self
            .selection()
            .unwrap_or(self.cursor.row..self.cursor.row + 1);
        let p = self.pattern();
        if let Some(track) = p.tracks.get(self.cursor.track) {
            self.clipboard = track.cells[rows].to_vec();
        }
    }

    fn paste(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        self.snapshot();
        let start = self.selection().map_or(self.cursor.row, |r| r.start);
        for (i, &cell) in self.clipboard.clone().iter().enumerate() {
            match self.cell_mut(start + i) {
                Some(c) => *c = cell,
                None => break,
            }
        }
    }

    fn add_track(&mut self) {
        if self.pattern().tracks.len() >= crate::engine::MAX_TRACKS {
            return;
        }
        self.snapshot();
        let instrument = self.instrument();
        for p in &mut self.song.patterns {
            let name = format!("t{}", p.tracks.len() + 1);
            p.tracks.push(crate::pattern::Track {
                name,
                instrument,
                cells: vec![Cell::default(); p.rows],
            });
        }
    }

    fn undo_redo(&mut self, undo: bool) {
        let (from, to) = if undo {
            (&mut self.undo, &mut self.redo)
        } else {
            (&mut self.redo, &mut self.undo)
        };
        if let Some(patterns) = from.pop() {
            to.push(std::mem::replace(&mut self.song.patterns, patterns));
            self.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::Instrument;
    use crate::sample::Sample;
    use std::sync::Arc;

    fn editor() -> Editor {
        let sample = Sample::new("s", 48_000, vec![[0.0; 2]; 10]);
        Editor::new(Song {
            bpm: 120.0,
            instruments: vec![Instrument::Sampler(Arc::new(sample))],
            patterns: vec![Pattern::new(4, 4, &[("a", 0), ("b", 0)])],
            order: vec![0],
        })
    }

    fn type_str(e: &mut Editor, s: &str) {
        s.chars().for_each(|c| e.apply(Action::Char(c)));
    }

    #[test]
    fn hex_entry_fills_digits_then_steps_down() {
        let mut e = editor();
        type_str(&mut e, "0a");
        assert_eq!(
            e.pattern().tracks[0].cells[0].trigger,
            Some(Trigger {
                slice: 0x0a,
                velocity: 0x7f
            })
        );
        assert_eq!((e.cursor.row, e.cursor.digit), (1, 0));
        assert_eq!(e.audition, Some((0, 0x0a)));
        assert!(e.dirty);
    }

    #[test]
    fn velocity_is_clamped() {
        let mut e = editor();
        e.apply(Action::Right);
        e.apply(Action::Right);
        assert_eq!(e.cursor.field, Field::Velocity);
        type_str(&mut e, "f");
        assert_eq!(
            e.pattern().tracks[0].cells[0].trigger.unwrap().velocity,
            0x7f
        );
    }

    #[test]
    fn effect_letter_then_param() {
        let mut e = editor();
        e.cursor.field = Field::Fx(0);
        type_str(&mut e, "r18");
        assert_eq!(e.pattern().tracks[0].cells[0].fx[0], Some(Fx::Retrig(0x18)));
        e.cursor.row = 1;
        type_str(&mut e, "pf9");
        assert_eq!(e.pattern().tracks[0].cells[1].fx[0], Some(Fx::Pitch(-7)));
        // Hex letters are not effect commands.
        e.cursor.row = 2;
        type_str(&mut e, "a");
        assert_eq!(e.pattern().tracks[0].cells[2].fx[0], None);
    }

    #[test]
    fn right_crosses_fields_and_tracks() {
        let mut e = editor();
        for _ in 0..(2 + 2 + 3 + 3) {
            e.apply(Action::Right);
        }
        assert_eq!(
            (e.cursor.track, e.cursor.field, e.cursor.digit),
            (1, Field::Slice, 0)
        );
        e.apply(Action::Left);
        assert_eq!(
            (e.cursor.track, e.cursor.field, e.cursor.digit),
            (0, Field::Fx(1), 2)
        );
    }

    #[test]
    fn undo_and_redo() {
        let mut e = editor();
        type_str(&mut e, "05");
        e.apply(Action::Undo);
        assert_eq!(e.pattern().tracks[0].cells[0], Cell::default());
        e.apply(Action::Redo);
        assert_eq!(e.pattern().tracks[0].cells[0].trigger.unwrap().slice, 5);
    }

    #[test]
    fn roll_fills_selection() {
        let mut e = editor();
        type_str(&mut e, "03");
        e.cursor.row = 4;
        for _ in 0..7 {
            e.apply(Action::SelectDown);
        }
        assert_eq!(e.selection(), Some(4..12));
        e.apply(Action::Roll);
        let cells = &e.pattern().tracks[0].cells;
        assert!(cells[4..12].iter().all(|c| c.trigger.unwrap().slice == 3));
        assert_eq!(cells[11].fx[0], Some(Fx::Retrig(6)));
        assert_eq!(cells[12], Cell::default());
    }

    #[test]
    fn copy_paste_and_clear_selection() {
        let mut e = editor();
        type_str(&mut e, "0102");
        e.cursor.row = 0;
        e.apply(Action::SelectDown);
        e.apply(Action::Copy);
        e.apply(Action::Clear);
        assert!(
            e.pattern().tracks[0].cells[..2]
                .iter()
                .all(|c| *c == Cell::default())
        );
        e.apply(Action::Undo);
        e.go_to(0, 8);
        e.apply(Action::Paste);
        let slices: Vec<_> = e.pattern().tracks[0].cells[8..10]
            .iter()
            .map(|c| c.trigger.unwrap().slice)
            .collect();
        assert_eq!(slices, vec![1, 2]);
    }

    #[test]
    fn rows_wrap() {
        let mut e = editor();
        e.apply(Action::Up);
        assert_eq!(e.cursor.row, 15);
        e.apply(Action::Down);
        assert_eq!(e.cursor.row, 0);
    }
}
