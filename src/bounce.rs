//! Bounce: render rows of a pattern, through the rack, to a new sample, and
//! slice it on row boundaries so it can be re-sequenced.
//!
//! Slice starts are moved back to the start of the row a hit falls in. A
//! slice then always starts exactly where the engine starts a row, so
//! triggering each slice on its row plays the bounce back sample for sample.

use std::ops::Range;

use crate::engine::{Command, Engine, Position, frames_per_row};
use crate::pattern::Song;
use crate::sample::Sample;

/// Bounces are rendered at this rate; voices resample on playback.
pub const RATE: u32 = 48_000;

/// The frame each row starts on, relative to the first, for `n` rows (plus
/// one entry for the end). Mirrors the engine's arithmetic exactly, so the
/// result matches where it really fires rows.
pub fn row_starts(n: usize, fpr: f64) -> Vec<usize> {
    let mut starts = Vec::with_capacity(n + 1);
    let (mut clock, mut frame) = (0.0f64, 0usize);
    for _ in 0..=n {
        while clock > 0.0 {
            clock -= 1.0;
            frame += 1;
        }
        starts.push(frame);
        clock += fpr;
    }
    starts
}

pub struct Bounce {
    pub sample: Sample,
    /// For each slice, the row (relative to the first bounced row) it starts
    /// on.
    pub slice_rows: Vec<usize>,
}

/// Render `rows` of the pattern at `order` and slice the result.
pub fn bounce(song: &Song, order: usize, rows: Range<usize>, name: &str) -> Bounce {
    let rpb = song
        .order
        .get(order)
        .and_then(|&p| song.patterns.get(p))
        .map_or(4, |p| p.rows_per_beat);
    let fpr = frames_per_row(RATE as f64, song.bpm, rpb);
    let starts = row_starts(rows.len(), fpr);
    let len = *starts.last().unwrap();

    let (mut engine, mut controller) = Engine::new(RATE, song.clone());
    let _ = controller.send(Command::PlayFrom(Position {
        order,
        row: rows.start,
    }));
    let mut buf = vec![0.0f32; 2 * len];
    for block in buf.chunks_mut(2 * 1024) {
        engine.process(block);
    }
    let frames: Vec<[f32; 2]> = buf.as_chunks::<2>().0.to_vec();

    let mut sample = Sample::new(name, RATE, frames);
    // Hits closer than a row apart can't get a row each anyway.
    let row_ms = (fpr / RATE as f64 * 1000.0) as f32;
    sample.detect_slices(0.6, row_ms.min(60.0));
    let mut slice_rows: Vec<usize> = sample
        .slices
        .iter()
        .map(|&s| starts.partition_point(|&r| r <= s).saturating_sub(1))
        .collect();
    slice_rows.push(0);
    slice_rows.sort_unstable();
    slice_rows.dedup();
    slice_rows.retain(|&r| r < rows.len());
    sample.slices = slice_rows.iter().map(|&r| starts[r]).collect();
    Bounce { sample, slice_rows }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_starts_match_whole_frames() {
        assert_eq!(row_starts(3, 100.0), vec![0, 100, 200, 300]);
        // 2.5 frames per row: rows start on the first frame at or after
        // each boundary.
        assert_eq!(row_starts(4, 2.5), vec![0, 3, 5, 8, 10]);
    }
}
