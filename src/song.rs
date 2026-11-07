//! Song data model: plain data, no audio code. The UI edits this, the engine
//! plays it, transforms rewrite it.
//!
//! A song is a stack of tracks (lanes), each holding clips on a shared
//! timeline. A clip is a run of cells on its own grid: one cell per row,
//! `rows_per_beat` rows to the beat. Time is counted in [`Tick`]s.

use std::ops::Range;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::engine::MAX_TRACKS;
use crate::rack::{DeviceId, Rack};
use crate::sample::Sample;
use crate::synthvoice::SynthPatch;
use crate::tuning::Tuning;

/// Effect columns per cell.
pub const MAX_FX: usize = 2;

/// Per-cell effect commands, applied when the cell plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Fx {
    /// Start `n/256` of the way into the slice.
    Offset(u8),
    /// Retrigger the note `n` times, evenly spread over the row.
    Retrig(u8),
    /// Play the slice backwards.
    Reverse,
    /// Transpose by this many semitones (playback rate, like a sampler).
    Pitch(i8),
    /// Reduce to this many bits (1..=16).
    Crush(u8),
    /// Loop the first `n` milliseconds of the slice.
    Stutter(u8),
    /// Cut the note `n/16` of the way through the row. Also cuts a note that
    /// is still ringing when the cell has no trigger. Synths release.
    Cut(u8),
    /// Synths: slide from the held note to this one over `n` × 4 ms,
    /// without restarting the envelopes.
    Glide(u8),
    /// Synths: also play the notes `x` and `y` semitones above, for
    /// `n = 0xXY`. `H47` is a major chord; a 0 nibble adds nothing.
    Chord(u8),
    /// Set the track's automation target (see [`Target`]) to `n/FF` of
    /// its range.
    Knob(u8),
    /// Synths: the sustain pedal. [`PEDAL_UP`], [`PEDAL_DOWN`], or
    /// [`PEDAL_CHANGE`]: up and straight back down, so what rang on stops
    /// and this row's notes ring on. It stays as set until the next pedal
    /// in the clip, or the clip's end.
    Pedal(u8),
}

pub const PEDAL_UP: u8 = 0;
pub const PEDAL_DOWN: u8 = 1;
pub const PEDAL_CHANGE: u8 = 2;

/// Whether the sustain pedal is down at `row` of a clip of `cells`, by its
/// last pedal at or before `row`.
pub fn pedal_at(cells: &[Cell], row: usize) -> bool {
    cells[..=row.min(cells.len().saturating_sub(1))]
        .iter()
        .rev()
        .find_map(Cell::pedal)
        .is_some_and(|p| p != PEDAL_UP)
}

/// The knob a track's `Knob` effects turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// A knob in the rack.
    Rack { device: DeviceId, param: usize },
    /// A knob of the synth the track plays.
    Synth { param: usize },
}

/// Where the pedal is down, row by row, and the rows where it's lifted and
/// put straight back down.
pub fn pedal_rows(cells: &[Cell]) -> (Vec<bool>, Vec<bool>) {
    let mut down = vec![false; cells.len()];
    let mut change = vec![false; cells.len()];
    let mut now = false;
    for (r, c) in cells.iter().enumerate() {
        match c.pedal() {
            Some(PEDAL_UP) => now = false,
            Some(PEDAL_CHANGE) => {
                change[r] = now;
                now = true;
            }
            Some(_) => now = true,
            None => {}
        }
        down[r] = now;
    }
    (down, change)
}

/// `cells` with their pedals set to match `down` and `change` (as
/// [`pedal_rows`] gives them). A cell whose effect slots are full gives
/// up its last one.
pub fn with_pedal_rows(cells: &[Cell], down: &[bool], change: &[bool]) -> Vec<Cell> {
    let mut out = cells.to_vec();
    for (r, c) in out.iter_mut().enumerate() {
        for f in &mut c.fx {
            if matches!(f, Some(Fx::Pedal(_))) {
                *f = None;
            }
        }
        let before = r > 0 && down[r - 1];
        let want = match (down[r], before) {
            (true, false) => Some(PEDAL_DOWN),
            (true, true) if change[r] => Some(PEDAL_CHANGE),
            (false, true) => Some(PEDAL_UP),
            _ => None,
        };
        if let Some(p) = want {
            if c.fx.iter().all(Option::is_some) {
                c.fx[MAX_FX - 1] = None;
            }
            *c = c.with_fx(Fx::Pedal(p));
        }
    }
    out
}

