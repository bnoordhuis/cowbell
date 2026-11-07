//! Bounce: render a stretch of the song, through the rack, to a new sample,
//! and slice it on row boundaries so it can be re-sequenced.
//!
//! Slice starts are moved back to the start of the row a hit falls in. A
//! slice then always starts exactly where the engine starts a row, so
//! triggering each slice on its row plays the bounce back sample for sample.

use std::sync::Arc;

use crate::engine::{Command, Engine, MAX_TRACKS, frames_per_tick};
use crate::rack::{Kind, MASTER_LIMIT};
use crate::sample::Sample;
use crate::song::{Cell, Clip, Instrument, Song, TICKS_PER_BEAT, Tick, Track};

/// Bounces are rendered at this rate; voices resample on playback.
pub const RATE: u32 = 48_000;

/// The frame each of `n` ticks starts on, relative to the first, plus one
/// entry for the end. Mirrors the engine's clock exactly, so the result
/// matches where it really fires rows.
pub fn tick_starts(n: usize, fpt: f64) -> Vec<usize> {
    let mut starts = Vec::with_capacity(n + 1);
    let (mut clock, mut frame) = (0.0f64, 0usize);
    for _ in 0..=n {
        while clock > 0.0 {
            clock -= 1.0;
            frame += 1;
        }
        starts.push(frame);
        clock += fpt;
    }
    starts
}

pub struct Bounce {
    pub sample: Sample,
    /// For each slice, the row (relative to the first bounced row) it starts
    /// on.
    pub slice_rows: Vec<usize>,
}

/// Render `rows` rows of a `rows_per_beat` grid from `start`, and slice the
/// result on that grid.
pub fn bounce(song: &Song, start: Tick, rows: usize, rows_per_beat: u32, name: &str) -> Bounce {
    let tpr = (TICKS_PER_BEAT / rows_per_beat.max(1) as Tick) as usize;
    let ticks = tick_starts(rows * tpr, frames_per_tick(RATE as f64, song.bpm));
    let starts: Vec<usize> = ticks.iter().step_by(tpr).copied().collect();
    let len = *starts.last().unwrap();

    // A bounce becomes a track that's mixed and limited again, so take it
    // from before the master's limiter.
    let mut song = song.clone();
    if let Some(master) = song.rack.devices.iter().find(|d| d.kind == Kind::Master) {
        let id = master.id;
        song.rack.set_param(id, MASTER_LIMIT, 0.0);
    }
    let (mut engine, mut controller) = Engine::new(RATE, song);
    // Straight through, whatever the loop range says.
    engine.set_looping(false);
    let _ = controller.send(Command::PlayFrom(start));
    let mut buf = vec![0.0f32; 2 * len];
    for block in buf.chunks_mut(2 * 1024) {
        engine.process(block);
    }
    let frames: Vec<[f32; 2]> = buf.as_chunks::<2>().0.to_vec();

    let mut sample = Sample::new(name, RATE, frames);
    // Hits closer than a row apart can't get a row each anyway.
    let row_ms = ((starts[1] - starts[0]) as f64 / RATE as f64 * 1000.0) as f32;
    sample.detect_slices(0.6, row_ms.min(60.0));
    let mut slice_rows: Vec<usize> = sample
        .slices
        .iter()
        .map(|&s| starts.partition_point(|&r| r <= s).saturating_sub(1))
        .collect();
    slice_rows.push(0);
    slice_rows.sort_unstable();
    slice_rows.dedup();
    slice_rows.retain(|&r| r < rows);
    sample.slices = slice_rows.iter().map(|&r| starts[r]).collect();
    Bounce { sample, slice_rows }
}

/// Replace a stretch of the song with its bounce: the audio goes to a new
/// sample, played by a new track with a clip of its slices, and the cells it
/// replaces are cleared. Returns the new track.
pub fn bounce_to_track(
    song: &mut Song,
    start: Tick,
    rows: usize,
    rows_per_beat: u32,
) -> Result<usize, String> {
    let t = song.tracks.len();
    if t >= MAX_TRACKS {
        return Err(format!("can't bounce: already {t} tracks"));
    }
    let n = song.instruments.len();
    let b = bounce(song, start, rows, rows_per_beat, &format!("bounce{n}"));

    let mut clip = Clip::new(start, rows_per_beat, rows);
    let end = clip.end();
    for (slice, &r) in b.slice_rows.iter().enumerate() {
        clip.cells[r] = Cell::hit(slice as u16, 0x7f);
    }
    for track in &mut song.tracks {
        for c in &mut track.clips {
            for row in 0..c.cells.len() {
                if (start..end).contains(&c.row_tick(row)) {
                    c.cells[row] = Cell::default();
                }
            }
        }
    }
    song.instruments
        .push(Instrument::Sampler(Arc::new(b.sample)));
    song.tracks
        .push(Track::new(&format!("b{n}"), n).with_clip(clip));
    song.rack.add_track(t);
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_starts_match_whole_frames() {
        assert_eq!(tick_starts(3, 100.0), vec![0, 100, 200, 300]);
        // 2.5 frames per tick: ticks start on the first frame at or after
        // each boundary.
        assert_eq!(tick_starts(4, 2.5), vec![0, 3, 5, 8, 10]);
    }
}
