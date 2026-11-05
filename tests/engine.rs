use std::sync::Arc;

use cowbell::engine::{Command, Engine};
use cowbell::pattern::{Cell, Fx, Instrument, Pattern, Song};
use cowbell::sample::Sample;
use cowbell::transform::Roll;
use cowbell::{demo, synth};

const RATE: u32 = 48_000;

/// A sample of `len` frames that is a single click at frame 0, so every
/// (re)trigger shows up as exactly one nonzero output frame.
fn click(len: usize) -> Sample {
    let mut frames = vec![[0.0; 2]; len];
    frames[0] = [1.0, 1.0];
    Sample::new("click", RATE, frames)
}

fn one_track_song(sample: Sample, bpm: f64, rpb: u32, beats: u32) -> Song {
    Song {
        bpm,
        instruments: vec![Instrument::Sampler(Arc::new(sample))],
        patterns: vec![Pattern::new(rpb, beats, &[("t", 0)])],
        order: vec![0],
    }
}

fn render(song: Song, frames: usize) -> Vec<f32> {
    let (mut engine, _c) = Engine::new(RATE, song);
    engine.play();
    let mut out = vec![0.0; 2 * frames];
    // Odd block size so row boundaries fall inside blocks.
    for block in out.chunks_mut(2 * 333) {
        engine.process(block);
    }
    out
}

fn onsets(out: &[f32]) -> Vec<usize> {
    out.chunks_exact(2).enumerate().filter(|(_, f)| f[0] != 0.0).map(|(i, _)| i).collect()
}

#[test]
fn rows_land_on_exact_frames() {
    // 120 bpm, 4 rows/beat: 6000 frames per row.
    let mut song = one_track_song(click(10), 120.0, 4, 1);
    for row in 0..4 {
        song.patterns[0].tracks[0].cells[row] = Cell::hit(0, 127);
    }
    assert_eq!(onsets(&render(song, 24_000)), vec![0, 6000, 12_000, 18_000]);
}

#[test]
fn fractional_row_length_does_not_drift() {
    // 7 rows/beat at 173 bpm: 2378.03... frames per row.
    let mut song = one_track_song(click(10), 173.0, 7, 4);
    song.patterns[0].tracks[0].cells.fill(Cell::hit(0, 127));
    let fpr = RATE as f64 * 60.0 / (173.0 * 7.0);
    let got = onsets(&render(song, 28 * 2378));
    assert_eq!(got.len(), 28);
    for (i, &f) in got.iter().enumerate() {
        assert!((f as f64 - i as f64 * fpr).abs() <= 1.0, "row {i} at {f}");
    }
}

#[test]
fn retrigger_splits_row_evenly() {
    let mut song = one_track_song(click(10), 120.0, 4, 1);
    song.patterns[0].tracks[0].cells[0] = Cell::hit(0, 127).with_fx(Fx::Retrig(4));
    assert_eq!(onsets(&render(song, 6000)), vec![0, 1500, 3000, 4500]);
}

#[test]
fn cut_stops_note() {
    let sample = Sample::new("dc", RATE, vec![[0.5, 0.5]; 48_000]);
    let mut song = one_track_song(sample, 120.0, 4, 1);
    song.patterns[0].tracks[0].cells[0] = Cell::hit(0, 127).with_fx(Fx::Cut(8));
    let out = render(song, 6000);
    let sounding = onsets(&out);
    assert_eq!(sounding.len(), 3000, "half a row");
}

#[test]
fn reverse_plays_backwards() {
    let frames = (0..100).map(|i| [i as f32 / 100.0; 2]).collect();
    let mut song = one_track_song(Sample::new("ramp", RATE, frames), 120.0, 4, 1);
    song.patterns[0].tracks[0].cells[0] = Cell::hit(0, 127).with_fx(Fx::Reverse);
    let out = render(song, 10);
    assert!((out[0] - 0.99).abs() < 1e-6);
    assert!(out[2] < out[0]);
}

#[test]
fn slicer_finds_demo_break_hits() {
    let expected = synth::demo_break(RATE, 172.0);
    let mut detected = expected.clone();
    detected.detect_slices(0.6, 60.0);
    assert_eq!(detected.slices.len(), expected.slices.len());
    for (d, e) in detected.slices.iter().zip(&expected.slices) {
        assert!(d.abs_diff(*e) <= 256, "detected {d}, expected {e}");
    }
}

#[test]
fn roll_sweeps_hits_and_pitch() {
    let mut p = Pattern::new(4, 2, &[("snr", 0)]);
    Roll { slice: 3, hits: (1, 8), pitch: (0, 12), velocity: (60, 120), curve: 1.0 }
        .apply(&mut p.tracks[0], 0..8);
    let cells = &p.tracks[0].cells;
    assert_eq!(cells[0].fx().next(), Some(Fx::Retrig(1)));
    assert_eq!(cells[7].fx().collect::<Vec<_>>(), vec![Fx::Retrig(8), Fx::Pitch(12)]);
    assert_eq!(cells[7].trigger.unwrap().velocity, 120);
}

#[test]
fn swapped_song_is_handed_back() {
    let song = demo::song(synth::demo_break(RATE, 172.0));
    let (mut engine, mut controller) = Engine::new(RATE, song.clone());
    engine.play();
    let mut buf = vec![0.0; 1024];
    engine.process(&mut buf);
    let before = Arc::strong_count(match &song.instruments[0] {
        Instrument::Sampler(s) => s,
    });
    assert!(controller.send(Command::SwapSong(Box::new(song.clone()))).is_ok());
    engine.process(&mut buf);
    assert!(buf.iter().all(|s| s.is_finite()));
    controller.collect_garbage();
    let after = Arc::strong_count(match &song.instruments[0] {
        Instrument::Sampler(s) => s,
    });
    assert_eq!(after, before, "old song dropped by the controller");
}

#[test]
fn demo_song_renders_without_clipping_or_nans() {
    let out = render(demo::song(synth::demo_break(RATE, 172.0)), RATE as usize * 4);
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(peak > 0.1 && peak < 2.0, "peak {peak}");
}