impl Fx {
    /// The semitones a `Chord` adds above the root.
    pub fn chord_intervals(n: u8) -> impl Iterator<Item = u8> {
        [n >> 4, n & 0x0f].into_iter().filter(|&i| i > 0)
    }
}

/// Most notes (or hits) one cell can start.
pub const MAX_NOTES: usize = 4;

/// A note: a slice of a sample, or a synth's note number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    pub slice: u16,
    /// 0..=127
    pub velocity: u8,
    /// How many rows it lasts. 0 is until something stops it: on a mono
    /// track the next notes, on a poly track a cut (or, for a sample, the
    /// end of its slice).
    #[serde(default)]
    pub rows: u16,
}

impl Trigger {
    pub fn new(slice: u16, velocity: u8) -> Self {
        Trigger {
            slice,
            velocity,
            rows: 0,
        }
    }
}

/// One row of a clip: the notes it starts, and its effects, which apply to
/// all of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// Filled from the front.
    pub notes: [Option<Trigger>; MAX_NOTES],
    pub fx: [Option<Fx>; MAX_FX],
}

impl Cell {
    pub fn hit(slice: u16, velocity: u8) -> Self {
        Cell::default().with_note(Trigger::new(slice, velocity))
    }

    /// The cell with another note added, if there's room.
    pub fn with_note(mut self, note: Trigger) -> Self {
        self.add(note);
        self
    }

    /// The notes it starts, in order.
    pub fn hits(&self) -> impl Iterator<Item = Trigger> + '_ {
        self.notes.iter().flatten().copied()
    }

    pub fn has_hit(&self) -> bool {
        self.notes[0].is_some()
    }

    /// The first note, which leads a chord.
    pub fn first(&self) -> Option<Trigger> {
        self.notes[0]
    }

    pub fn note(&self, slice: u16) -> Option<Trigger> {
        self.hits().find(|n| n.slice == slice)
    }

    pub fn note_mut(&mut self, slice: u16) -> Option<&mut Trigger> {
        self.notes.iter_mut().flatten().find(|n| n.slice == slice)
    }

    /// Add a note, or replace the one on the same slice. Fails if the cell
    /// is full.
    pub fn add(&mut self, note: Trigger) -> bool {
        if let Some(n) = self.note_mut(note.slice) {
            *n = note;
            return true;
        }
        match self.notes.iter_mut().find(|n| n.is_none()) {
            Some(slot) => {
                *slot = Some(note);
                true
            }
            None => false,
        }
    }

    /// Remove the note on `slice`, keeping the rest in order.
    pub fn remove(&mut self, slice: u16) -> bool {
        let Some(i) = self
            .notes
            .iter()
            .position(|n| n.is_some_and(|n| n.slice == slice))
        else {
            return false;
        };
        self.notes.copy_within(i + 1.., i);
        self.notes[MAX_NOTES - 1] = None;
        true
    }

    pub fn clear_notes(&mut self) {
        self.notes = [None; MAX_NOTES];
    }

    pub fn with_fx(mut self, fx: Fx) -> Self {
        if let Some(slot) = self.fx.iter_mut().find(|f| f.is_none()) {
            *slot = Some(fx);
        }
        self
    }

    /// The cell's pedal, if it has one.
    pub fn pedal(&self) -> Option<u8> {
        self.fx().find_map(|f| match f {
            Fx::Pedal(p) => Some(p),
            _ => None,
        })
    }

    pub fn fx(&self) -> impl Iterator<Item = Fx> + '_ {
        self.fx.iter().flatten().copied()
    }
}

#[derive(Clone, Debug)]
pub enum Instrument {
    Sampler(Arc<Sample>),
    /// A synth voice. Its trigger's `slice` is the note number.
    Synth(Arc<SynthPatch>),
}

