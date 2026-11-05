//! Transforms rewrite a selection of cells. Their output is ordinary cells,
//! so anything a transform writes can be edited by hand afterwards.

use std::ops::Range;

use crate::pattern::{Cell, Fx, Track};

/// A snare roll: one trigger per row, with the retrigger count, pitch and
/// velocity swept from start to end.
#[derive(Clone, Copy, Debug)]
pub struct Roll {
    pub slice: u16,
    pub hits: (u8, u8),
    pub pitch: (i8, i8),
    pub velocity: (u8, u8),
    /// Sweep curve. 1 is linear; above 1 the change bunches up at the end,
    /// which sounds like a roll that keeps accelerating.
    pub curve: f32,
}

impl Roll {
    pub fn apply(&self, track: &mut Track, rows: Range<usize>) {
        let rows = rows.start.min(track.cells.len())..rows.end.min(track.cells.len());
        let n = rows.len();
        for (i, row) in rows.enumerate() {
            let t = if n > 1 {
                i as f32 / (n - 1) as f32
            } else {
                0.0
            };
            let t = t.powf(self.curve.max(0.01));
            let lerp = |(a, b): (f32, f32)| (a + (b - a) * t).round();
            let hits = lerp((self.hits.0 as f32, self.hits.1 as f32)) as u8;
            let pitch = lerp((self.pitch.0 as f32, self.pitch.1 as f32)) as i8;
            let vel = lerp((self.velocity.0 as f32, self.velocity.1 as f32)) as u8;
            let mut cell = Cell::hit(self.slice, vel).with_fx(Fx::Retrig(hits.max(1)));
            if pitch != 0 {
                cell = cell.with_fx(Fx::Pitch(pitch));
            }
            track.cells[row] = cell;
        }
    }
}
