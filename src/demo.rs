//! The demo song: melodic drill 'n' bass at 175 bpm, in D minor. Chopped
//! break edits with stutters, reverses and snare rolls that speed up and
//! rise, under strings, a horn-like gliding lead, celesta and a quietly
//! menacing bass that creeps by semitones into each chord.
//!
//! The drums are real: a CC0 kit (Karoryfer's Gogodze Phu Vol II, see
//! `assets/drums/README.md`), built into a two-bar break at load time and
//! sliced like one, plus toms and a few other hits for fills.
//!
//! 32 bars:
//!
//! | bars  | section   | what plays                                       |
//! |-------|-----------|--------------------------------------------------|
//! | 1–4   | intro     | strings, celesta, low toms like timpani; the     |
//! |       |           | bass creeps in                                   |
//! | 5–12  | groove    | break, bass, strings, horn; rolls into 9 and 13  |
//! | 13–16 | breakdown | half-time break and toms, strings swelling,      |
//! |       |           | celesta, a long horn line; a big roll into 17    |
//! | 17–24 | groove    | back, its drums more chopped, celesta joining    |
//! | 25–28 | bridge    | sidesticks and tom fills, the bass at its most   |
//! |       |           | menacing, no horn                                |
//! | 29–32 | outro     | the break thinning out; strings end on D minor   |
//!
//! The chords: Dm7 B♭maj7 Gm7 A7, then Dm7 Am7 B♭m C♯dim, a bar each.

use std::sync::Arc;

use crate::rack::{Jack, Kind, Rack};
use crate::sample::Sample;
use crate::song::{Cell, Clip, Fx, Instrument, Song, TICKS_PER_BAR, Track};
use crate::synthvoice::{SynthKind, SynthPatch};
use crate::transform::Roll;

pub const BPM: f64 = 175.0;
pub const BARS: u64 = 32;

/// The kit's samples, as recorded.
pub const KIT_RATE: u32 = 44_100;

/// The kit, built into the program so the demo works anywhere.
const KIT: [&[u8]; 18] = [
    include_bytes!("../assets/drums/kick_mic/ks_vl3_rr1.wav"),
    include_bytes!("../assets/drums/kick_mic/ks_vl5_rr1.wav"),
    include_bytes!("../assets/drums/kick_mic/ks_vl6_rr1.wav"),
    include_bytes!("../assets/drums/kick_mic/ks_vl6_rr2.wav"),
    include_bytes!("../assets/drums/snare_mic/sc_vl3_rr1.wav"),
    include_bytes!("../assets/drums/snare_mic/sc_vl5_rr1.wav"),
    include_bytes!("../assets/drums/snare_mic/sc_vl6_rr1.wav"),
    include_bytes!("../assets/drums/snare_mic/sc_vl6_rr2.wav"),
    include_bytes!("../assets/drums/snare_mic/se_vl4_rr1.wav"),
    include_bytes!("../assets/drums/snare_mic/ss_vl3_rr1.wav"),
    include_bytes!("../assets/drums/front_mic/hc_vl2_rr1.wav"),
    include_bytes!("../assets/drums/front_mic/hc_vl3_rr1.wav"),
    include_bytes!("../assets/drums/front_mic/hc_vl3_rr2.wav"),
    include_bytes!("../assets/drums/front_mic/hl_vl3_rr1.wav"),
    include_bytes!("../assets/drums/front_mic/ho_vl3_rr1.wav"),
    include_bytes!("../assets/drums/front_mic/th_vl4_rr1.wav"),
    include_bytes!("../assets/drums/front_mic/tm_vl4_rr1.wav"),
    include_bytes!("../assets/drums/front_mic/tl_vl4_rr1.wav"),
];

/// Indices into [`KIT`].
mod hit {
    pub const SOFT_KICK: usize = 1;
    pub const KICKS: [usize; 2] = [2, 3];
    pub const GHOST: usize = 4;
    pub const SNARES: [usize; 2] = [6, 7];
    pub const EDGE: usize = 8;
    pub const SIDESTICK: usize = 9;
    pub const HATS: [usize; 3] = [10, 11, 12];
    pub const LOOSE_HAT: usize = 13;
    pub const OPEN_HAT: usize = 14;
    pub const TOMS: [usize; 3] = [15, 16, 17];
}