impl Instrument {
    pub fn name(&self) -> &str {
        match self {
            Instrument::Sampler(s) => &s.name,
            Instrument::Synth(p) => &p.name,
        }
    }

    pub fn is_synth(&self) -> bool {
        matches!(self, Instrument::Synth(_))
    }
}

/// Timeline position. [`TICKS_PER_BEAT`] to the beat.
pub type Tick = u64;

/// Divisible by every entry in [`RESOLUTIONS`], so every row of every clip
/// starts on a whole tick.
pub const TICKS_PER_BEAT: Tick = 192;
pub const BEATS_PER_BAR: Tick = 4;
pub const TICKS_PER_BAR: Tick = TICKS_PER_BEAT * BEATS_PER_BAR;

/// The grids a clip can have, in rows per beat.
pub const RESOLUTIONS: [u32; 12] = [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64];

/// The nearest resolution to `rows_per_beat`.
pub fn nearest_resolution(rows_per_beat: u32) -> u32 {
    *RESOLUTIONS
        .iter()
        .min_by_key(|&&r| r.abs_diff(rows_per_beat))
        .unwrap()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub start: Tick,
    /// One of [`RESOLUTIONS`].
    pub rows_per_beat: u32,
    /// One cell per row. The clip is as long as its rows.
    pub cells: Vec<Cell>,
}

impl Clip {
    pub fn new(start: Tick, rows_per_beat: u32, rows: usize) -> Self {
        Clip {
            start,
            rows_per_beat,
            cells: vec![Cell::default(); rows],
        }
    }

    /// A clip `beats` long.
    pub fn beats(start: Tick, rows_per_beat: u32, beats: u32) -> Self {
        Clip::new(start, rows_per_beat, (rows_per_beat * beats) as usize)
    }

    pub fn ticks_per_row(&self) -> Tick {
        TICKS_PER_BEAT / self.rows_per_beat.max(1) as Tick
    }

    pub fn len(&self) -> Tick {
        self.cells.len() as Tick * self.ticks_per_row()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn end(&self) -> Tick {
        self.start + self.len()
    }

    pub fn span(&self) -> Range<Tick> {
        self.start..self.end()
    }

    /// The row that starts exactly at `tick`.
    pub fn row_at(&self, tick: Tick) -> Option<usize> {
        let offset = tick.checked_sub(self.start)?;
        let tpr = self.ticks_per_row();
        let row = (offset / tpr) as usize;
        (offset % tpr == 0 && row < self.cells.len()).then_some(row)
    }

    /// When `row` starts.
    pub fn row_tick(&self, row: usize) -> Tick {
        self.start + row as Tick * self.ticks_per_row()
    }
}

#[derive(Clone, Debug)]
pub struct Track {
    pub name: String,
    pub instrument: usize,
    pub mute: bool,
    pub solo: bool,
    /// Off (mono): a row's notes play until the next row with notes (or a
    /// cut) takes over, as when chopping a break or sliding a bassline. On:
    /// each note plays out by itself, as with a drum kit or a pad.
    pub poly: bool,
    /// The knob this track's `Knob` effects turn.
    pub automation: Option<Target>,
    /// Sorted by start, and never overlapping.
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn new(name: &str, instrument: usize) -> Self {
        Track {
            name: name.to_owned(),
            instrument,
            mute: false,
            solo: false,
            poly: false,
            automation: None,
            clips: Vec::new(),
        }
    }

    /// Add a clip, for building songs in code. Panics if it overlaps.
    pub fn with_clip(mut self, clip: Clip) -> Self {
        self.insert(clip).expect("clips mustn't overlap");
        self
    }

    /// The clip playing at `tick`.
    pub fn clip_at(&self, tick: Tick) -> Option<&Clip> {
        let i = self.clips.partition_point(|c| c.start <= tick);
        let clip = self.clips.get(i.checked_sub(1)?)?;
        (tick < clip.end()).then_some(clip)
    }

    /// Whether `span` is free of clips, apart from clip `except`.
    pub fn is_free(&self, span: Range<Tick>, except: Option<usize>) -> bool {
        self.clips
            .iter()
            .enumerate()
            .all(|(i, c)| Some(i) == except || c.end() <= span.start || span.end <= c.start)
    }

