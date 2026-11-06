use std::sync::Arc;

use cowbell::engine::{Command, Engine};
use cowbell::pattern::{Cell, Fx, Instrument, Pattern, Song};
use cowbell::rack::Rack;
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
        rack: Rack::simple(1),
        tuning: Default::default(),
        automation: Vec::new(),
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
    out.as_chunks::<2>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, f)| f[0] != 0.0)
        .map(|(i, _)| i)
        .collect()
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
    Roll {
        slice: 3,
        hits: (1, 8),
        pitch: (0, 12),
        velocity: (60, 120),
        curve: 1.0,
    }
    .apply(&mut p.tracks[0], 0..8);
    let cells = &p.tracks[0].cells;
    assert_eq!(cells[0].fx().next(), Some(Fx::Retrig(1)));
    assert_eq!(
        cells[7].fx().collect::<Vec<_>>(),
        vec![Fx::Retrig(8), Fx::Pitch(12)]
    );
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
        Instrument::Synth(_) => panic!("not a sample"),
    });
    assert!(
        controller
            .send(Command::SwapSong(Box::new(song.clone())))
            .is_ok()
    );
    engine.process(&mut buf);
    assert!(buf.iter().all(|s| s.is_finite()));
    controller.collect_garbage();
    let after = Arc::strong_count(match &song.instruments[0] {
        Instrument::Sampler(s) => s,
        Instrument::Synth(_) => panic!("not a sample"),
    });
    assert_eq!(after, before, "old song dropped by the controller");
}

#[test]
fn demo_song_renders_without_clipping_or_nans() {
    let out = render(
        demo::song(synth::demo_break(RATE, 172.0)),
        RATE as usize * 4,
    );
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(peak > 0.1 && peak < 2.0, "peak {peak}");
}

fn dc_song() -> Song {
    let sample = Sample::new("dc", RATE, vec![[0.5, 0.5]; 48_000]);
    let mut song = one_track_song(sample, 120.0, 4, 8);
    song.patterns[0].tracks[0].cells[0] = Cell::hit(0, 127);
    song
}

#[test]
fn edit_keeps_ringing_notes() {
    let song = dc_song();
    let (mut engine, mut controller) = Engine::new(RATE, song.clone());
    engine.play();
    let mut buf = vec![0.0; 2 * 1000];
    engine.process(&mut buf);
    // Same sample, edited pattern: the note keeps sounding.
    let mut edited = song.clone();
    edited.patterns[0].tracks[0].cells[3] = Cell::hit(0, 64);
    assert!(controller.send(Command::SwapSong(Box::new(edited))).is_ok());
    engine.process(&mut buf);
    assert!(buf.iter().all(|&s| s != 0.0));
    // New sample: the note stops, because its frames may be gone.
    assert!(
        controller
            .send(Command::SwapSong(Box::new(dc_song())))
            .is_ok()
    );
    engine.process(&mut buf);
    assert!(buf.iter().all(|&s| s == 0.0));
}

#[test]
fn audition_plays_while_stopped() {
    let (mut engine, mut controller) = Engine::new(RATE, dc_song());
    let mut buf = vec![0.0; 2 * 100];
    engine.process(&mut buf);
    assert!(buf.iter().all(|&s| s == 0.0));
    let cmd = Command::Audition {
        instrument: 0,
        slice: 0,
        velocity: 127,
    };
    assert!(controller.send(cmd).is_ok());
    engine.process(&mut buf);
    assert!(buf.iter().all(|&s| s != 0.0));
    assert!(!controller.is_playing());
}

#[test]
fn play_from_starts_at_row() {
    let mut song = one_track_song(click(10), 120.0, 4, 1);
    song.patterns[0].tracks[0].cells[2] = Cell::hit(0, 127);
    let (mut engine, mut controller) = Engine::new(RATE, song);
    let pos = cowbell::engine::Position { order: 0, row: 2 };
    assert!(controller.send(Command::PlayFrom(pos)).is_ok());
    let mut buf = vec![0.0; 2 * 100];
    engine.process(&mut buf);
    assert_eq!(onsets(&buf), vec![0]);
    assert!(controller.is_playing());
    assert_eq!(controller.position(), pos);
}

fn master_id(rack: &Rack) -> u32 {
    rack.devices
        .iter()
        .find(|d| d.kind == cowbell::rack::Kind::Master)
        .unwrap()
        .id
}