/// The break, a sixteenth a character, two bars: `K` kick (`k` softer),
/// `S` snare (`g` ghost), `h` closed hat, `l` loose hat, `o` open hat.
const KICK_LANE: &str = "K......K.k......K......k..K.....";
const SNARE_LANE: &str = "....S.......S..g....S......gS..S";
const HAT_LANE: &str = "h.h.h.h.h.hlh.h.h.h.h.h.h.hlh.o.";
/// One-shots after the break, a slice each: high, mid and low tom, open
/// hat, sidestick, snare edge.
const EXTRAS: [usize; 6] = [
    hit::TOMS[0],
    hit::TOMS[1],
    hit::TOMS[2],
    hit::OPEN_HAT,
    hit::SIDESTICK,
    hit::EDGE,
];
/// One-shots are cut to this long, with a short fade.
const MAX_HIT_S: f32 = 1.5;

/// The break's slices by sound, then the extras.
struct Layout {
    kicks: Vec<u16>,
    snares: Vec<u16>,
    hats: Vec<u16>,
    /// High, mid, low.
    toms: [u16; 3],
    open: u16,
    sidestick: u16,
    edge: u16,
}

impl Layout {
    fn new() -> Self {
        let mut l = Layout {
            kicks: Vec::new(),
            snares: Vec::new(),
            hats: Vec::new(),
            toms: [0; 3],
            open: 0,
            sidestick: 0,
            edge: 0,
        };
        let mut slice = 0;
        for step in 0..32 {
            let at = |lane: &str| lane.as_bytes()[step] != b'.';
            if !(at(KICK_LANE) || at(SNARE_LANE) || at(HAT_LANE)) {
                continue;
            }
            // A kick with a hat over it is a kick.
            if at(KICK_LANE) {
                l.kicks.push(slice);
            } else if at(SNARE_LANE) {
                l.snares.push(slice);
            } else {
                l.hats.push(slice);
            }
            slice += 1;
        }
        l.toms = [slice, slice + 1, slice + 2];
        l.open = slice + 3;
        l.sidestick = slice + 4;
        l.edge = slice + 5;
        l
    }
}

/// One of the kit's hits, cut to [`MAX_HIT_S`].
fn kit_hit(i: usize) -> Vec<[f32; 2]> {
    let s = Sample::from_wav_bytes("kit", KIT[i]).expect("the built-in kit loads");
    let max = (KIT_RATE as f32 * MAX_HIT_S) as usize;
    let mut f: Vec<[f32; 2]> = s.frames.iter().take(max).copied().collect();
    let fade = (KIT_RATE as usize / 50).min(f.len());
    let n = f.len();
    for (k, frame) in f[n - fade..].iter_mut().enumerate() {
        let g = 1.0 - k as f32 / fade as f32;
        *frame = [frame[0] * g, frame[1] * g];
    }
    f
}

/// The built-in break: the kit played as two bars at [`BPM`], sliced on
/// every hit, with the extras after it.
pub fn kit() -> Sample {
    let step = (KIT_RATE as f64 * 60.0 / BPM / 4.0).round() as usize;
    let mut frames = vec![[0.0f32; 2]; 32 * step];
    let mut slices = Vec::new();
    let (mut kicks, mut snares, mut hats) = (0, 0, 0);
    let cycle = |list: &[usize], n: &mut usize| {
        let i = list[*n % list.len()];
        *n += 1;
        i
    };
    for s in 0..32 {
        let mut hits = Vec::new();
        match KICK_LANE.as_bytes()[s] {
            b'K' => hits.push((cycle(&hit::KICKS, &mut kicks), 1.0)),
            b'k' => hits.push((hit::SOFT_KICK, 0.9)),
            _ => {}
        }
        match SNARE_LANE.as_bytes()[s] {
            b'S' => hits.push((cycle(&hit::SNARES, &mut snares), 1.0)),
            b'g' => hits.push((hit::GHOST, 0.35)),
            _ => {}
        }
        match HAT_LANE.as_bytes()[s] {
            b'h' => hits.push((cycle(&hit::HATS, &mut hats), 0.45)),
            b'l' => hits.push((hit::LOOSE_HAT, 0.5)),
            b'o' => hits.push((hit::OPEN_HAT, 0.45)),
            _ => {}
        }
        if hits.is_empty() {
            continue;
        }
        slices.push(s * step);
        for (i, gain) in hits {
            for (o, h) in frames[s * step..].iter_mut().zip(kit_hit(i)) {
                o[0] += h[0] * gain;
                o[1] += h[1] * gain;
            }
        }
    }
    for i in EXTRAS {
        slices.push(frames.len());
        frames.extend(kit_hit(i));
    }
    let peak = frames
        .iter()
        .flat_map(|f| f.iter())
        .fold(1e-6f32, |m, s| m.max(s.abs()));
    for f in &mut frames {
        *f = [f[0] * 0.9 / peak, f[1] * 0.9 / peak];
    }
    let mut sample = Sample::new("kit", KIT_RATE, frames);
    sample.slices = slices;
    sample
}

