//! A demo song that uses each effect, for listening tests until there's an
//! editor.

use std::sync::Arc;

use crate::pattern::{Cell, Fx, Instrument, Pattern, Song};
use crate::sample::Sample;
use crate::transform::Roll;

pub const BPM: f64 = 172.0;

pub fn song(brk: Sample) -> Song {
    let n = brk.slices.len().max(1) as u16;
    let s = |i: u16| i % n;
    let instruments = vec![Instrument::Sampler(Arc::new(brk))];
    let tracks = [("brk", 0), ("snr", 0)];

    // Pattern 0: two bars in 16ths. The break mostly as recorded, with a few
    // hits edited.
    let mut p0 = Pattern::new(4, 8, &tracks);
    let straight = [
        (0, 0), (2, 1), (4, 2), (6, 3), (7, 4), (9, 5), (10, 6), (11, 7), (12, 8), (14, 9),
        (16, 10), (18, 11), (20, 12), (22, 13), (23, 14), (26, 15), (27, 16), (28, 17),
        (30, 18), (31, 19),
    ];
    let brk = &mut p0.tracks[0].cells;
    for (row, slice) in straight {
        brk[row] = Cell::hit(s(slice), 110);
    }
    brk[6] = Cell::hit(s(2), 100).with_fx(Fx::Reverse);
    brk[13] = Cell::hit(s(8), 90).with_fx(Fx::Offset(0x40)).with_fx(Fx::Pitch(5));
    brk[24] = Cell::hit(s(12), 120).with_fx(Fx::Crush(4));
    brk[29] = Cell::default().with_fx(Fx::Cut(4));

    // Pattern 1: one bar in 32nds. Chopped up, with a stutter and a roll that
    // speeds up and pitches up into the next bar.
    let mut p1 = Pattern::new(8, 4, &tracks);
    let brk = &mut p1.tracks[0].cells;
    brk[0] = Cell::hit(s(0), 120);
    brk[4] = Cell::hit(s(5), 100).with_fx(Fx::Retrig(3));
    brk[6] = Cell::hit(s(2), 110);
    brk[10] = Cell::hit(s(10), 110).with_fx(Fx::Stutter(30));
    brk[14] = Cell::hit(s(8), 100).with_fx(Fx::Reverse).with_fx(Fx::Pitch(-7));
    brk[16] = Cell::hit(s(4), 120).with_fx(Fx::Crush(6));
    Roll { slice: s(2), hits: (1, 6), pitch: (0, 12), velocity: (70, 120), curve: 2.0 }
        .apply(&mut p1.tracks[1], 20..32);

    Song { bpm: BPM, instruments, patterns: vec![p0, p1], order: vec![0, 1, 0, 1] }
}
