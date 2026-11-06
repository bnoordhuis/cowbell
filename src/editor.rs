//! Tracker editing: cursor movement, hex entry, selection, undo. No UI code,
//! so all of this is testable without a window.
//!
//! Entry works like the classic trackers: each hex digit of a field is its
//! own cursor position. Typing a digit overwrites it and moves to the next;
//! after the last digit of a field the cursor drops down by the edit step.

use std::ops::Range;
use std::sync::Arc;

use crate::pattern::{Cell, Fx, Instrument, MAX_FX, Pattern, Song, Target, Trigger};
use crate::rack::Rack;
use crate::sample::Sample;
use crate::synthvoice::{SynthKind, SynthPatch};
use crate::transform::Roll;
use crate::tuning::{HIGHEST_NOTE, LOWEST_NOTE};

const UNDO_DEPTH: usize = 500;

/// Rows per beat to choose from: straight and triplet grids, down to 256th
/// notes.
pub const RESOLUTIONS: [u32; 12] = [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64];
/// Longest pattern, in rows.
pub const MAX_ROWS: usize = 1024;

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
    /// Render the selected rows (or the whole pattern) to a new sample on a
    /// new track, replacing what was there. See [`crate::bounce`].
    Bounce,
    /// Move the octave that note keys enter, for synth tracks.
    Octave(isize),
    /// Put a new synth on the cursor's track.
    AddSynth(SynthKind),
    /// Fill the selection's effect or velocity column between its first
    /// and last value.
    Interpolate,
    /// Add variations of the current pattern after it in the order list,
    /// varying the cursor's track (its selection, if any).
    Variations,
    /// Cycle the instrument of the cursor's track.
    NextInstrument,
    PrevInstrument,
    /// Insert a new empty pattern after the current order slot.
    NewPattern,
    /// Insert a copy of the current pattern after the current order slot.
    DuplicatePattern,
    /// Play the current pattern again in the next order slot.
    RepeatSlot,
    /// Remove the current order slot.
    DeleteSlot,
    /// Make the current order slot play another pattern.
    SlotPattern(isize),
    /// Step the rows per beat through [`RESOLUTIONS`], keeping the timing
    /// of what's in the pattern.
    Resolution(isize),
    /// Lengthen or shorten the pattern by this many rows.
    Length(isize),
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
            Fx::Glide(_) => 'G',
            Fx::Chord(_) => 'H',
            Fx::Knob(_) => 'K',
        }
    }

    pub fn param(self) -> u8 {
        match self {
            Fx::Offset(n)
            | Fx::Retrig(n)
            | Fx::Crush(n)
            | Fx::Stutter(n)
            | Fx::Cut(n)
            | Fx::Glide(n)
            | Fx::Chord(n)
            | Fx::Knob(n) => n,
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
            'G' => Fx::Glide(param),
            'H' => Fx::Chord(param),
            'K' => Fx::Knob(param),
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
            Fx::Glide(n) => format!("glide over {} ms", n as u32 * 4),
            Fx::Knob(n) => format!("automated knob to {:.0}%", n as f32 / 2.55),
            Fx::Chord(n) => {
                let notes: Vec<String> = Fx::chord_intervals(n).map(|i| format!("+{i}")).collect();
                format!("chord: root {}", notes.join(" "))
            }
        }
    }
}

/// Everything undo puts back: patterns, the order list, the rack, and
/// instruments (a bounce adds one). Instruments are `Arc`s, so this doesn't
/// copy sample data.
#[derive(Clone)]
pub struct Snapshot {
    patterns: Vec<Pattern>,
    order: Vec<usize>,
    automation: Vec<Option<Target>>,
    rack: Rack,
    instruments: Vec<Instrument>,
}

pub struct Editor {
    pub song: Song,
    pub cursor: Cursor,
    /// Selection anchor row, in the cursor's track.
    pub anchor: Option<usize>,
    pub edit_step: usize,
    pub last_slice: u16,
    pub last_velocity: u8,
    /// The last note entered on a synth track.
    pub last_note: u8,
    /// The octave note keys enter on synth tracks; 4 is middle C.
    pub octave: u8,
    /// Seed for the next set of variations.
    pub seed: u32,
    /// Set when the song changed and the engine needs the new version.
    pub dirty: bool,
    /// Counts changes, so the UI can tell whether there are unsaved ones.
    pub revision: u64,
    /// Set when a slice should be previewed: `(instrument, slice)`.
    pub audition: Option<(usize, u16)>,
    /// A note for the status bar about the last action, e.g. why it failed.
    pub message: Option<String>,
    clipboard: Vec<Cell>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The field being typed into. Typing into the same field again is part
    /// of the same undo step.
    typing_in: Option<(usize, usize, usize, Field)>,
}