    /// Add a clip in start order, returning its index. Fails, handing the
    /// clip back, if it would overlap another.
    pub fn insert(&mut self, clip: Clip) -> Result<usize, Clip> {
        if clip.is_empty() || !self.is_free(clip.span(), None) {
            return Err(clip);
        }
        let i = self.clips.partition_point(|c| c.start < clip.start);
        self.clips.insert(i, clip);
        Ok(i)
    }

    pub fn end(&self) -> Tick {
        self.clips.last().map_or(0, Clip::end)
    }
}

#[derive(Clone, Debug)]
pub struct Song {
    pub bpm: f64,
    pub instruments: Vec<Instrument>,
    /// Top to bottom. Track `t` plays through the rack's `Track(t)` device.
    pub tracks: Vec<Track>,
    /// While set, playback loops this range instead of the whole song.
    pub loop_range: Option<Range<Tick>>,
    pub rack: Rack,
    /// Which frequency each note plays, for synths.
    pub tuning: Tuning,
}

impl Song {
    pub fn new(bpm: f64, instruments: Vec<Instrument>, tracks: Vec<Track>) -> Self {
        let rack = Rack::simple(tracks.len());
        Song {
            bpm,
            instruments,
            tracks,
            loop_range: None,
            rack,
            tuning: Tuning::default(),
        }
    }

    /// Where the song ends: after its last clip, rounded up to a whole bar.
    /// At least one bar, so an empty song still has somewhere to play.
    pub fn end(&self) -> Tick {
        let last = self.tracks.iter().map(Track::end).max().unwrap_or(0);
        last.div_ceil(TICKS_PER_BAR).max(1) * TICKS_PER_BAR
    }

    pub fn automation(&self, track: usize) -> Option<Target> {
        self.tracks.get(track).and_then(|t| t.automation)
    }

    /// Whether a track is heard: not muted, and soloed if any track is.
    pub fn audible(&self, track: usize) -> bool {
        let solo = self.tracks.iter().any(|t| t.solo);
        self.tracks
            .get(track)
            .is_some_and(|t| !t.mute && (t.solo || !solo))
    }

    /// Seconds per tick.
    pub fn tick_seconds(&self) -> f64 {
        60.0 / (self.bpm.max(1.0) * TICKS_PER_BEAT as f64)
    }

    pub fn clip(&self, at: ClipRef) -> Option<&Clip> {
        self.tracks.get(at.0)?.clips.get(at.1)
    }

    /// Move a clip to `start` on `track`. Fails, changing nothing, if it
    /// would overlap a clip there.
    pub fn move_clip(&mut self, at: ClipRef, track: usize, start: Tick) -> Option<ClipRef> {
        let mut clip = self.clip(at)?.clone();
        clip.start = start;
        let to = self.tracks.get(track)?;
        let except = (track == at.0).then_some(at.1);
        if !to.is_free(clip.span(), except) {
            return None;
        }
        self.tracks[at.0].clips.remove(at.1);
        let i = self.tracks[track].insert(clip).ok()?;
        Some((track, i))
    }

    /// Make a clip `rows` long: cut off at the end, or with empty rows
    /// added. Fails if it would run into the next clip.
    pub fn resize_clip(&mut self, at: ClipRef, rows: usize) -> bool {
        let Some(clip) = self.clip(at) else {
            return false;
        };
        let end = clip.start + rows as Tick * clip.ticks_per_row();
        if rows == 0 || !self.tracks[at.0].is_free(clip.start..end, Some(at.1)) {
            return false;
        }
        self.tracks[at.0].clips[at.1]
            .cells
            .resize(rows, Cell::default());
        true
    }

    /// A new empty clip at `start`, up to `rows` long but cut short to end
    /// where the next clip starts.
    pub fn new_clip(
        &mut self,
        track: usize,
        start: Tick,
        rows_per_beat: u32,
        rows: usize,
    ) -> Option<ClipRef> {
        let t = self.tracks.get_mut(track)?;
        if t.clip_at(start).is_some() {
            return None;
        }
        let mut clip = Clip::new(start, rows_per_beat, rows);
        if let Some(next) = t.clips.iter().find(|c| c.start > start) {
            let room = ((next.start - start) / clip.ticks_per_row()) as usize;
            clip.cells.truncate(room);
        }
        t.insert(clip).ok().map(|i| (track, i))
    }