/// Break edits, a character per sixteenth, two bars each:
///
/// - `k` kick, `u` stuttered kick
/// - `s` snare, `S` accented, `g` ghost, `r` reversed, `x` crushed,
///   `p` pitched up a fourth, `e` edge, `i` sidestick
/// - `h` hat, `t` hat rolled in three, `o` hat started halfway in, `O` open
/// - `T` `M` `L` high, mid, low tom
/// - `c` cut whatever is ringing, `.` nothing
const INTRO_A: &str = "L.......h...h...L.......h...h.h.";
const INTRO_B: &str = "L.......h...h...L...h...M.T.r.rr";
const GROOVE: &str = "k.h.s.hk.kh.s.h.k.h.s.hk..khsc.h";
const EDIT: &str = "k.h.s.tk.kh.g.hrk.h.s.uk..kts.gS";
const CHOP: &str = "k.tos.hku.hgx.hrk.hts.pk.kus.xpS";
const HALF_A: &str = "L.......r...o...k.....h.r...gg..";
const HALF_B: &str = "L.....o.r.......k..k..h.r.r.TMLS";
const EDIT_2: &str = "k.ts.thkuk.g.ths.ktg.ruk.kts.gtS";
const CHOP_2: &str = "kutos.hkuuhgx.trk.hts.pkukusxxpS";
const BRIDGE_A: &str = "k.i.s.ikk.i.e.i.k.i.s.ik.ki.TMLS";
const BRIDGE_B: &str = "k.i.s.ik.ki.e.irk.i.s.ikuk.TTMML";
const OUT_A: &str = "k.h...h.k.h...h.k.h...h...k.h...";
const OUT_B: &str = "L.h...h.....O...L.......r.......";