/// Semitone above the octave's C for a note key, tracker style: the bottom
/// letter row is one octave, the top row (with the number row for sharps)
/// the next.
fn piano_key(c: char) -> Option<u8> {
    const LOWER: &str = "zsxdcvgbhnjm,";
    const UPPER: &str = "q2w3er5t6y7ui";
    let c = c.to_ascii_lowercase();
    LOWER
        .find(c)
        .or_else(|| UPPER.find(c).map(|i| i + 12))
        .map(|i| i as u8)
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
            last_note: 60,
            octave: 4,
            seed: 0x5eed,
            dirty: false,
            revision: 0,
            audition: None,
            message: None,
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
        self.cursor.order = self.cursor.order.min(self.song.order.len() - 1);
        let p = self.pattern();
        let (rows, tracks) = (p.rows.max(1), p.tracks.len().max(1));
        self.cursor.row = self.cursor.row.min(rows - 1);
        self.cursor.track = self.cursor.track.min(tracks - 1);
        self.anchor = self.anchor.map(|a| a.min(rows - 1));
        self.cursor.digit = self.cursor.digit.min(self.digits(self.cursor.field) - 1);
    }

    /// The current state, for undo.
    pub fn capture(&self) -> Snapshot {
        Snapshot {
            patterns: self.song.patterns.clone(),
            order: self.song.order.clone(),
            automation: self.song.automation.clone(),
            rack: self.song.rack.clone(),
            instruments: self.song.instruments.clone(),
        }
    }

    /// Record `before` as an undo step, for changes made outside the editor
    /// (the rack view edits the rack directly).
    pub fn record(&mut self, before: Snapshot) {
        self.undo.push(before);
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.changed();
    }

    /// Record a rack change made outside the editor, given the rack as it
    /// was before.
    pub fn record_rack(&mut self, before: Rack) {
        let mut snap = self.capture();
        snap.rack = before;
        self.record(snap);
    }

    /// Mark the song as changed: the engine needs it, and it's unsaved.
    pub fn changed(&mut self) {
        self.dirty = true;
        self.revision += 1;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn snapshot(&mut self) {
        self.record(self.capture());
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

    /// Cursor positions in `field` on the cursor's track. A synth track's
    /// note is one position: a key enters the whole note.
    pub fn digits(&self, field: Field) -> usize {
        if field == Field::Slice && self.on_synth() {
            1
        } else {
            field.digits()
        }
    }

    /// Whether the cursor's track plays a synth.
    pub fn on_synth(&self) -> bool {
        self.song
            .instruments
            .get(self.instrument())
            .is_some_and(Instrument::is_synth)
    }

    fn move_horizontal(&mut self, right: bool) {
        let tracks = self.pattern().tracks.len();
        let slice_digits = self.digits(Field::Slice);
        let digits = |f: Field| {
            if f == Field::Slice {
                slice_digits
            } else {
                f.digits()
            }
        };
        let c = &mut self.cursor;
        if right {
            if c.digit + 1 < digits(c.field) {
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
            c.digit = digits(c.field) - 1;
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
            Action::Bounce => self.bounce(),
            Action::NewPattern => self.new_pattern(false),
            Action::DuplicatePattern => self.new_pattern(true),
            Action::RepeatSlot => self.repeat_slot(),
            Action::DeleteSlot => self.delete_slot(),
            Action::SlotPattern(d) => self.slot_pattern(d),
            Action::Resolution(d) => self.step_resolution(d),
            Action::Length(d) => self.change_length(d),
            Action::AddSynth(kind) => self.add_synth(kind),
            Action::Interpolate => self.interpolate(),
            Action::Variations => self.variations(),
            Action::Octave(d) => {
                self.octave = (self.octave as isize + d).clamp(0, 8) as u8;
                self.message = Some(format!("octave {}", self.octave));
            }
            Action::NextInstrument => self.cycle_instrument(1),
            Action::PrevInstrument => self.cycle_instrument(-1),
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
        let synth = self.on_synth();
        let last = if synth {
            self.last_note as u16
        } else {
            self.last_slice
        };
        let (last_slice, last_velocity) = (last, self.last_velocity);
        let current = self.cell();
        let new = match field {
            Field::Slice if synth && c == '.' => {
                // Note off: release whatever the track is holding.
                Cell {
                    trigger: None,
                    fx: [Some(Fx::Cut(0)), current.fx[1]],
                }
            }
            Field::Slice if synth => {
                let Some(key) = piano_key(c) else { return };
                let note = ((self.octave as u16 + 1) * 12 + key as u16)
                    .clamp(LOWEST_NOTE as u16, HIGHEST_NOTE as u16);
                let velocity = current.trigger.map_or(last_velocity, |t| t.velocity);
                Cell {
                    trigger: Some(Trigger {
                        slice: note,
                        velocity,
                    }),
                    ..current
                }
            }
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
        self.changed();
        *self.cell_mut(row).unwrap() = new;
        if let Some(t) = new.trigger {
            if synth {
                self.last_note = t.slice as u8;
            } else {
                self.last_slice = t.slice;
            }
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
        if self.cursor.digit + 1 < self.digits(self.cursor.field) {
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
        let track = self.pattern().tracks.len();
        self.song.rack.add_track(track);
        for p in &mut self.song.patterns {
            let name = format!("t{}", p.tracks.len() + 1);
            p.tracks.push(crate::pattern::Track {
                name,
                instrument,
                cells: vec![Cell::default(); p.rows],
            });
        }
    }

    fn new_pattern(&mut self, copy: bool) {
        let current = self.pattern();
        let pattern = if copy {
            current.clone()
        } else {
            let mut p = current.clone();
            for t in &mut p.tracks {
                t.cells.fill(Cell::default());
            }
            p
        };
        self.snapshot();
        self.song.patterns.push(pattern);
        let index = self.song.patterns.len() - 1;
        self.song.order.insert(self.cursor.order + 1, index);
        self.go_to(self.cursor.order + 1, 0);
        let what = if copy {
            "copy of the pattern"
        } else {
            "new pattern"
        };
        self.message = Some(format!("{what}: {index:02X}"));
    }

    fn repeat_slot(&mut self) {
        self.snapshot();
        let p = self.pattern_index();
        self.song.order.insert(self.cursor.order + 1, p);
        self.go_to(self.cursor.order + 1, self.cursor.row);
    }

    fn delete_slot(&mut self) {
        if self.song.order.len() < 2 {
            self.message = Some("can't remove the only order slot".into());
            return;
        }
        self.snapshot();
        self.song.order.remove(self.cursor.order);
        self.go_to(
            self.cursor.order.min(self.song.order.len() - 1),
            self.cursor.row,
        );
    }

    fn slot_pattern(&mut self, delta: isize) {
        let n = self.song.patterns.len() as isize;
        if n < 2 {
            return;
        }
        self.snapshot();
        let p = (self.pattern_index() as isize + delta).rem_euclid(n) as usize;
        self.song.order[self.cursor.order] = p;
        self.clamp_cursor();
    }

    fn step_resolution(&mut self, delta: isize) {
        let rpb = self.pattern().rows_per_beat;
        let i = RESOLUTIONS
            .iter()
            .position(|&r| r >= rpb)
            .unwrap_or(RESOLUTIONS.len() - 1);
        let j = (i as isize + delta).clamp(0, RESOLUTIONS.len() as isize - 1) as usize;
        if RESOLUTIONS[j] != rpb {
            self.set_resolution(RESOLUTIONS[j]);
        }
    }

    /// Change rows per beat. Cells move to the row nearest their old time;
    /// if two land on the same row, the first one stays.
    pub fn set_resolution(&mut self, rows_per_beat: u32) {
        let old = self.pattern().rows_per_beat.max(1);
        let scale = rows_per_beat as f64 / old as f64;
        let rows = ((self.pattern().rows as f64 * scale).round() as usize).clamp(1, MAX_ROWS);
        self.snapshot();
        let p = self.pattern_index();
        let pattern = &mut self.song.patterns[p];
        let mut dropped = 0;
        for t in &mut pattern.tracks {
            let mut cells = vec![Cell::default(); rows];
            for (r, c) in t
                .cells
                .iter()
                .enumerate()
                .filter(|(_, c)| **c != Cell::default())
            {
                let to = ((r as f64 * scale).round() as usize).min(rows - 1);
                if cells[to] == Cell::default() {
                    cells[to] = *c;
                } else {
                    dropped += 1;
                }
            }
            t.cells = cells;
        }
        pattern.rows_per_beat = rows_per_beat;
        pattern.rows = rows;
        self.cursor.row = ((self.cursor.row as f64 * scale) as usize).min(rows - 1);
        self.anchor = None;
        self.message = Some(match dropped {
            0 => format!("{rows_per_beat} rows per beat"),
            n => format!(
                "{rows_per_beat} rows per beat: {n} cells shared a row and were dropped (ctrl+z to undo)"
            ),
        });
    }

    fn change_length(&mut self, delta: isize) {
        let p = self.pattern();
        let rows = (p.rows as isize + delta).clamp(1, MAX_ROWS as isize) as usize;
        if rows == p.rows {
            return;
        }
        self.snapshot();
        let p = self.pattern_index();
        let pattern = &mut self.song.patterns[p];
        for t in &mut pattern.tracks {
            t.cells.resize(rows, Cell::default());
        }
        pattern.rows = rows;
        let rpb = pattern.rows_per_beat as usize;
        let beats = if rows.is_multiple_of(rpb) {
            format!("{}", rows / rpb)
        } else {
            format!("{:.2}", rows as f64 / rpb as f64)
        };
        self.message = Some(format!("{rows} rows, {beats} beats"));
        self.clamp_cursor();
    }

    /// The sample the cursor's track plays, if it plays one.
    pub fn track_sample(&self) -> Option<(usize, &Arc<Sample>)> {
        let i = self.instrument();
        match self.song.instruments.get(i)? {
            Instrument::Sampler(s) => Some((i, s)),
            Instrument::Synth(_) => None,
        }
    }

    /// The synth the cursor's track plays, if it plays one.
    pub fn track_synth(&self) -> Option<(usize, &Arc<SynthPatch>)> {
        let i = self.instrument();
        match self.song.instruments.get(i)? {
            Instrument::Synth(p) => Some((i, p)),
            Instrument::Sampler(_) => None,
        }
    }

    /// Add a new synth and put it on the cursor's track.
    pub fn add_synth(&mut self, kind: SynthKind) {
        let n = self
            .song
            .instruments
            .iter()
            .filter(|i| i.is_synth())
            .count()
            + 1;
        let patch = SynthPatch::new(kind, format!("{}{n}", kind.name()));
        let name = patch.name.clone();
        self.snapshot();
        self.song
            .instruments
            .push(Instrument::Synth(Arc::new(patch)));
        self.set_track_instrument(self.song.instruments.len() - 1);
        self.clamp_cursor();
        self.message = Some(format!(
            "track plays {name}: type notes with z-m and q-u, . for note off"
        ));
    }

    /// Turn a synth knob. `new_step` starts an undo step; a drag passes it
    /// only on its first move, so the whole drag undoes at once.
    pub fn set_synth_param(&mut self, instrument: usize, param: usize, value: f32, new_step: bool) {
        let Some(Instrument::Synth(p)) = self.song.instruments.get(instrument) else {
            return;
        };
        let Some(spec) = p.kind.params().get(param) else {
            return;
        };
        let value = spec.clamp(value);
        if p.params.get(param) == Some(&value) {
            return;
        }
        let mut patch = SynthPatch::clone(p);
        patch.params[param] = value;
        if new_step {
            self.snapshot();
        } else {
            self.changed();
        }
        self.song.instruments[instrument] = Instrument::Synth(Arc::new(patch));
    }

    /// Make `target` the knob the cursor's track automates with `K`, or
    /// clear it.
    pub fn set_automation(&mut self, target: Option<Target>) {
        let t = self.cursor.track;
        if self.song.automation(t) == target {
            return;
        }
        self.snapshot();
        if self.song.automation.len() <= t {
            self.song.automation.resize(t + 1, None);
        }
        self.song.automation[t] = target;
    }

    fn variations(&mut self) {
        use crate::variations::{Rng, STRENGTHS, vary};
        let rows = self.selection().unwrap_or(0..self.pattern().rows);
        let (t, synth) = (self.cursor.track, self.on_synth());
        let mut rng = Rng::new(self.seed);
        self.seed = self
            .seed
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        let base = self.pattern().clone();
        let made: Vec<Pattern> = STRENGTHS
            .iter()
            .map(|&s| vary(&base, t, rows.clone(), synth, s, &mut rng))
            .collect();
        self.snapshot();
        for (k, p) in made.into_iter().enumerate() {
            self.song.patterns.push(p);
            let index = self.song.patterns.len() - 1;
            self.song.order.insert(self.cursor.order + 1 + k, index);
        }
        self.message = Some(format!(
            "{} variations after this slot, mild to wild: enter plays from here, ctrl+delete drops one, ctrl+g makes more",
            STRENGTHS.len()
        ));
    }

    fn interpolate(&mut self) {
        let Some(rows) = self.selection() else {
            self.message =
                Some("select rows first (shift+↑↓), then ctrl+k fills between the ends".into());
            return;
        };
        let field = self.cursor.field;
        let p = self.pattern_index();
        let t = self.cursor.track;
        let cells = &self.song.patterns[p].tracks[t].cells[rows.clone()];
        let lerp = |a: f32, b: f32, x: f32| (a + (b - a) * x).round();
        let mut new: Vec<Cell> = cells.to_vec();
        match field {
            Field::Velocity => {
                let hits: Vec<usize> = (0..cells.len())
                    .filter(|&i| cells[i].trigger.is_some())
                    .collect();
                let (Some(&a), Some(&b)) = (hits.first(), hits.last()) else {
                    return;
                };
                if a == b {
                    return;
                }
                let (va, vb) = (
                    cells[a].trigger.unwrap().velocity as f32,
                    cells[b].trigger.unwrap().velocity as f32,
                );
                for &i in &hits {
                    let v = lerp(va, vb, (i - a) as f32 / (b - a) as f32);
                    new[i].trigger.as_mut().unwrap().velocity = v as u8;
                }
            }
            Field::Fx(k) => {
                let set: Vec<usize> = (0..cells.len())
                    .filter(|&i| cells[i].fx[k].is_some())
                    .collect();
                let (Some(&a), Some(&b)) = (set.first(), set.last()) else {
                    self.message =
                        Some("put a value at both ends of the selection to fill between".into());
                    return;
                };
                let (fa, fb) = (cells[a].fx[k].unwrap(), cells[b].fx[k].unwrap());
                if a == b || fa.letter() != fb.letter() {
                    self.message = Some("both ends need the same effect".into());
                    return;
                }
                // Pitch is signed; everything else is 0..=FF.
                let signed = matches!(fa, Fx::Pitch(_));
                let value = |f: Fx| {
                    if signed {
                        f.param() as i8 as f32
                    } else {
                        f.param() as f32
                    }
                };
                for (i, cell) in new.iter_mut().enumerate().take(b + 1).skip(a) {
                    let v = lerp(value(fa), value(fb), (i - a) as f32 / (b - a) as f32);
                    let param = if signed { v as i8 as u8 } else { v as u8 };
                    cell.fx[k] = Fx::from_letter(fa.letter(), param);
                }
            }
            Field::Slice => {
                self.message = Some("put the cursor in the velocity or an effect column".into());
                return;
            }
        }
        if new == cells {
            return;
        }
        self.snapshot();
        self.song.patterns[p].tracks[t].cells[rows].copy_from_slice(&new);
    }

    /// Use another tuning for synths.
    pub fn set_tuning(&mut self, tuning: crate::tuning::Tuning) {
        self.song.tuning = tuning;
        self.changed();
    }

    /// Add a sample as a new instrument and put it on the cursor's track.
    pub fn add_sample(&mut self, sample: Sample) {
        self.snapshot();
        let (name, slices) = (sample.name.clone(), sample.slices.len());
        self.song
            .instruments
            .push(Instrument::Sampler(Arc::new(sample)));
        self.set_track_instrument(self.song.instruments.len() - 1);
        self.message = Some(format!("loaded {name}: {slices} slices"));
    }

    /// A track is the same column in every pattern, so it keeps its
    /// instrument across all of them.
    fn set_track_instrument(&mut self, instrument: usize) {
        let t = self.cursor.track;
        for p in &mut self.song.patterns {
            if let Some(track) = p.tracks.get_mut(t) {
                track.instrument = instrument;
            }
        }
    }

    fn cycle_instrument(&mut self, delta: isize) {
        let n = self.song.instruments.len() as isize;
        if n < 2 {
            return;
        }
        self.snapshot();
        let i = (self.instrument() as isize + delta).rem_euclid(n) as usize;
        self.set_track_instrument(i);
        self.clamp_cursor();
        self.message = Some(format!("track plays {}", self.song.instruments[i].name()));
    }

    fn sample_at(&self, instrument: usize) -> Option<&Arc<Sample>> {
        match self.song.instruments.get(instrument)? {
            Instrument::Sampler(s) => Some(s),
            Instrument::Synth(_) => None,
        }
    }

    /// Change which slice numbers cells point at, for every track playing
    /// `instrument`, so they keep playing the same audio after markers are
    /// added or removed.
    fn renumber(&mut self, instrument: usize, f: impl Fn(u16) -> u16) {
        for p in &mut self.song.patterns {
            for t in p.tracks.iter_mut().filter(|t| t.instrument == instrument) {
                for c in &mut t.cells {
                    if let Some(tr) = &mut c.trigger {
                        tr.slice = f(tr.slice);
                    }
                }
            }
        }
    }

    fn replace_sample(&mut self, instrument: usize, sample: Sample) {
        self.song.instruments[instrument] = Instrument::Sampler(Arc::new(sample));
    }

    /// Add a slice marker at `frame`. Returns its index. Later slices move
    /// up one, and cells follow them.
    pub fn insert_marker(&mut self, instrument: usize, frame: usize) -> Option<usize> {
        let s = self.sample_at(instrument)?;
        if frame == 0 || frame >= s.frames.len() || s.slices.contains(&frame) {
            return None;
        }
        let k = s.slices.partition_point(|&m| m < frame);
        let mut slices = s.slices.clone();
        slices.insert(k, frame);
        let new = s.with_slices(slices);
        self.snapshot();
        self.replace_sample(instrument, new);
        let k16 = k as u16;
        self.renumber(instrument, |sl| if sl >= k16 { sl + 1 } else { sl });
        Some(k)
    }

    /// Remove slice marker `index`; that slice joins the one before it, and
    /// cells follow. The first marker can't be removed.
    pub fn delete_marker(&mut self, instrument: usize, index: usize) {
        let Some(s) = self.sample_at(instrument) else {
            return;
        };
        if index == 0 || index >= s.slices.len() {
            return;
        }
        let mut slices = s.slices.clone();
        slices.remove(index);
        let new = s.with_slices(slices);
        self.snapshot();
        self.replace_sample(instrument, new);
        let k = index as u16;
        self.renumber(instrument, |sl| if sl >= k { sl - 1 } else { sl });
    }

    /// Move marker `index` to `frame`, kept between its neighbours so slice
    /// numbers don't change. `new_step` starts an undo step; a drag passes
    /// it only on its first move, so the whole drag undoes at once.
    pub fn move_marker(&mut self, instrument: usize, index: usize, frame: usize, new_step: bool) {
        let Some(s) = self.sample_at(instrument) else {
            return;
        };
        if index == 0 || index >= s.slices.len() {
            return;
        }
        let lo = s.slices[index - 1] + 1;
        let hi = s.slices.get(index + 1).map_or(s.frames.len(), |&n| n) - 1;
        let frame = frame.clamp(lo, hi.max(lo));
        if frame == s.slices[index] {
            return;
        }
        let mut slices = s.slices.clone();
        slices[index] = frame;
        let new = s.with_slices(slices);
        if new_step {
            self.snapshot();
        } else {
            self.changed();
        }
        self.replace_sample(instrument, new);
    }

    /// Find slices again with another sensitivity (0..=1). Slice numbers
    /// can't be mapped across, so cells keep their numbers.
    pub fn redetect(&mut self, instrument: usize, sensitivity: f32) {
        let Some(s) = self.sample_at(instrument) else {
            return;
        };
        let mut new = s.with_slices(vec![0]);
        new.detect_slices(sensitivity, 40.0);
        let n = new.slices.len();
        self.snapshot();
        self.replace_sample(instrument, new);
        self.message = Some(format!("sensitivity {sensitivity:.1}: {n} slices"));
    }

    fn bounce(&mut self) {
        let tracks = self.pattern().tracks.len();
        if tracks >= crate::engine::MAX_TRACKS {
            self.message = Some(format!("can't bounce: already {tracks} tracks"));
            return;
        }
        let rows = self.selection().unwrap_or(0..self.pattern().rows);
        let n = self.song.instruments.len();
        let name = format!("bounce{n}");
        let b = crate::bounce::bounce(&self.song, self.cursor.order, rows.clone(), &name);
        let slices = b.sample.slices.len();

        self.snapshot();
        self.song
            .instruments
            .push(Instrument::Sampler(std::sync::Arc::new(b.sample)));
        for p in &mut self.song.patterns {
            let cells = vec![Cell::default(); p.rows];
            p.tracks.push(crate::pattern::Track {
                name: format!("b{n}"),
                instrument: n,
                cells,
            });
        }
        self.song.rack.add_track(tracks);
        let p = self.pattern_index();
        for track in &mut self.song.patterns[p].tracks[..tracks] {
            track.cells[rows.clone()].fill(Cell::default());
        }
        let cells = &mut self.song.patterns[p].tracks[tracks].cells;
        for (slice, &r) in b.slice_rows.iter().enumerate() {
            cells[rows.start + r] = Cell::hit(slice as u16, 0x7f);
        }
        let patched = self
            .song
            .rack
            .track_device(tracks)
            .is_some_and(|d| self.song.rack.cables.iter().any(|c| c.from.device == d.id));
        self.cursor.track = tracks;
        self.cursor.row = rows.start;
        self.anchor = None;
        self.message = Some(if patched {
            format!(
                "bounced rows {:02X}-{:02X} to b{n}: {slices} slices",
                rows.start,
                rows.end - 1
            )
        } else {
            format!("bounced to b{n}, but no free mixer channel: patch it in the rack")
        });
    }

    fn undo_redo(&mut self, undo: bool) {
        let (from, to) = if undo {
            (&mut self.undo, &mut self.redo)
        } else {
            (&mut self.redo, &mut self.undo)
        };
        let Some(snap) = from.pop() else { return };
        let version = self.song.rack.version.max(snap.rack.version) + 1;
        to.push(Snapshot {
            patterns: std::mem::replace(&mut self.song.patterns, snap.patterns),
            order: std::mem::replace(&mut self.song.order, snap.order),
            automation: std::mem::replace(&mut self.song.automation, snap.automation),
            rack: std::mem::replace(&mut self.song.rack, snap.rack),
            instruments: std::mem::replace(&mut self.song.instruments, snap.instruments),
        });
        // A version the engine hasn't seen, so it rebuilds the graph even if
        // the restored rack had been sent before.
        self.song.rack.version = version;
        self.changed();
        self.clamp_cursor();
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
            rack: crate::rack::Rack::simple(2),
            tuning: Default::default(),
            automation: Vec::new(),
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
    fn undo_add_track_removes_its_rack_device() {
        let mut e = editor();
        e.apply(Action::AddTrack);
        assert_eq!(e.pattern().tracks.len(), 3);
        assert!(e.song.rack.track_device(2).is_some());
        e.cursor.track = 2;
        e.apply(Action::Undo);
        assert_eq!(e.pattern().tracks.len(), 2);
        assert!(e.song.rack.track_device(2).is_none());
        assert_eq!(e.cursor.track, 1, "cursor clamped to a track that exists");
        e.apply(Action::Redo);
        assert!(e.song.rack.track_device(2).is_some());
    }

    #[test]
    fn undo_rack_change_bumps_version() {
        let mut e = editor();
        let before = e.song.rack.clone();
        let mixer = before
            .devices
            .iter()
            .find(|d| d.kind == crate::rack::Kind::Mixer)
            .unwrap()
            .id;
        e.song.rack.remove(mixer);
        let removed_version = e.song.rack.version;
        e.record_rack(before.clone());
        e.apply(Action::Undo);
        assert!(e.song.rack.device(mixer).is_some());
        assert_eq!(e.song.rack.cables, before.cables);
        assert!(e.song.rack.version > removed_version.max(before.version));
        e.apply(Action::Redo);
        assert!(e.song.rack.device(mixer).is_none());
    }

    #[test]
    fn revision_counts_changes() {
        let mut e = editor();
        let r = e.revision;
        e.apply(Action::Down);
        assert_eq!(e.revision, r, "moving isn't a change");
        type_str(&mut e, "0");
        assert!(e.revision > r);
    }

    fn editor_with(slices: Vec<usize>) -> Editor {
        let mut e = editor();
        let s = Sample::new("s", 48_000, vec![[0.0; 2]; 1000]).with_slices(slices);
        e.song.instruments[0] = Instrument::Sampler(Arc::new(s));
        e
    }

    fn slices(e: &Editor) -> Vec<usize> {
        e.track_sample().unwrap().1.slices.clone()
    }

    fn cell_slices(e: &Editor) -> Vec<u16> {
        e.pattern().tracks[0]
            .cells
            .iter()
            .filter_map(|c| c.trigger.map(|t| t.slice))
            .collect()
    }

    #[test]
    fn inserting_a_marker_renumbers_later_cells() {
        let mut e = editor_with(vec![0, 200, 400]);
        type_str(&mut e, "000102");
        assert_eq!(e.insert_marker(0, 300), Some(2));
        assert_eq!(slices(&e), vec![0, 200, 300, 400]);
        assert_eq!(cell_slices(&e), vec![0, 1, 3], "old slice 2 is now 3");
        e.apply(Action::Undo);
        assert_eq!(slices(&e), vec![0, 200, 400]);
        assert_eq!(cell_slices(&e), vec![0, 1, 2]);
    }

    #[test]
    fn deleting_a_marker_merges_into_the_previous_slice() {
        let mut e = editor_with(vec![0, 200, 400]);
        type_str(&mut e, "000102");
        e.delete_marker(0, 1);
        assert_eq!(slices(&e), vec![0, 400]);
        assert_eq!(cell_slices(&e), vec![0, 0, 1]);
        e.delete_marker(0, 0);
        assert_eq!(slices(&e), vec![0, 400], "the first marker stays");
    }

    #[test]
    fn moving_a_marker_stays_between_neighbours_and_undoes_at_once() {
        let mut e = editor_with(vec![0, 200, 400]);
        e.move_marker(0, 1, 250, true);
        e.move_marker(0, 1, 300, false);
        e.move_marker(0, 1, 900, false);
        assert_eq!(slices(&e), vec![0, 399, 400]);
        e.apply(Action::Undo);
        assert_eq!(slices(&e), vec![0, 200, 400]);
    }

    #[test]
    fn added_sample_goes_on_the_cursor_track() {
        let mut e = editor();
        e.cursor.track = 1;
        e.add_sample(Sample::new("new", 48_000, vec![[0.0; 2]; 10]));
        assert_eq!(e.pattern().tracks[1].instrument, 1);
        assert_eq!(e.pattern().tracks[0].instrument, 0);
        e.apply(Action::NextInstrument);
        assert_eq!(e.pattern().tracks[1].instrument, 0);
        e.apply(Action::Undo);
        e.apply(Action::Undo);
        assert_eq!(e.song.instruments.len(), 1);
    }

    #[test]
    fn new_and_duplicate_patterns_go_after_the_current_slot() {
        let mut e = editor();
        type_str(&mut e, "05");
        e.apply(Action::DuplicatePattern);
        assert_eq!(e.song.order, vec![0, 1]);
        assert_eq!(e.cursor.order, 1);
        assert_eq!(e.pattern().tracks[0].cells[0].trigger.unwrap().slice, 5);
        e.apply(Action::NewPattern);
        assert_eq!(e.song.order, vec![0, 1, 2]);
        assert!(
            e.pattern()
                .tracks
                .iter()
                .all(|t| t.cells.iter().all(|c| *c == Cell::default()))
        );
        assert_eq!(e.pattern().tracks.len(), 2, "same tracks");
        e.apply(Action::Undo);
        e.apply(Action::Undo);
        assert_eq!(e.song.order, vec![0]);
    }

    #[test]
    fn order_slots() {
        let mut e = editor();
        e.apply(Action::RepeatSlot);
        assert_eq!(e.song.order, vec![0, 0]);
        e.apply(Action::NewPattern);
        e.go_to(0, 0);
        e.apply(Action::SlotPattern(1));
        assert_eq!(e.song.order, vec![1, 0, 1]);
        e.apply(Action::DeleteSlot);
        assert_eq!(e.song.order, vec![0, 1]);
        e.apply(Action::DeleteSlot);
        e.apply(Action::DeleteSlot);
        assert_eq!(e.song.order.len(), 1, "the last slot stays");
    }

    #[test]
    fn doubling_resolution_keeps_timing() {
        let mut e = editor();
        type_str(&mut e, "01");
        e.go_to(0, 3);
        type_str(&mut e, "02");
        e.apply(Action::Resolution(1));
        let p = e.pattern();
        assert_eq!((p.rows_per_beat, p.rows), (6, 24));
        let at: Vec<_> = p.tracks[0]
            .cells
            .iter()
            .enumerate()
            .filter_map(|(r, c)| c.trigger.map(|t| (r, t.slice)))
            .collect();
        assert_eq!(
            at,
            vec![(0, 1), (5, 2)],
            "row 3 of 4 per beat is row 4.5 of 6: rounds to 5"
        );
        e.apply(Action::Undo);
        assert_eq!((e.pattern().rows_per_beat, e.pattern().rows), (4, 16));
    }

    #[test]
    fn halving_resolution_drops_collisions() {
        let mut e = editor();
        // Rows 1 and 2 become rows 0.5 and 1: both round to row 1.
        e.go_to(0, 1);
        type_str(&mut e, "0102");
        e.apply(Action::Resolution(-2));
        let p = e.pattern();
        assert_eq!((p.rows_per_beat, p.rows), (2, 8));
        assert_eq!(
            p.tracks[0].cells[1].trigger.unwrap().slice,
            1,
            "the first one stays"
        );
        assert!(e.message.as_deref().unwrap().contains("1 cells"));
    }

    #[test]
    fn length_changes_by_rows() {
        let mut e = editor();
        e.go_to(0, 15);
        type_str(&mut e, "07");
        // Typing on the last row wrapped the cursor to the top.
        e.go_to(0, 15);
        e.apply(Action::Length(-2));
        assert_eq!(e.pattern().rows, 14);
        assert_eq!(e.cursor.row, 13);
        e.apply(Action::Length(4));
        assert_eq!(e.pattern().rows, 18);
        assert!(
            e.pattern().tracks[0].cells[15].trigger.is_none(),
            "cut off rows don't come back"
        );
        e.apply(Action::Undo);
        e.apply(Action::Undo);
        assert_eq!(e.pattern().tracks[0].cells[15].trigger.unwrap().slice, 7);
    }

    #[test]
    fn interpolate_fills_an_effect_column() {
        let mut e = editor();
        e.cursor.field = Field::Fx(0);
        type_str(&mut e, "k00");
        e.go_to(0, 4);
        e.cursor.field = Field::Fx(0);
        type_str(&mut e, "k80");
        e.go_to(0, 0);
        for _ in 0..4 {
            e.apply(Action::SelectDown);
        }
        e.cursor.field = Field::Fx(0);
        e.apply(Action::Interpolate);
        let ks: Vec<_> = e.pattern().tracks[0].cells[..6]
            .iter()
            .map(|c| c.fx[0])
            .collect();
        let want = [0x00, 0x20, 0x40, 0x60, 0x80].map(|n| Some(Fx::Knob(n)));
        assert_eq!(ks[..5], want);
        assert_eq!(ks[5], None);
        e.apply(Action::Undo);
        assert_eq!(e.pattern().tracks[0].cells[2].fx[0], None);
    }

    #[test]
    fn interpolate_signed_pitch_and_velocity() {
        let mut e = editor();
        e.cursor.field = Field::Fx(0);
        type_str(&mut e, "pf4"); // -12
        e.go_to(0, 2);
        e.cursor.field = Field::Fx(0);
        type_str(&mut e, "p0c"); // +12
        e.go_to(0, 0);
        e.apply(Action::SelectDown);
        e.apply(Action::SelectDown);
        e.cursor.field = Field::Fx(0);
        e.apply(Action::Interpolate);
        assert_eq!(e.pattern().tracks[0].cells[1].fx[0], Some(Fx::Pitch(0)));

        let mut e = editor();
        e.last_velocity = 0x10;
        type_str(&mut e, "01");
        e.go_to(0, 1);
        type_str(&mut e, "01");
        e.go_to(0, 3);
        e.last_velocity = 0x70;
        e.cursor.field = Field::Slice;
        type_str(&mut e, "01");
        e.go_to(0, 3);
        e.cursor.field = Field::Velocity;
        type_str(&mut e, "70");
        e.go_to(0, 0);
        for _ in 0..3 {
            e.apply(Action::SelectDown);
        }
        e.cursor.field = Field::Velocity;
        e.apply(Action::Interpolate);
        let v: Vec<_> = e.pattern().tracks[0].cells[..4]
            .iter()
            .map(|c| c.trigger.map(|t| t.velocity))
            .collect();
        assert_eq!(
            v,
            vec![Some(0x10), Some(0x30), None, Some(0x70)],
            "only cells with notes"
        );
    }

    #[test]
    fn automation_target_undoes() {
        let mut e = editor();
        e.cursor.track = 1;
        e.set_automation(Some(Target::Synth { param: 3 }));
        assert_eq!(e.song.automation(1), Some(Target::Synth { param: 3 }));
        assert_eq!(e.song.automation(0), None);
        e.apply(Action::Undo);
        assert_eq!(e.song.automation(1), None);
    }

    #[test]
    fn variations_go_after_the_slot_and_undo() {
        let mut e = editor();
        type_str(&mut e, "00010203");
        e.go_to(0, 0);
        e.apply(Action::Variations);
        assert_eq!(e.song.order, vec![0, 1, 2, 3, 4]);
        assert_eq!(e.cursor.order, 0, "stays on the original");
        let original = e.song.patterns[0].tracks[1].cells.clone();
        assert!(
            e.song.patterns[1..]
                .iter()
                .all(|p| p.tracks[1].cells == original),
            "other tracks untouched"
        );
        let seed = e.seed;
        e.apply(Action::Undo);
        assert_eq!(e.song.order, vec![0]);
        assert_eq!(e.song.patterns.len(), 1);
        assert_eq!(e.seed, seed, "a new seed next time, even after undo");
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