    /// Change a clip's grid, keeping when its hits play: each lands on the
    /// nearest row of the new grid (the first one wins if two land on the
    /// same row). The clip keeps its length, rounded up to whole rows.
    pub fn set_resolution(&mut self, at: ClipRef, rows_per_beat: u32) -> bool {
        let Some(clip) = self.clip(at) else {
            return false;
        };
        if !RESOLUTIONS.contains(&rows_per_beat) {
            return false;
        }
        let old_tpr = clip.ticks_per_row();
        let tpr = TICKS_PER_BEAT / rows_per_beat as Tick;
        let rows = clip.len().div_ceil(tpr).max(1) as usize;
        let end = clip.start + rows as Tick * tpr;
        if !self.tracks[at.0].is_free(clip.start..end, Some(at.1)) {
            return false;
        }
        let mut cells = vec![Cell::default(); rows];
        let mut taken = vec![false; rows];
        for (row, cell) in clip.cells.iter().enumerate() {
            if *cell == Cell::default() {
                continue;
            }
            let tick = row as Tick * old_tpr;
            let new = ((tick + tpr / 2) / tpr).min(rows as Tick - 1) as usize;
            if !taken[new] {
                cells[new] = *cell;
                taken[new] = true;
            }
        }
        let clip = &mut self.tracks[at.0].clips[at.1];
        clip.rows_per_beat = rows_per_beat;
        clip.cells = cells;
        true
    }

    /// A copy of a clip, straight after it. Fails if there's no room.
    pub fn duplicate_clip(&mut self, at: ClipRef) -> Option<ClipRef> {
        let mut copy = self.clip(at)?.clone();
        copy.start = copy.end();
        self.tracks[at.0].insert(copy).ok().map(|i| (at.0, i))
    }

    pub fn delete_clip(&mut self, at: ClipRef) -> bool {
        if self.clip(at).is_none() {
            return false;
        }
        self.tracks[at.0].clips.remove(at.1);
        true
    }

    /// A new empty track at the bottom, with its rack device.
    pub fn add_track(&mut self, name: &str, instrument: usize) -> Option<usize> {
        let t = self.tracks.len();
        if t >= MAX_TRACKS {
            return None;
        }
        self.tracks.push(Track::new(name, instrument));
        self.rack.insert_track(t);
        Some(t)
    }

    /// A copy of a track, clips and all, just below it. Its rack device
    /// starts with the same settings and goes into a free mixer channel. A
    /// synth gets its own copy of the patch, so turning one's knobs leaves
    /// the other alone; samples don't change, so the copy shares them.
    pub fn duplicate_track(&mut self, track: usize) -> Option<usize> {
        if self.tracks.len() >= MAX_TRACKS {
            return None;
        }
        let mut copy = self.tracks.get(track)?.clone();
        copy.name.push_str(" copy");
        copy.solo = false;
        if let Some(Instrument::Synth(patch)) = self.instruments.get(copy.instrument) {
            let mut patch = SynthPatch::clone(patch);
            patch.name.push_str(" copy");
            self.instruments.push(Instrument::Synth(Arc::new(patch)));
            copy.instrument = self.instruments.len() - 1;
        }
        let t = track + 1;
        self.tracks.insert(t, copy);
        let params = self.rack.track_device(track).map(|d| d.params.clone());
        let id = self.rack.insert_track(t);
        if let Some(params) = params {
            for (i, v) in params.into_iter().enumerate() {
                self.rack.set_param(id, i, v);
            }
        }
        Some(t)
    }

    /// Move a track up or down to `to`, the ones in between shifting over.
    /// Its rack device goes with it.
    pub fn move_track(&mut self, from: usize, to: usize) -> bool {
        let n = self.tracks.len();
        if from >= n || to >= n {
            return false;
        }
        let track = self.tracks.remove(from);
        self.tracks.insert(to, track);
        self.rack.move_track(from, to);
        true
    }

