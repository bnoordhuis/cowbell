use std::sync::Arc;

use cowbell::engine::{Command, Engine};
use cowbell::rack::{MASTER_LIMIT, Rack};
use cowbell::sample::Sample;
use cowbell::song::{Cell, Clip, Fx, Instrument, Song, Track, Trigger};
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

/// One track playing `sample`. The master's limiter is off: these tests
/// check voices sample for sample, some of them louder than its knee.
fn one_track_song(sample: Sample, bpm: f64, rpb: u32, beats: u32) -> Song {
    let track = Track::new("t", 0).with_clip(Clip::beats(0, rpb, beats));
    let mut song = Song::new(
        bpm,
        vec![Instrument::Sampler(Arc::new(sample))],
        vec![track],
    );
    let master = master_id(&song.rack);
    song.rack.set_param(master, MASTER_LIMIT, 0.0);
    song
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
        song.tracks[0].clips[0].cells[row] = Cell::hit(0, 127);
    }
    assert_eq!(onsets(&render(song, 24_000)), vec![0, 6000, 12_000, 18_000]);
}

#[test]
fn fractional_row_length_does_not_drift() {
    // 4 rows/beat at 173 bpm: 4161.85... frames per row, and 86.7...
    // frames per tick.
    let mut song = one_track_song(click(10), 173.0, 4, 4);
    song.tracks[0].clips[0].cells.fill(Cell::hit(0, 127));
    let fpr = RATE as f64 * 60.0 / (173.0 * 4.0);
    // Stop just short of the loop back to row 0.
    let got = onsets(&render(song, (16.0 * fpr) as usize));
    assert_eq!(got.len(), 16);
    for (i, &f) in got.iter().enumerate() {
        assert!((f as f64 - i as f64 * fpr).abs() <= 1.0, "row {i} at {f}");
    }
}

#[test]
fn retrigger_splits_row_evenly() {
    let mut song = one_track_song(click(10), 120.0, 4, 1);
    song.tracks[0].clips[0].cells[0] = Cell::hit(0, 127).with_fx(Fx::Retrig(4));
    assert_eq!(onsets(&render(song, 6000)), vec![0, 1500, 3000, 4500]);
}

#[test]
fn cut_stops_note() {
    let sample = Sample::new("dc", RATE, vec![[0.5, 0.5]; 48_000]);
    let mut song = one_track_song(sample, 120.0, 4, 1);
    song.tracks[0].clips[0].cells[0] = Cell::hit(0, 127).with_fx(Fx::Cut(8));
    let out = render(song, 6000);
    let sounding = onsets(&out);
    assert_eq!(sounding.len(), 3000, "half a row");
}

#[test]
fn reverse_plays_backwards() {
    let frames = (0..100).map(|i| [i as f32 / 100.0; 2]).collect();
    let mut song = one_track_song(Sample::new("ramp", RATE, frames), 120.0, 4, 1);
    song.tracks[0].clips[0].cells[0] = Cell::hit(0, 127).with_fx(Fx::Reverse);
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
    let mut cells = vec![Cell::default(); 8];
    Roll {
        slice: 3,
        hits: (1, 8),
        pitch: (0, 12),
        velocity: (60, 120),
        curve: 1.0,
    }
    .apply(&mut cells, 0..8);
    assert_eq!(cells[0].fx().next(), Some(Fx::Retrig(1)));
    assert_eq!(
        cells[7].fx().collect::<Vec<_>>(),
        vec![Fx::Retrig(8), Fx::Pitch(12)]
    );
    assert_eq!(cells[7].first().unwrap().velocity, 120);
}

#[test]
fn swapped_song_is_handed_back() {
    let song = demo::song(demo::kit());
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
    let out = render(demo::song(demo::kit()), RATE as usize * 4);
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(out.iter().all(|s| s.is_finite()));
    assert!(peak > 0.1 && peak < 2.0, "peak {peak}");
}

