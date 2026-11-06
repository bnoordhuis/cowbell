use std::sync::Arc;

use cowbell::editor::{Action, Editor, Field};
use cowbell::engine::Engine;
use cowbell::pattern::{Cell, Fx, Instrument, Pattern, Song};
use cowbell::rack::Rack;
use cowbell::synthvoice::{SynthKind, SynthPatch};
use cowbell::tuning::Tuning;

const RATE: u32 = 48_000;

/// One FM track with the modulation off, so it plays a plain sine.
fn sine_song(tuning: Tuning) -> Song {
    let mut patch = SynthPatch::new(SynthKind::Fm, "sine");
    patch.params[1] = 0.0; // index
    patch.params[6] = 1.0; // sustain
    Song {
        bpm: 120.0,
        instruments: vec![Instrument::Synth(Arc::new(patch))],
        patterns: vec![Pattern::new(4, 8, &[("t", 0)])],
        order: vec![0],
        rack: Rack::simple(1),
        tuning,
        automation: Vec::new(),
    }
}

fn render(song: Song, frames: usize) -> Vec<f32> {
    let (mut engine, _c) = Engine::new(RATE, song);
    engine.play();
    let mut out = vec![0.0; 2 * frames];
    engine.process(&mut out);
    out.chunks(2).map(|f| f[0]).collect()
}

fn pitch(out: &[f32]) -> f64 {
    let ups = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    ups as f64 / (out.len() as f64 / RATE as f64)
}

#[test]
fn notes_play_at_their_pitch() {
    let mut song = sine_song(Tuning::default());
    song.patterns[0].tracks[0].cells[0] = Cell::hit(69, 127);
    let out = render(song, RATE as usize);
    let f = pitch(&out[4800..]);
    assert!((f - 440.0).abs() < 3.0, "A-4 at {f} Hz");
}

#[test]
fn tuning_changes_pitch() {
    let scl = "fifths\n1\n3/2\n";
    let mut song = sine_song(Tuning::from_scala(scl).unwrap());
    // One degree per "octave" of 3/2: note 61 is a fifth above middle C.
    song.patterns[0].tracks[0].cells[0] = Cell::hit(61, 127);
    let f = pitch(&render(song, RATE as usize)[4800..]);
    let want = Tuning::default().freq(60) * 1.5;
    assert!((f - want).abs() < 3.0, "{f} Hz, want {want}");
}

#[test]
fn cut_releases_the_note() {
    let mut song = sine_song(Tuning::default());
    song.patterns[0].tracks[0].cells[0] = Cell::hit(60, 127);
    song.patterns[0].tracks[0].cells[1] = Cell::default().with_fx(Fx::Cut(0));
    // Two rows is half a second at 120 bpm; the release is 600 ms.
    let out = render(song, RATE as usize * 3);
    assert!(
        out[..RATE as usize / 4].iter().any(|s| s.abs() > 0.1),
        "sounds first"
    );
    assert!(
        out[RATE as usize * 2..].iter().all(|s| s.abs() < 1e-3),
        "then silent"
    );
}

#[test]
fn piano_keys_enter_notes_on_synth_tracks() {
    let mut e = Editor::new(sine_song(Tuning::default()));
    e.apply(Action::Char('z'));
    e.apply(Action::Char('q'));
    e.apply(Action::Char('.'));
    let cells = &e.pattern().tracks[0].cells;
    assert_eq!(cells[0].trigger.unwrap().slice, 60, "z is C-4 in octave 4");
    assert_eq!(cells[1].trigger.unwrap().slice, 72, "q is an octave up");
    assert_eq!(cells[2].trigger, None);
    assert_eq!(cells[2].fx[0], Some(Fx::Cut(0)), ". is note off");
    e.apply(Action::Octave(-1));
    e.apply(Action::Char('a'));
    assert_eq!(
        e.pattern().tracks[0].cells[3].trigger,
        None,
        "a isn't a note key"
    );
    e.apply(Action::Char('x'));
    assert_eq!(
        e.pattern().tracks[0].cells[3].trigger.unwrap().slice,
        50,
        "D-3"
    );
    // Velocity and effects still take hex.
    e.go_to(0, 0);
    e.cursor.field = Field::Velocity;
    e.apply(Action::Char('4'));
    e.apply(Action::Char('0'));
    assert_eq!(
        e.pattern().tracks[0].cells[0].trigger.unwrap().velocity,
        0x40
    );
}