/// The chords: (strings root, the `Chord` effect for the rest, bass root).
const CHORDS: [(u16, u8, u16); 9] = [
    (62, 0x3a, 38), // Dm7
    (58, 0x4b, 34), // B♭maj7
    (55, 0x3a, 31), // Gm7
    (57, 0x4a, 33), // A7
    (62, 0x3a, 38), // Dm7
    (57, 0x3a, 33), // Am7
    (58, 0x37, 34), // B♭m, borrowed: darker
    (61, 0x36, 37), // C♯dim
    (62, 0x37, 38), // Dm, to end on
];
/// The chords of each section, a bar each, by index into [`CHORDS`].
const FIRST_HALF: [usize; 4] = [0, 1, 2, 3];
const ALL: [usize; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
const SECOND_HALF: [usize; 4] = [4, 5, 6, 7];
const ENDING: [usize; 4] = [0, 1, 2, 8];

/// A horn line: per bar, (sixteenth, note, glides into it).
type Line = [&'static [(usize, u16, bool)]; 4];
const HORN_A: Line = [
    &[
        (0, 74, false),
        (6, 72, true),
        (8, 69, false),
        (12, 70, false),
    ],
    &[(0, 69, false), (8, 65, true), (12, 67, false)],
    &[
        (0, 70, false),
        (4, 69, true),
        (8, 67, false),
        (12, 62, false),
    ],
    &[
        (0, 64, false),
        (6, 61, true),
        (8, 64, false),
        (12, 69, true),
    ],
];
const HORN_B: Line = [
    &[
        (0, 77, false),
        (4, 76, true),
        (8, 74, false),
        (14, 72, false),
    ],
    &[(0, 72, false), (8, 69, true), (12, 72, false)],
    &[
        (0, 74, false),
        (4, 70, true),
        (8, 69, false),
        (12, 65, false),
    ],
    &[(0, 64, false), (8, 67, true), (12, 61, false)],
];
/// The breakdown: long notes, sliding.
const HORN_LONG: Line = [
    &[(0, 81, false)],
    &[(4, 77, true)],
    &[(0, 74, false), (8, 76, true)],
    &[(0, 73, false), (8, 69, true)],
];

/// How a section's bass goes.
#[derive(Clone, Copy, PartialEq)]
enum Bass {
    /// Long notes, quietly.
    Creep,
    /// Pulsing, creeping a semitone into the next chord.
    Pulse,
    /// Pulsing, dropping an octave mid-bar.
    Menace,
}

/// A section: its first bar, its chords, and what plays in it.
struct Section {
    bar: u64,
    chords: &'static [usize],
    bass: Option<Bass>,
    strings: u8,
    horn: bool,
    celesta: bool,
}

const SECTIONS: [Section; 6] = [
    Section {
        bar: 0,
        chords: &FIRST_HALF,
        bass: Some(Bass::Creep),
        strings: 85,
        horn: false,
        celesta: true,
    },
    Section {
        bar: 4,
        chords: &ALL,
        bass: Some(Bass::Pulse),
        strings: 80,
        horn: true,
        celesta: false,
    },
    Section {
        bar: 12,
        chords: &FIRST_HALF,
        bass: None,
        strings: 105,
        horn: true,
        celesta: true,
    },
    Section {
        bar: 16,
        chords: &ALL,
        bass: Some(Bass::Pulse),
        strings: 80,
        horn: true,
        celesta: false,
    },
    Section {
        bar: 24,
        chords: &SECOND_HALF,
        bass: Some(Bass::Menace),
        strings: 90,
        horn: false,
        celesta: false,
    },
    Section {
        bar: 28,
        chords: &ENDING,
        bass: Some(Bass::Creep),
        strings: 85,
        horn: false,
        celesta: true,
    },
];

pub fn song(brk: Sample) -> Song {
    let l = Layout::new();
    let n = brk.slices.len().max(1) as u16;
    // Another break may have fewer slices: wrap around.
    let s = |i: u16| i % n;
    let instruments = vec![
        Instrument::Sampler(Arc::new(brk)),
        synth(
            "bass",
            &[1.0, 1.4, 1800.0, 0.25, 15.0, 3000.0, 0.8, 180.0, 0.0, 0.6],
        ),
        synth(
            "strings",
            &[
                1.0, 0.7, 3000.0, 0.35, 900.0, 3000.0, 0.85, 1800.0, 7.0, 0.32,
            ],
        ),
        synth(
            "horn",
            &[1.0, 2.2, 1200.0, 0.1, 70.0, 2500.0, 0.7, 350.0, 2.0, 0.34],
        ),
        synth(
            "celesta",
            &[4.0, 2.5, 250.0, 0.0, 1.0, 1600.0, 0.0, 1200.0, 0.0, 0.26],
        ),
    ];
    let at = |bar: u64| bar * TICKS_PER_BAR;

    let edits = [
        (0, INTRO_A),
        (2, INTRO_B),
        (4, GROOVE),
        (6, EDIT),
        (8, GROOVE),
        (10, CHOP),
        (12, HALF_A),
        (14, HALF_B),
        (16, EDIT),
        (18, EDIT_2),
        (20, GROOVE),
        (22, CHOP_2),
        (24, BRIDGE_A),
        (26, BRIDGE_B),
        (28, OUT_A),
        (30, OUT_B),
    ];
    let mut brk = Track::new("brk", 0);
    for (bar, pattern) in edits {
        brk = brk.with_clip(break_clip(at(bar), pattern, &l, &s));
    }

    // Snare rolls into new sections, speeding up and rising; the ones out
    // of the breakdown and the second groove take longer.
    let roll = |i: usize, top| Roll {
        slice: s(l.snares[i % l.snares.len()]),
        hits: (1, 6),
        pitch: (0, top),
        velocity: (60, 120),
        curve: 2.0,
    };
    let rolls = [
        (7, roll(0, 7), 8..16),
        (11, roll(1, 12), 8..16),
        (
            15,
            Roll {
                hits: (1, 8),
                pitch: (-5, 17),
                velocity: (40, 127),
                ..roll(2, 0)
            },
            0..16,
        ),
        (19, roll(3, 7), 8..16),
        (
            23,
            Roll {
                hits: (2, 8),
                pitch: (0, 19),
                ..roll(4, 0)
            },
            4..16,
        ),
        (27, roll(5, 12), 8..16),
    ];
    let mut snr = Track::new("snr", 0);
    for (bar, roll, rows) in rolls {
        let mut clip = Clip::beats(at(bar), 4, 4);
        roll.apply(&mut clip.cells, rows);
        snr = snr.with_clip(clip);
    }

    let mut bass = Track::new("bass", 1);
    let mut strings = Track::new("strings", 2);
    let mut horn = Track::new("horn", 3);
    let mut celesta = Track::new("celesta", 4);
    for (k, sec) in SECTIONS.iter().enumerate() {
        let last = k + 1 == SECTIONS.len();
        // The next section's first root, for the bass to creep to.
        let next = SECTIONS
            .get(k + 1)
            .map_or(CHORDS[sec.chords[0]].2, |n| CHORDS[n.chords[0]].2);
        if let Some(style) = sec.bass {
            // The intro's bass comes in on its third bar.
            let skip = if sec.bar == 0 { 2 } else { 0 };
            let chords = &sec.chords[skip..];
            bass = bass.with_clip(bass_clip(
                at(sec.bar + skip as u64),
                chords,
                next,
                style,
                last,
            ));
        }
        strings = strings.with_clip(strings_clip(at(sec.bar), sec.chords, sec.strings, last));
        if sec.horn {
            if sec.chords.len() == 8 {
                horn = horn.with_clip(horn_clip(at(sec.bar), &HORN_A));
                horn = horn.with_clip(horn_clip(at(sec.bar + 4), &HORN_B));
            } else {
                horn = horn.with_clip(horn_clip(at(sec.bar), &HORN_LONG));
            }
        }
        if sec.celesta {
            celesta = celesta.with_clip(celesta_clip(at(sec.bar), sec.chords));
        }
    }
    // The celesta joins the second groove's second half.
    celesta = celesta.with_clip(celesta_clip(at(20), &SECOND_HALF));

    let tracks = vec![brk, snr, bass, strings, horn, celesta];
    let mut song = Song::new(BPM, instruments, tracks);
    song.rack = rack();
    // Balanced by ear and by peak: the mix stays under full scale.
    // The drums lead; the bass sits under them, quietly.
    for (t, volume) in [(0, 1.2), (1, 1.0), (2, 0.5), (3, 0.6), (4, 1.1), (5, 1.2)] {
        song.set_volume(t, volume);
    }
    song
}

fn synth(name: &str, params: &[f32]) -> Instrument {
    let mut patch = SynthPatch::new(SynthKind::Fm, name);
    patch.params = params.to_vec();
    patch.repair();
    Instrument::Synth(Arc::new(patch))
}

fn cut() -> Cell {
    Cell::default().with_fx(Fx::Cut(0))
}

/// A two-bar break clip of sixteenths from a pattern string.
fn break_clip(start: u64, pattern: &str, l: &Layout, s: &impl Fn(u16) -> u16) -> Clip {
    assert_eq!(pattern.len(), 32, "{pattern}");
    let mut clip = Clip::beats(start, 4, 8);
    // Each kind of hit cycles through its slices, so repeats sound
    // different.
    let (mut k, mut sn, mut h) = (0, 0, 0);
    let next = |list: &[u16], i: &mut usize| {
        let v = list[*i % list.len()];
        *i += 1;
        s(v)
    };
    for (row, c) in pattern.chars().enumerate() {
        clip.cells[row] = match c {
            'k' => Cell::hit(next(&l.kicks, &mut k), 120),
            'u' => Cell::hit(next(&l.kicks, &mut k), 110).with_fx(Fx::Stutter(40)),
            's' => Cell::hit(next(&l.snares, &mut sn), 112),
            'S' => Cell::hit(next(&l.snares, &mut sn), 127),
            'g' => Cell::hit(next(&l.snares, &mut sn), 55),
            'r' => Cell::hit(next(&l.snares, &mut sn), 100).with_fx(Fx::Reverse),
            'x' => Cell::hit(next(&l.snares, &mut sn), 110).with_fx(Fx::Crush(5)),
            'p' => Cell::hit(next(&l.snares, &mut sn), 100).with_fx(Fx::Pitch(5)),
            'e' => Cell::hit(s(l.edge), 95),
            'i' => Cell::hit(s(l.sidestick), 90),
            'h' => Cell::hit(next(&l.hats, &mut h), 78),
            't' => Cell::hit(next(&l.hats, &mut h), 72).with_fx(Fx::Retrig(3)),
            'o' => Cell::hit(next(&l.hats, &mut h), 70).with_fx(Fx::Offset(0x40)),
            'O' => Cell::hit(s(l.open), 80),
            'T' => Cell::hit(s(l.toms[0]), 105),
            'M' => Cell::hit(s(l.toms[1]), 110),
            'L' => Cell::hit(s(l.toms[2]), 115),
            'c' => Cell::default().with_fx(Fx::Cut(4)),
            _ => Cell::default(),
        };
    }
    clip
}

/// The bass for a section: `next` is the root to creep to at the end.
fn bass_clip(start: u64, chords: &[usize], next: u16, style: Bass, last: bool) -> Clip {
    let mut clip = Clip::beats(start, 4, 4 * chords.len() as u32);
    for (b, &c) in chords.iter().enumerate() {
        let root = CHORDS[c].2;
        let to = chords.get(b + 1).map_or(next, |&n| CHORDS[n].2);
        // A semitone off the next root, from the side it's on.
        let creep = if to < root { to + 1 } else { to - 1 };
        let row = b * 16;
        match style {
            Bass::Creep => {
                clip.cells[row] = Cell::hit(root, 90);
                if to != root {
                    clip.cells[row + 12] = Cell::hit(creep, 70).with_fx(Fx::Glide(60));
                }
            }
            Bass::Pulse | Bass::Menace => {
                clip.cells[row] = Cell::hit(root, 110);
                clip.cells[row + 6] = Cell::hit(root, 80);
                if style == Bass::Menace {
                    clip.cells[row + 8] = Cell::hit(root - 12, 115).with_fx(Fx::Glide(20));
                } else {
                    clip.cells[row + 10] = Cell::hit(root, 95);
                }
                if to != root {
                    clip.cells[row + 14] = Cell::hit(creep, 85).with_fx(Fx::Glide(30));
                }
            }
        }
    }
    if last {
        // The last note rings out at the end.
        let n = clip.cells.len();
        clip.cells[n - 4] = Cell::hit(CHORDS[*chords.last().unwrap()].2, 90);
        clip.cells[n - 1] = cut();
    }
    clip
}

/// A chord a bar.
fn strings_clip(start: u64, chords: &[usize], velocity: u8, last: bool) -> Clip {
    let mut clip = Clip::beats(start, 4, 4 * chords.len() as u32);
    for (b, &c) in chords.iter().enumerate() {
        let (root, chord, _) = CHORDS[c];
        clip.cells[b * 16] = Cell::hit(root, velocity).with_fx(Fx::Chord(chord));
    }
    if last {
        let n = clip.cells.len();
        clip.cells[n - 1] = cut();
    }
    clip
}

/// Four bars of horn. Glides take 100 ms.
fn horn_clip(start: u64, line: &Line) -> Clip {
    let mut clip = Clip::beats(start, 4, 16);
    for (bar, notes) in line.iter().enumerate() {
        for &(row, note, glide) in *notes {
            let mut cell = Cell::hit(note, 100);
            if glide {
                cell = cell.with_fx(Fx::Glide(25));
            }
            clip.cells[bar * 16 + row] = cell;
        }
    }
    // Let go at the end, rather than hold into whatever comes next.
    clip.cells[63] = cut();
    clip
}

/// Celesta: each chord's notes rising, an octave up, in quarter notes,
/// with a soft echo.
fn celesta_clip(start: u64, chords: &[usize]) -> Clip {
    let mut clip = Clip::beats(start, 4, 4 * chords.len() as u32);
    for (b, &c) in chords.iter().enumerate() {
        let (root, chord, _) = CHORDS[c];
        let (x, y) = ((chord >> 4) as u16, (chord & 0x0f) as u16);
        let tones = [root + 12, root + 12 + x, root + 12 + y, root + 24];
        let row = b * 16;
        for (k, &tone) in tones.iter().enumerate() {
            clip.cells[row + 4 * k] = Cell::hit(tone, 100 - 8 * k as u8);
        }
        clip.cells[row + 14] = Cell::hit(tones[1], 55);
    }
    clip
}

/// Every track into the mixer, the master turned down a little, with:
/// - horn → filter, its cutoff swept by a slow LFO
/// - snr → delay, a dotted eighth
/// - celesta → delay, a quarter
fn rack() -> Rack {
    let mut rack = Rack::simple(6);
    let jack = |device, port| Jack { device, port };
    // Six tracks add up: leave headroom so the loudest moments stay
    // under full scale.
    let master = rack
        .devices
        .iter()
        .find(|d| d.kind == Kind::Master)
        .unwrap()
        .id;
    rack.set_param(master, 0, 0.62);
    // A dotted eighth and a quarter, in milliseconds.
    let dotted = (60_000.0 / BPM * 0.75) as f32;
    let quarter = (60_000.0 / BPM) as f32;

    let filter = rack.append_effect(4, Kind::Filter).unwrap();
    rack.set_param(filter, 0, 1500.0);
    rack.set_param(filter, 1, 0.25);
    rack.set_param(filter, 3, 0.45);
    let lfo = rack.add(Kind::Lfo);
    rack.set_param(lfo, 0, 0.2);
    rack.connect(jack(lfo, 0), jack(filter, 1)).unwrap();

    let delay = rack.append_effect(1, Kind::Delay).unwrap();
    rack.set_param(delay, 0, dotted);
    rack.set_param(delay, 1, 0.35);
    rack.set_param(delay, 2, 0.25);

    let delay = rack.append_effect(5, Kind::Delay).unwrap();
    rack.set_param(delay, 0, quarter);
    rack.set_param(delay, 1, 0.45);
    rack.set_param(delay, 2, 0.35);
    rack
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> Song {
        song(kit())
    }

    #[test]
    fn the_kit_is_a_break_and_extras() {
        let k = kit();
        let l = Layout::new();
        assert_eq!(k.slices.len(), l.edge as usize + 1);
        assert_eq!(k.sample_rate, KIT_RATE);
        let peak = k
            .frames
            .iter()
            .flat_map(|f| f.iter())
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.9).abs() < 1e-3, "normalised, not {peak}");
        assert!(
            k.frames
                .iter()
                .all(|f| f[0].is_finite() && f[1].is_finite())
        );
        assert_eq!(l.kicks.len(), 6);
        assert_eq!(l.snares.len(), 7, "ghosts too");
    }

    #[test]
    fn thirty_two_bars_in_sections() {
        let song = demo();
        assert_eq!(song.end(), BARS * TICKS_PER_BAR);
        let names: Vec<&str> = song.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["brk", "snr", "bass", "strings", "horn", "celesta"]);
        assert_eq!(
            song.tracks[0].clips.len(),
            16,
            "the break all the way through"
        );
        assert_eq!(song.tracks[1].clips.len(), 6, "rolls into sections");
    }

    #[test]
    fn heavy_on_minor_chords() {
        let song = demo();
        let chords: Vec<u8> = song.tracks[3]
            .clips
            .iter()
            .flat_map(|c| &c.cells)
            .filter_map(|c| {
                c.fx().find_map(|f| match f {
                    Fx::Chord(n) => Some(n),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(chords.len() as u64, BARS, "a chord a bar");
        let minor = chords.iter().filter(|&&n| n >> 4 == 3).count();
        assert!(
            minor * 3 >= chords.len() * 2,
            "{minor} of {} minor",
            chords.len()
        );
    }

    #[test]
    fn the_horn_glides_and_the_bass_creeps() {
        let song = demo();
        let glides = |t: usize| {
            song.tracks[t]
                .clips
                .iter()
                .flat_map(|c| &c.cells)
                .filter(|c| c.fx().any(|f| matches!(f, Fx::Glide(_))))
                .count()
        };
        assert!(glides(4) > 0);
        assert!(glides(2) > 8, "into nearly every chord");
    }

    #[test]
    fn rack_has_the_filter_and_delays() {
        let song = demo();
        let kinds = |t: usize| -> Vec<Kind> {
            song.rack
                .chain(t)
                .iter()
                .map(|&id| song.rack.device(id).unwrap().kind)
                .collect()
        };
        assert_eq!(kinds(4), [Kind::Filter]);
        assert_eq!(kinds(1), [Kind::Delay]);
        assert_eq!(kinds(5), [Kind::Delay]);
        let filter = song.rack.chain(4)[0];
        let cv = song
            .rack
            .cable_into(Jack {
                device: filter,
                port: 1,
            })
            .unwrap();
        assert_eq!(song.rack.device(cv.from.device).unwrap().kind, Kind::Lfo);
    }
}