#[test]
fn set_param_reaches_the_graph() {
    let song = dc_song();
    let master = master_id(&song.rack);
    let (mut engine, mut controller) = Engine::new(RATE, song);
    engine.play();
    let mut buf = vec![0.0; 2 * 100];
    engine.process(&mut buf);
    assert!(buf.iter().all(|&s| s != 0.0));
    let cmd = Command::SetParam {
        device: master,
        param: 0,
        value: 0.0,
    };
    assert!(controller.send(cmd).is_ok());
    engine.process(&mut buf);
    assert!(buf.iter().all(|&s| s == 0.0));
}

#[test]
fn swapped_graph_keeps_notes_ringing() {
    let song = dc_song();
    let (mut engine, mut controller) = Engine::new(RATE, song.clone());
    engine.play();
    let mut buf = vec![0.0; 2 * 100];
    engine.process(&mut buf);
    let mut rack = song.rack.clone();
    rack.add(cowbell::rack::Kind::Lfo);
    let graph = Box::new(cowbell::graph::Graph::build(&rack, RATE));
    assert!(controller.send(Command::SwapGraph(graph)).is_ok());
    engine.process(&mut buf);
    assert!(
        buf.iter().all(|&s| s == 0.5),
        "note still sounding, at unity gain"
    );
    controller.collect_garbage();
}

// Declick. At 48 kHz a fade is 72 frames; frame k of it has gain
// (72 - k) / 73, so it ends just above silence and never steps by more than
// 1/73 of the level.

const FADE: usize = 72;

fn fade_gain(k: usize) -> f32 {
    (FADE - k) as f32 / (FADE + 1) as f32
}

fn set_declick(song: &mut Song, on: bool) {
    let t = song.rack.track_device(0).unwrap().id;
    song.rack.set_param(t, 0, on as u8 as f32);
}

fn left(out: &[f32]) -> Vec<f32> {
    out.as_chunks::<2>().0.iter().map(|f| f[0]).collect()
}

fn biggest_step(out: &[f32]) -> f32 {
    left(out)
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max)
}

fn cut_song(declick: bool) -> Song {
    let sample = Sample::new("dc", RATE, vec![[0.5, 0.5]; 48_000]);
    let mut song = one_track_song(sample, 120.0, 4, 1);
    song.patterns[0].tracks[0].cells[0] = Cell::hit(0, 127).with_fx(Fx::Cut(8));
    set_declick(&mut song, declick);
    song
}

#[test]
fn declick_is_on_by_default() {
    let song = one_track_song(click(10), 120.0, 4, 1);
    let t = song.rack.track_device(0).unwrap();
    assert_eq!(t.params, vec![1.0]);
}

#[test]
fn cut_fades_out_and_ends_on_time() {
    let out = left(&render(cut_song(true), 6000));
    // Full level up to the fade, which ends exactly where the cut is due.
    assert!(out[..3000 - FADE].iter().all(|&s| s == 0.5));
    for k in 0..FADE {
        assert_eq!(out[3000 - FADE + k], 0.5 * fade_gain(k), "fade frame {k}");
    }
    assert!(out[3000..].iter().all(|&s| s == 0.0));
    assert!(biggest_step(&render(cut_song(true), 6000)) <= 0.5 / 73.0 + 1e-6);
}

#[test]
fn cut_without_declick_is_a_hard_stop() {
    let out = left(&render(cut_song(false), 6000));
    assert!(out[..3000].iter().all(|&s| s == 0.5));
    assert!(out[3000..].iter().all(|&s| s == 0.0));
}

#[test]
fn cut_of_ringing_note_fades() {
    // A cut with no trigger, on the next row: the note started long ago.
    for declick in [false, true] {
        let mut song = dc_song();
        song.patterns[0].tracks[0].cells[1] = Cell::default().with_fx(Fx::Cut(0));
        set_declick(&mut song, declick);
        let out = left(&render(song, 12_000));
        assert!(out[..6000].iter().all(|&s| s == 0.5));
        if declick {
            // Cut(0) leaves no time to fade before the cut, so it fades from
            // the cut on.
            for k in 0..FADE {
                assert_eq!(out[6000 + k], 0.5 * fade_gain(k));
            }
            assert!(out[6000 + FADE..].iter().all(|&s| s == 0.0));
        } else {
            assert!(out[6000..].iter().all(|&s| s == 0.0));
        }
    }
}

