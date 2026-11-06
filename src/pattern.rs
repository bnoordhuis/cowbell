//! Song data model: plain data, no audio code. The UI edits this, the engine
//! plays it, transforms rewrite it.

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

#[derive(Clone, Debug)]
pub struct Track {
    pub name: String,
    pub instrument: usize,
    /// One cell per row; always `Pattern::rows` long.
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug)]
pub struct Pattern {
    /// Resolution of this pattern. Different patterns can use different
    /// resolutions, so one bar can be edited in 64ths and the next in 16ths.
    pub rows_per_beat: u32,
    pub rows: usize,
    pub tracks: Vec<Track>,
}

impl Pattern {
    pub fn new(rows_per_beat: u32, beats: u32, track_names: &[(&str, usize)]) -> Self {
        let rows = (rows_per_beat * beats) as usize;
        let tracks = track_names
            .iter()
            .map(|&(name, instrument)| Track {
                name: name.to_owned(),
                instrument,
                cells: vec![Cell::default(); rows],
            })
            .collect();
        Pattern {
            rows_per_beat,
            rows,
            tracks,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Song {
    pub bpm: f64,
    pub instruments: Vec<Instrument>,
    pub patterns: Vec<Pattern>,
    /// Indices into `patterns`, played in sequence and looped.
    pub order: Vec<usize>,
    pub rack: Rack,
    /// Which frequency each note plays, for synths.
    pub tuning: Tuning,
    /// Per track (by column): the knob its `Knob` effects turn.
    pub automation: Vec<Option<Target>>,
}

impl Song {
    pub fn automation(&self, track: usize) -> Option<Target> {
        self.automation.get(track).copied().flatten()
    }
}