fn dc_song() -> Song {
    let sample = Sample::new("dc", RATE, vec![[0.5, 0.5]; 48_000]);
    let mut song = one_track_song(sample, 120.0, 4, 8);
    song.tracks[0].clips[0].cells[0] = Cell::hit(0, 127);
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
    edited.tracks[0].clips[0].cells[3] = Cell::hit(0, 64);
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
        hold: false,
    };
    assert!(controller.send(cmd).is_ok());
    engine.process(&mut buf);
    assert!(buf.iter().all(|&s| s != 0.0));
    assert!(!controller.is_playing());
}

#[test]
fn a_previewed_file_plays_to_its_end_and_can_be_stopped() {
    let song = Song::new(120.0, Vec::new(), Vec::new());
    let (mut engine, mut controller) = Engine::new(RATE, song);
    // Not in the song, and played while the song is stopped.
    let file = Sample::new("file", RATE, vec![[0.5, 0.5]; 150]);
    assert!(
        controller
            .send(Command::Preview(Some(Arc::new(file))))
            .is_ok()
    );
    let mut buf = vec![0.0; 2 * 200];
    engine.process(&mut buf);
    let sounding = buf
        .as_chunks::<2>()
        .0
        .iter()
        .filter(|f| f[0] != 0.0)
        .count();
    assert_eq!(sounding, 150);
    assert!(!controller.is_playing(), "the song stays stopped");
    let long = Sample::new("long", RATE, vec![[0.5, 0.5]; 10_000]);
    assert!(
        controller
            .send(Command::Preview(Some(Arc::new(long))))
            .is_ok()
    );
    engine.process(&mut buf);
    assert!(buf.iter().all(|&s| s != 0.0));
    assert!(controller.send(Command::Preview(None)).is_ok());
    engine.process(&mut buf);
    assert!(buf.iter().all(|&s| s == 0.0));
}

#[test]
fn play_from_starts_at_row() {
    let mut song = one_track_song(click(10), 120.0, 4, 1);
    song.tracks[0].clips[0].cells[2] = Cell::hit(0, 127);
    let (mut engine, mut controller) = Engine::new(RATE, song);
    let pos = 2 * 48;
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
    song.tracks[0].clips[0].cells[0] = Cell::hit(0, 127).with_fx(Fx::Cut(8));
    set_declick(&mut song, declick);
    song
}