    pub fn delete_track(&mut self, track: usize) -> bool {
        if track >= self.tracks.len() {
            return false;
        }
        self.tracks.remove(track);
        self.rack.remove_track(track);
        true
    }

    /// A track's volume: its rack device's volume knob.
    pub fn volume(&self, track: usize) -> f32 {
        self.rack
            .track_device(track)
            .and_then(|d| d.params.get(TRACK_VOLUME).copied())
            .unwrap_or(1.0)
    }

    /// Set a track's volume, returning the device to tell the engine about.
    pub fn set_volume(&mut self, track: usize, volume: f32) -> Option<DeviceId> {
        let id = self.rack.track_device(track)?.id;
        self.rack.set_param(id, TRACK_VOLUME, volume);
        Some(id)
    }
}

/// A clip by (track, index in the track).
pub type ClipRef = (usize, usize);

/// The track device's volume knob.
pub const TRACK_VOLUME: usize = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_hold_several_notes() {
        let mut c = Cell::hit(60, 100);
        assert!(c.add(Trigger::new(64, 90)));
        assert!(c.add(Trigger::new(60, 70)), "same note: replaced");
        assert_eq!(
            c.hits().map(|n| (n.slice, n.velocity)).collect::<Vec<_>>(),
            [(60, 70), (64, 90)]
        );
        assert!(c.add(Trigger::new(67, 1)) && c.add(Trigger::new(71, 1)));
        assert!(!c.add(Trigger::new(72, 1)), "full");
        assert!(c.remove(60));
        assert_eq!(c.first().map(|n| n.slice), Some(64), "the rest move up");
        assert!(!c.remove(60));
        c.clear_notes();
        assert!(!c.has_hit());
    }

    #[test]
    fn every_resolution_divides_a_beat() {
        for r in RESOLUTIONS {
            assert_eq!(TICKS_PER_BEAT % r as Tick, 0, "{r} rows per beat");
        }
        assert_eq!(nearest_resolution(7), 6);
        assert_eq!(nearest_resolution(100), 64);
    }

    #[test]
    fn rows_land_on_their_ticks() {
        let clip = Clip::beats(TICKS_PER_BEAT, 4, 2);
        assert_eq!(clip.len(), 2 * TICKS_PER_BEAT);
        assert_eq!(clip.row_at(TICKS_PER_BEAT), Some(0));
        assert_eq!(clip.row_at(TICKS_PER_BEAT + 48), Some(1));
        assert_eq!(clip.row_at(TICKS_PER_BEAT + 47), None);
        assert_eq!(clip.row_at(3 * TICKS_PER_BEAT), None, "past the end");
        assert_eq!(clip.row_at(0), None, "before the start");
        assert_eq!(clip.row_tick(1), TICKS_PER_BEAT + 48);
    }

    #[test]
    fn clips_stay_sorted_and_apart() {
        let mut t = Track::new("t", 0);
        assert_eq!(t.insert(Clip::beats(800, 4, 4)), Ok(0));
        assert_eq!(t.insert(Clip::beats(0, 4, 4)), Ok(0));
        assert!(t.insert(Clip::beats(700, 4, 1)).is_err(), "overlaps");
        assert!(t.insert(Clip::new(2000, 4, 0)).is_err(), "empty");
        assert_eq!(t.clips[0].start, 0);
        assert_eq!(t.clip_at(767).map(|c| c.start), Some(0));
        assert_eq!(t.clip_at(768), None, "the gap");
        assert_eq!(t.clip_at(800).map(|c| c.start), Some(800));
        assert_eq!(t.end(), 800 + 768);
    }

    #[test]
    fn song_ends_on_a_bar_line() {
        let mut song = Song::new(120.0, Vec::new(), vec![Track::new("t", 0)]);
        assert_eq!(song.end(), TICKS_PER_BAR, "empty: one bar");
        song.tracks[0].insert(Clip::beats(0, 4, 5)).unwrap();
        assert_eq!(song.end(), 2 * TICKS_PER_BAR);
    }

    fn two_track_song() -> Song {
        // A beat, a gap, then a bar.
        let a = Track::new("a", 0)
            .with_clip(Clip::beats(0, 4, 1))
            .with_clip(Clip::beats(TICKS_PER_BAR, 4, 4));
        Song::new(120.0, Vec::new(), vec![a, Track::new("b", 0)])
    }