#[test]
fn synth_knobs_undo_per_drag() {
    let mut e = Editor::new(sine_song(Tuning::default()));
    e.set_synth_param(0, 0, 2.0, true);
    e.set_synth_param(0, 0, 4.0, false);
    let Instrument::Synth(p) = &e.song.instruments[0] else {
        panic!()
    };
    assert_eq!(p.params[0], 4.0);
    e.apply(Action::Undo);
    let Instrument::Synth(p) = &e.song.instruments[0] else {
        panic!()
    };
    assert_eq!(p.params[0], SynthKind::Fm.params()[0].default);
}

#[test]
fn synths_and_tuning_survive_save_and_load() {
    let dir = std::env::temp_dir().join(format!("cowbell-test-synth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("song.cowbell");
    let mut song = sine_song(Tuning::from_scala("fifths\n1\n3/2\n").unwrap());
    let mut acid = SynthPatch::new(SynthKind::Acid, "bass");
    acid.params[1] = 900.0;
    song.instruments
        .push(Instrument::Synth(Arc::new(acid.clone())));
    song.patterns[0].tracks[0].cells[0] = Cell::hit(61, 127).with_fx(Fx::Glide(10));
    cowbell::project::save(&song, &path).unwrap();
    let loaded = cowbell::project::load(&path).unwrap();
    assert_eq!(loaded.tuning, song.tuning);
    let Instrument::Synth(p) = &loaded.instruments[1] else {
        panic!("not a synth")
    };
    assert_eq!(**p, acid);
    assert_eq!(
        loaded.patterns[0].tracks[0].cells[0],
        song.patterns[0].tracks[0].cells[0]
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Strength of frequency `f` in `out` (single-bin DFT).
fn magnitude(out: &[f32], f: f64) -> f64 {
    let w = std::f64::consts::TAU * f / RATE as f64;
    let (mut re, mut im) = (0.0, 0.0);
    for (n, &s) in out.iter().enumerate() {
        re += s as f64 * (w * n as f64).cos();
        im -= s as f64 * (w * n as f64).sin();
    }
    (re * re + im * im).sqrt() / out.len() as f64
}

#[test]
fn chord_effect_plays_all_its_notes() {
    let mut song = sine_song(Tuning::default());
    song.patterns[0].tracks[0].cells[0] = Cell::hit(57, 127).with_fx(Fx::Chord(0x47));
    let out = render(song, RATE as usize);
    let t = Tuning::default();
    let out = &out[4800..];
    let [root, third, fifth, second] = [57, 61, 64, 59].map(|n| magnitude(out, t.freq(n)));
    assert!(
        root > 0.05 && third > 0.05 && fifth > 0.05,
        "{root} {third} {fifth}"
    );
    assert!(second < root / 20.0, "no note a whole step up: {second}");
}

#[test]
fn knob_effects_automate_rack_and_synth_knobs() {
    use cowbell::engine::{Command, Position};
    use cowbell::pattern::Target;

    // Master volume to zero from row 2 (a quarter second in).
    let mut song = sine_song(Tuning::default());
    let master = song
        .rack
        .devices
        .iter()
        .find(|d| d.kind == cowbell::rack::Kind::Master)
        .unwrap()
        .id;
    song.automation = vec![Some(Target::Rack {
        device: master,
        param: 0,
    })];
    song.patterns[0].tracks[0].cells[0] = Cell::hit(60, 127);
    song.patterns[0].tracks[0].cells[2] = Cell::default().with_fx(Fx::Knob(0));
    let (mut engine, mut c) = Engine::new(RATE, song.clone());
    engine.play();
    let mut out = vec![0.0; 2 * RATE as usize / 2];
    engine.process(&mut out);
    let quarter = RATE as usize / 4;
    assert!(
        out[..2 * quarter].iter().any(|s| s.abs() > 0.1),
        "sounds before"
    );
    assert!(
        out[2 * quarter + 2..].iter().all(|&s| s == 0.0),
        "silent after"
    );
    // Playing again starts from the rack's own setting.
    assert!(
        c.send(Command::PlayFrom(Position { order: 0, row: 0 }))
            .is_ok()
    );
    let mut again = vec![0.0; 2 * 4800];
    engine.process(&mut again);
    assert!(again.iter().any(|s| s.abs() > 0.1), "volume restored");

    // The synth's own volume knob (FM param 9).
    song.automation = vec![Some(Target::Synth { param: 9 })];
    let out = render(song, RATE as usize / 2);
    assert!(out[..quarter].iter().any(|s| s.abs() > 0.1));
    assert!(out[quarter + 1..].iter().all(|&s| s == 0.0));
}