/// Two slices: +0.5 then -0.5, each a second long.
fn two_level_song(declick: bool) -> Song {
    let mut frames = vec![[0.5; 2]; 48_000];
    frames.extend(vec![[-0.5; 2]; 48_000]);
    let mut sample = Sample::new("levels", RATE, frames);
    sample.slices = vec![0, 48_000];
    let mut song = one_track_song(sample, 120.0, 4, 1);
    song.patterns[0].tracks[0].cells[0] = Cell::hit(0, 127);
    song.patterns[0].tracks[0].cells[1] = Cell::hit(1, 127);
    set_declick(&mut song, declick);
    song
}

#[test]
fn steal_fades_old_note_under_sharp_attack() {
    let out = left(&render(two_level_song(true), 12_000));
    assert!(out[..6000].iter().all(|&s| s == 0.5));
    // The new note is at full level from its first frame; the old one fades
    // out on top of it.
    for k in 0..FADE {
        assert_eq!(out[6000 + k], -0.5 + 0.5 * fade_gain(k), "frame {k}");
    }
    assert!(out[6000 + FADE..].iter().all(|&s| s == -0.5));
}

#[test]
fn steal_without_declick_is_a_hard_edge() {
    let out = left(&render(two_level_song(false), 12_000));
    assert!(out[..6000].iter().all(|&s| s == 0.5));
    assert!(out[6000..].iter().all(|&s| s == -0.5));
}

fn ramp_retrig_song(declick: bool) -> Song {
    let frames = (0..48_000).map(|i| [i as f32 / 48_000.0; 2]).collect();
    let mut song = one_track_song(Sample::new("ramp", RATE, frames), 120.0, 4, 1);
    song.patterns[0].tracks[0].cells[0] = Cell::hit(0, 127).with_fx(Fx::Retrig(2));
    set_declick(&mut song, declick);
    song
}

#[test]
fn retrigger_fades_old_sound() {
    let ramp = |i: usize| i as f32 / 48_000.0;
    let out = left(&render(ramp_retrig_song(true), 6000));
    assert!((0..3000).all(|i| out[i] == ramp(i)));
    for k in 0..FADE {
        let want = ramp(k) + ramp(3000 + k) * fade_gain(k);
        assert_eq!(out[3000 + k], want, "frame {k}");
    }
    assert!((3000 + FADE..6000).all(|i| out[i] == ramp(i - 3000)));
    // The restart is still a hit: one onset per retrigger in the gate.
}

#[test]
fn retrigger_without_declick_restarts_dead() {
    let ramp = |i: usize| i as f32 / 48_000.0;
    let out = left(&render(ramp_retrig_song(false), 6000));
    assert!((0..3000).all(|i| out[i] == ramp(i)));
    assert!((3000..6000).all(|i| out[i] == ramp(i - 3000)));
}

#[test]
fn back_to_back_slices_join_exactly_with_declick() {
    // A fractional row length, with slices cut where the engine starts
    // rows: each slice has played its last frame when the next one steals
    // the voice, so there is nothing to fade and the joins are exact.
    let (bpm, rpb, rows) = (173.0, 7, 14);
    let fpr = cowbell::engine::frames_per_row(RATE as f64, bpm, rpb);
    let starts = cowbell::bounce::row_starts(rows, fpr);
    let len = *starts.last().unwrap();
    let frames: Vec<[f32; 2]> = (0..len)
        .map(|i| {
            let s = (i as f32 * 0.05).sin() * 0.8;
            [s, -s]
        })
        .collect();
    let mut sample = Sample::new("chops", RATE, frames.clone());
    sample.slices = starts[..rows].to_vec();
    let mut song = one_track_song(sample, bpm, rpb, 2);
    for r in 0..rows {
        song.patterns[0].tracks[0].cells[r] = Cell::hit(r as u16, 127);
    }
    set_declick(&mut song, true);
    let out = render(song, len);
    assert!(out.as_chunks::<2>().0.iter().eq(frames.iter()));
}

#[test]
fn declick_param_can_be_turned_off_while_playing() {
    let song = two_level_song(true);
    let t = song.rack.track_device(0).unwrap().id;
    let (mut engine, mut controller) = Engine::new(RATE, song);
    engine.play();
    let cmd = Command::SetParam {
        device: t,
        param: 0,
        value: 0.0,
    };
    assert!(controller.send(cmd).is_ok());
    let mut out = vec![0.0; 2 * 12_000];
    engine.process(&mut out);
    let out = left(&out);
    assert_eq!(out[5999], 0.5);
    assert_eq!(out[6000], -0.5, "hard edge");
}