    #[test]
    fn clips_move_within_and_between_tracks() {
        let mut song = two_track_song();
        assert_eq!(
            song.move_clip((0, 1), 0, TICKS_PER_BEAT / 2),
            None,
            "overlaps"
        );
        assert_eq!(song.tracks[0].clips[1].start, TICKS_PER_BAR, "unchanged");
        assert_eq!(
            song.move_clip((0, 0), 0, 3 * TICKS_PER_BAR),
            Some((0, 1)),
            "now last"
        );
        assert_eq!(song.tracks[0].clips[0].start, TICKS_PER_BAR);
        assert_eq!(song.move_clip((0, 1), 1, 0), Some((1, 0)));
        assert_eq!(song.tracks[0].clips.len(), 1);
        assert_eq!(song.tracks[1].clips[0].start, 0);
        assert_eq!(song.move_clip((0, 0), 5, 0), None, "no such track");
    }

    #[test]
    fn clips_resize_but_not_into_the_next() {
        let mut song = two_track_song();
        assert!(song.resize_clip((0, 0), 8));
        assert_eq!(song.tracks[0].clips[0].cells.len(), 8);
        assert!(song.resize_clip((0, 0), 16), "up to the next clip");
        assert!(!song.resize_clip((0, 0), 17));
        assert!(!song.resize_clip((0, 0), 0));
        assert!(song.resize_clip((0, 1), 64), "the last can grow freely");
    }

    #[test]
    fn new_clips_fit_into_gaps() {
        let mut song = two_track_song();
        assert_eq!(
            song.new_clip(0, TICKS_PER_BEAT / 2, 4, 16),
            None,
            "inside a clip"
        );
        // The first clip is a beat long, so a bar from beat 2 is cut short
        // at the second clip.
        assert_eq!(song.new_clip(0, 2 * TICKS_PER_BEAT, 4, 16), Some((0, 1)));
        assert_eq!(song.tracks[0].clips[1].cells.len(), 8);
        assert_eq!(song.new_clip(1, 0, 4, 16), Some((1, 0)));
    }

    #[test]
    fn duplicates_go_right_after_if_there_is_room() {
        let mut song = two_track_song();
        assert_eq!(song.duplicate_clip((0, 1)), Some((0, 2)));
        assert_eq!(song.tracks[0].clips[2].start, 2 * TICKS_PER_BAR);
        assert_eq!(song.duplicate_clip((0, 1)), None, "the copy is in the way");
        assert!(song.delete_clip((0, 2)));
        assert!(!song.delete_clip((0, 2)));
    }

    #[test]
    fn resolution_changes_keep_the_timing() {
        let mut clip = Clip::beats(0, 4, 2);
        clip.cells[0] = Cell::hit(1, 100);
        clip.cells[3] = Cell::hit(2, 100);
        clip.cells[6] = Cell::hit(3, 100);
        let mut song = Song::new(120.0, Vec::new(), vec![Track::new("a", 0).with_clip(clip)]);
        assert!(song.set_resolution((0, 0), 8));
        let c = &song.tracks[0].clips[0];
        assert_eq!((c.rows_per_beat, c.cells.len(), c.len()), (8, 16, 384));
        let hits: Vec<_> = (0..16).filter(|&r| c.cells[r].has_hit()).collect();
        assert_eq!(hits, vec![0, 6, 12]);
        // Down to a beat grid: rows 0, 3 and 6 of 16ths are beats 0, 0.75
        // and 1.5, which round to 0, 1 and 2; the clip stays two beats long
        // but there are only rows 0 and 1, so the last hit lands on row 1,
        // where the second already is.
        assert!(song.set_resolution((0, 0), 1));
        let c = &song.tracks[0].clips[0];
        assert_eq!(c.cells.len(), 2);
        assert_eq!(c.cells[0].first().unwrap().slice, 1);
        assert_eq!(c.cells[1].first().unwrap().slice, 2);
        assert!(!song.set_resolution((0, 0), 5), "not a resolution");
    }

