//! Song data model: plain data, no audio code. The UI edits this, the engine
//! plays it, transforms rewrite it.
//!
//! A song is a stack of tracks (lanes), each holding clips on a shared
//! timeline. A clip is a run of cells on its own grid: one cell per row,
//! `rows_per_beat` rows to the beat. Time is counted in [`Tick`]s.

use std::ops::Range;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::rack::{DeviceId, Rack};
use crate::sample::Sample;
use crate::synthvoice::SynthPatch;
use crate::tuning::Tuning;

/// Effect columns per cell.
pub const MAX_FX: usize = 2;

/// Per-cell effect commands. These are what the tracker shows as `O3C`,
/// `R18`, `Rv1` and so on.
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
}

/// The knob a track's `Knob` effects turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// A knob in the rack.
    Rack { device: DeviceId, param: usize },
    /// A knob of the synth the track plays.
    Synth { param: usize },
}

impl Fx {
    /// The semitones a `Chord` adds above the root.
    pub fn chord_intervals(n: u8) -> impl Iterator<Item = u8> {
        [n >> 4, n & 0x0f].into_iter().filter(|&i| i > 0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    pub slice: u16,
    /// 0..=127
    pub velocity: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub trigger: Option<Trigger>,
    pub fx: [Option<Fx>; MAX_FX],
}

impl Cell {
    pub fn hit(slice: u16, velocity: u8) -> Self {
        Cell {
            trigger: Some(Trigger { slice, velocity }),
            fx: [None; MAX_FX],
        }
    }

    pub fn with_fx(mut self, fx: Fx) -> Self {
        if let Some(slot) = self.fx.iter_mut().find(|f| f.is_none()) {
            *slot = Some(fx);
        }
        self
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