#[test]
fn declick_is_on_by_default() {
    let song = one_track_song(click(10), 120.0, 4, 1);
    let t = song.rack.track_device(0).unwrap();
    assert_eq!(
        t.params,
        vec![1.0, 1.0, 0.0],
        "declick on, full volume, in the middle"
    );
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
        song.tracks[0].clips[0].cells[1] = Cell::default().with_fx(Fx::Cut(0));
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
    song.tracks[0].clips[0].cells[0] = Cell::hit(0, 127);
    song.tracks[0].clips[0].cells[1] = Cell::hit(1, 127);
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
    song.tracks[0].clips[0].cells[0] = Cell::hit(0, 127).with_fx(Fx::Retrig(2));
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
    let (bpm, rpb, rows) = (173.0, 4, 8);
    let fpt = cowbell::engine::frames_per_tick(RATE as f64, bpm);
    let starts: Vec<usize> = cowbell::bounce::tick_starts(rows * 48, fpt)
        .into_iter()
        .step_by(48)
        .collect();
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
        song.tracks[0].clips[0].cells[r] = Cell::hit(r as u16, 127);
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

#[test]
fn clips_play_where_they_start_on_their_own_grid() {
    // 120 bpm: 125 frames per tick, 24_000 per beat. A 4-row clip in beat 0
    // and a 2-rows-per-beat clip from beat 2 on a second track.
    let mut song = one_track_song(click(10), 120.0, 4, 1);
    song.tracks[0].clips[0].cells[1] = Cell::hit(0, 127);
    let mut b = Clip::beats(2 * 192, 2, 1);
    b.cells[0] = Cell::hit(0, 127);
    b.cells[1] = Cell::hit(0, 127);
    song.tracks.push(Track::new("b", 0).with_clip(b));
    song.rack = Rack::simple(2);
    assert_eq!(
        onsets(&render(song, 4 * 24_000)),
        vec![6000, 48_000, 60_000]
    );
}

#[test]
fn loop_range_repeats_its_stretch() {
    // Hits on beats 0 and 1 of a two-beat clip; loop beat 1 only.
    let mut song = one_track_song(click(10), 120.0, 1, 2);
    song.tracks[0].clips[0].cells.fill(Cell::hit(0, 127));
    song.loop_range = Some(192..384);
    let (mut engine, mut c) = Engine::new(RATE, song);
    assert!(c.send(Command::PlayFrom(192)).is_ok());
    let mut out = vec![0.0; 2 * 3 * 24_000];
    engine.process(&mut out);
    assert_eq!(onsets(&out), vec![0, 24_000, 48_000]);
    // Export ignores it and stops at the end.
    let mut song = one_track_song(click(10), 120.0, 1, 2);
    song.tracks[0].clips[0].cells.fill(Cell::hit(0, 127));
    song.loop_range = Some(192..384);
    let (mut engine, _c) = Engine::new(RATE, song);
    engine.set_looping(false);
    engine.play();
    let mut out = vec![0.0; 2 * 8 * 24_000];
    engine.process(&mut out);
    assert_eq!(onsets(&out), vec![0, 24_000]);
}

#[test]
fn mute_and_solo_silence_tracks() {
    let mut song = dc_song();
    song.tracks.push(Track::new("b", 0));
    song.rack = Rack::simple(2);
    song.tracks[0].mute = true;
    assert!(render(song.clone(), 1000).iter().all(|&s| s == 0.0));
    song.tracks[0].mute = false;
    song.tracks[1].solo = true;
    assert!(render(song.clone(), 1000).iter().all(|&s| s == 0.0));
    song.tracks[0].solo = true;
    assert!(render(song, 1000).iter().all(|&s| s != 0.0));
}

#[test]
fn stop_keeps_the_playhead_and_locate_moves_it() {
    let mut song = one_track_song(click(10), 120.0, 4, 4);
    song.tracks[0].clips[0].cells.fill(Cell::hit(0, 127));
    let (mut engine, mut c) = Engine::new(RATE, song);
    engine.play();
    let mut buf = vec![0.0; 2 * 13_000];
    engine.process(&mut buf);
    assert!(c.send(Command::Stop).is_ok());
    engine.process(&mut buf);
    assert!(!c.is_playing());
    assert_eq!(c.position(), 13_000 / 125 - 1, "the last tick played");
    assert!(c.send(Command::Locate(10 * 48)).is_ok());
    engine.process(&mut buf[..2]);
    assert_eq!(c.position(), 10 * 48);
    assert!(c.send(Command::Play).is_ok());
    let mut out = vec![0.0; 2 * 100];
    engine.process(&mut out);
    assert_eq!(onsets(&out), vec![0], "row 10 plays right away");
    assert!(c.send(Command::Locate(1 << 40)).is_ok());
    engine.process(&mut buf[..2]);
    assert_eq!(c.position(), 0, "past the end goes back to the start");
}

/// A steady 0.5 split into two slices, so two hits overlapping add up to
/// exactly 1.0 (with declick off).
fn two_slice_song(poly: bool) -> Song {
    let sample = Sample::new("dc", RATE, vec![[0.5, 0.5]; 48_000]).with_slices(vec![0, 24_000]);
    let mut song = one_track_song(sample, 120.0, 4, 8);
    song.tracks[0].poly = poly;
    set_declick(&mut song, false);
    song
}

#[test]
fn hits_in_one_row_play_together() {
    for poly in [false, true] {
        let mut song = two_slice_song(poly);
        song.tracks[0].clips[0].cells[0] = Cell::hit(0, 127).with_note(Trigger::new(1, 127));
        let out = left(&render(song, 1000));
        assert_eq!(out[10], 1.0, "poly {poly}");
    }
}

#[test]
fn mono_rows_take_over_and_poly_rows_ring_on() {
    // 120 bpm, 4 rows a beat: rows are 6000 frames.
    // Mono: only the second row's hit is left. Poly: it joins both of the
    // first row's.
    for (poly, expected) in [(false, 0.5), (true, 1.5)] {
        let mut song = two_slice_song(poly);
        let cells = &mut song.tracks[0].clips[0].cells;
        cells[0] = Cell::hit(0, 127).with_note(Trigger::new(1, 127));
        cells[1] = Cell::hit(1, 127);
        let out = left(&render(song, 8000));
        assert_eq!(out[7000], expected, "poly {poly}");
    }
}

#[test]
fn a_note_with_a_length_stops_on_time() {
    for poly in [false, true] {
        let mut song = two_slice_song(poly);
        let long = Trigger {
            slice: 0,
            velocity: 127,
            rows: 1,
        };
        song.tracks[0].clips[0].cells[0] = Cell::default().with_note(long);
        let out = left(&render(song, 8000));
        assert_eq!(out[5999], 0.5, "poly {poly}");
        assert_eq!(out[6000], 0.0, "poly {poly}: cut after its row");
    }
}

#[test]
fn a_poly_track_steals_its_oldest_voice() {
    // One slice eight rows long, hit on every row: more hits ringing than
    // there are voices.
    let mut song = one_track_song(
        Sample::new("dc", RATE, vec![[0.5, 0.5]; 48_000]),
        120.0,
        4,
        8,
    );
    song.tracks[0].poly = true;
    set_declick(&mut song, false);
    for row in 0..8 {
        song.tracks[0].clips[0].cells[row] = Cell::hit(0, 127);
    }
    let out = left(&render(song, 8 * 6000));
    let most = out.iter().fold(0.0f32, |m, &s| m.max(s));
    assert_eq!(most, 3.0, "six voices at most, 0.5 each");
    assert_eq!(
        out[7 * 6000 + 10],
        3.0,
        "the last hit took the oldest voice"
    );
}

#[test]
fn a_keyboard_plays_the_track_it_is_told_to() {
    use cowbell::engine::Key;
    use cowbell::synthvoice::{SynthKind, SynthPatch};
    let synth = Instrument::Synth(Arc::new(SynthPatch::new(SynthKind::Fm, "fm")));
    let tracks = vec![Track::new("a", 0), Track::new("b", 0)];
    let song = Song::new(120.0, vec![synth], tracks);
    let (mut engine, mut c) = Engine::new(RATE, song);
    let keys = c.keys();
    let press = |key| keys.lock().unwrap().push(key).unwrap();
    let level = |engine: &mut Engine, frames: usize| {
        let mut out = vec![0.0; 2 * frames];
        engine.process(&mut out);
        out.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    };
    press(Key::On {
        note: 60,
        velocity: 100,
    });
    assert_eq!(level(&mut engine, 4800), 0.0, "no track to play yet");
    assert!(c.send(Command::Keyboard(Some(1))).is_ok());
    level(&mut engine, 256);
    press(Key::On {
        note: 60,
        velocity: 100,
    });
    assert!(level(&mut engine, 4800) > 0.01, "playing, stopped or not");
    press(Key::Pedal(true));
    press(Key::Off { note: 60 });
    level(&mut engine, 48_000 * 2);
    assert!(level(&mut engine, 4800) > 0.001, "held by the pedal");
    press(Key::Pedal(false));
    level(&mut engine, 48_000 * 2);
    assert_eq!(level(&mut engine, 4800), 0.0, "let go");
    // Another track: held keys on the first are let go.
    press(Key::On {
        note: 64,
        velocity: 100,
    });
    level(&mut engine, 256);
    assert!(c.send(Command::Keyboard(Some(0))).is_ok());
    level(&mut engine, 48_000 * 2);
    assert_eq!(level(&mut engine, 4800), 0.0);
}