    #[test]
    fn tracks_come_and_go_with_their_rack_devices() {
        let mut song = two_track_song();
        song.set_volume(0, 0.5);
        assert_eq!(song.duplicate_track(0), Some(1));
        assert_eq!(song.tracks[1].name, "a copy");
        assert_eq!(song.tracks[1].clips, song.tracks[0].clips);
        assert_eq!(song.volume(1), 0.5, "same settings");
        assert_eq!(song.tracks[2].name, "b");
        assert!(song.rack.track_device(2).is_some());
        assert!(song.delete_track(0));
        assert_eq!(song.tracks[0].name, "a copy");
        assert_eq!(song.volume(0), 0.5, "its device moved up with it");
        assert_eq!(song.add_track("c", 0), Some(2));
        assert!(song.rack.track_device(2).is_some());
        assert!(song.rack.track_device(3).is_none());
        while song.add_track("more", 0).is_some() {}
        assert_eq!(song.tracks.len(), MAX_TRACKS);
        assert_eq!(song.duplicate_track(0), None);
    }

    #[test]
    fn tracks_move_with_their_rack_devices() {
        let names = |s: &Song| s.tracks.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        let mut song = Song::new(
            120.0,
            Vec::new(),
            ["a", "b", "c", "d"].map(|n| Track::new(n, 0)).to_vec(),
        );
        for t in 0..4 {
            song.set_volume(t, t as f32 / 10.0);
        }
        assert!(song.move_track(0, 2));
        assert_eq!(names(&song), ["b", "c", "a", "d"]);
        let volumes: Vec<f32> = (0..4).map(|t| song.volume(t)).collect();
        assert_eq!(
            volumes,
            [0.1, 0.2, 0.0, 0.3],
            "each device went with its track"
        );
        assert!(song.move_track(3, 0));
        assert_eq!(names(&song), ["d", "b", "c", "a"]);
        assert!(song.move_track(1, 1), "staying put is fine");
        assert!(!song.move_track(0, 4));
        assert!(!song.move_track(4, 0));
    }

    #[test]
    fn the_pedal_is_whatever_the_last_one_before_said() {
        let mut cells = vec![Cell::default(); 8];
        assert!(!pedal_at(&cells, 3), "up when there's none");
        cells[2] = Cell::default().with_fx(Fx::Pedal(PEDAL_DOWN));
        cells[5] = Cell::hit(60, 100).with_fx(Fx::Pedal(PEDAL_UP));
        cells[6] = Cell::default().with_fx(Fx::Pedal(PEDAL_CHANGE));
        let at: Vec<bool> = (0..8).map(|r| pedal_at(&cells, r)).collect();
        assert_eq!(at, [false, false, true, true, true, false, true, true]);
        assert!(pedal_at(&cells, 99), "past the end: the last one");
    }

    #[test]
    fn a_duplicated_synth_track_gets_its_own_patch() {
        let acid = SynthPatch::new(crate::synthvoice::SynthKind::Acid, "acid");
        let instruments = vec![Instrument::Synth(Arc::new(acid))];
        let mut song = Song::new(120.0, instruments, vec![Track::new("bass", 0)]);
        assert_eq!(song.duplicate_track(0), Some(1));
        assert_eq!(song.tracks[0].instrument, 0);
        assert_eq!(song.tracks[1].instrument, 1);
        assert_eq!(song.instruments[1].name(), "acid copy");
        let Instrument::Synth(copy) = &mut song.instruments[1] else {
            panic!("not a synth");
        };
        Arc::make_mut(copy).params[0] = -1.0;
        let Instrument::Synth(original) = &song.instruments[0] else {
            panic!("not a synth");
        };
        assert_ne!(original.params[0], -1.0, "the original keeps its knobs");
    }

    #[test]
    fn solo_silences_the_others() {
        let mut song = Song::new(
            120.0,
            Vec::new(),
            vec![Track::new("a", 0), Track::new("b", 0)],
        );
        assert!(song.audible(0) && song.audible(1));
        song.tracks[1].solo = true;
        assert!(!song.audible(0) && song.audible(1));
        song.tracks[1].mute = true;
        assert!(!song.audible(1), "mute beats solo");
        assert!(!song.audible(2), "no such track");
    }
}
