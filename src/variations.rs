//! Variations: copies of a pattern with one track's cells varied, from mild
//! to wild, to audition one after another and keep what works.
//!
//! The changes stay musical: hits are rearranged, retriggered, reversed or
//! shifted by an octave, fifth or fourth; synth notes move by those
//! intervals and pick up glides and accents. Nothing is random beyond a
//! seed, so the same seed gives the same variations.

use std::ops::Range;

use crate::pattern::{Cell, Fx, Pattern};
use crate::tuning::{HIGHEST_NOTE, LOWEST_NOTE};

/// How strongly each of the variations changes things, mild to wild.
pub const STRENGTHS: [f32; 4] = [0.25, 0.5, 0.75, 1.0];

pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Rng(seed.max(1))
    }

    fn next(&mut self) -> u32 {
        // xorshift32
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    /// True with probability `p`.
    fn chance(&mut self, p: f32) -> bool {
        (self.next() as f64 / u32::MAX as f64) < p as f64
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.next() as usize % items.len()]
    }

    fn range(&mut self, lo: u8, hi: u8) -> u8 {
        lo + (self.next() % (hi - lo + 1) as u32) as u8
    }
}

/// Put `fx` in the cell's first free effect column, if it has one and
/// doesn't already use that effect.
fn add_fx(cell: &mut Cell, fx: Fx) {
    if cell.fx().any(|f| f.letter() == fx.letter()) {
        return;
    }
    if let Some(slot) = cell.fx.iter_mut().find(|f| f.is_none()) {
        *slot = Some(fx);
    }
}

/// A copy of `pattern` with `rows` of track `track` varied by `strength`
/// (0..=1). `synth` says whether the track plays a synth (cells hold notes).
pub fn vary(
    pattern: &Pattern,
    track: usize,
    rows: Range<usize>,
    synth: bool,
    strength: f32,
    rng: &mut Rng,
) -> Pattern {
    let mut out = pattern.clone();
    let Some(t) = out.tracks.get_mut(track) else {
        return out;
    };
    let rows = rows.start.min(t.cells.len())..rows.end.min(t.cells.len());
    let s = strength.clamp(0.0, 1.0);
    let original: Vec<Cell> = t.cells[rows.clone()].to_vec();
    let hits: Vec<Cell> = original
        .iter()
        .copied()
        .filter(|c| c.trigger.is_some())
        .collect();
    if hits.is_empty() {
        return out;
    }
    let mut previous: Option<Cell> = None;
    for r in rows {
        let cell = &mut t.cells[r];
        if let Some(mut tr) = cell.trigger {
            if synth {
                if rng.chance(0.3 * s) {
                    let step = rng.pick(&[-12i16, -7, -5, 5, 7, 12]);
                    let note = tr.slice as i16 + step;
                    if (LOWEST_NOTE as i16..=HIGHEST_NOTE as i16).contains(&note) {
                        tr.slice = note as u16;
                    }
                }
                if rng.chance(0.25 * s) {
                    tr.velocity = if tr.velocity > 100 { 0x50 } else { 0x7f };
                }
                cell.trigger = Some(tr);
                if rng.chance(0.2 * s) {
                    add_fx(cell, Fx::Glide(rng.range(0x08, 0x20)));
                }
            } else {
                if rng.chance(0.35 * s) {
                    tr.slice = rng.pick(&hits).trigger.unwrap().slice;
                }
                cell.trigger = Some(tr);
                if rng.chance(0.25 * s) {
                    add_fx(cell, Fx::Retrig(rng.range(2, 6)));
                }
                if rng.chance(0.15 * s) {
                    add_fx(cell, Fx::Reverse);
                }
                if rng.chance(0.15 * s) {
                    add_fx(cell, Fx::Pitch(rng.pick(&[-12i8, -7, -5, 5, 7, 12])));
                }
                if rng.chance(0.1 * s) {
                    add_fx(cell, Fx::Offset(rng.range(0x20, 0xc0)));
                }
            }
            if rng.chance(0.1 * s) {
                *cell = Cell::default();
            }
        } else if cell.fx().next().is_none() {
            if synth {
                if let Some(p) = previous.filter(|_| rng.chance(0.08 * s)) {
                    *cell = p;
                }
            } else if rng.chance(0.12 * s) {
                // A ghost hit: something from the bar, quieter.
                let mut ghost = rng.pick(&hits);
                ghost.trigger.as_mut().unwrap().velocity = rng.range(0x40, 0x68);
                *cell = ghost;
            }
        }
        if cell.trigger.is_some() {
            previous = Some(*cell);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern() -> Pattern {
        let mut p = Pattern::new(4, 4, &[("a", 0), ("b", 0)]);
        for r in (0..16).step_by(2) {
            p.tracks[0].cells[r] = Cell::hit((r / 2) as u16, 100);
            p.tracks[1].cells[r] = Cell::hit(60 + r as u16, 100);
        }
        p
    }

    fn changed(a: &Pattern, b: &Pattern, t: usize) -> usize {
        a.tracks[t]
            .cells
            .iter()
            .zip(&b.tracks[t].cells)
            .filter(|(x, y)| x != y)
            .count()
    }

    #[test]
    fn only_the_track_and_rows_change() {
        let p = pattern();
        let v = vary(&p, 0, 4..12, false, 1.0, &mut Rng::new(7));
        assert_eq!(v.tracks[1].cells, p.tracks[1].cells);
        assert_eq!(v.tracks[0].cells[..4], p.tracks[0].cells[..4]);
        assert_eq!(v.tracks[0].cells[12..], p.tracks[0].cells[12..]);
        assert!(changed(&p, &v, 0) > 0);
    }

    #[test]
    fn same_seed_same_variation() {
        let p = pattern();
        let a = vary(&p, 0, 0..16, false, 0.75, &mut Rng::new(42));
        let b = vary(&p, 0, 0..16, false, 0.75, &mut Rng::new(42));
        assert_eq!(a.tracks[0].cells, b.tracks[0].cells);
    }

    #[test]
    fn wilder_changes_more_on_average() {
        let p = pattern();
        let total = |s: f32| {
            (0..50)
                .map(|seed| {
                    changed(
                        &p,
                        &vary(&p, 0, 0..16, false, s, &mut Rng::new(seed + 1)),
                        0,
                    )
                })
                .sum::<usize>()
        };
        assert!(total(0.25) < total(1.0));
    }

    #[test]
    fn synth_notes_stay_in_range_and_slices_stay_in_the_bar() {
        let p = pattern();
        for seed in 1..50 {
            let v = vary(&p, 1, 0..16, true, 1.0, &mut Rng::new(seed));
            for c in v.tracks[1].cells.iter().filter_map(|c| c.trigger) {
                assert!((LOWEST_NOTE as u16..=HIGHEST_NOTE as u16).contains(&c.slice));
            }
            let v = vary(&p, 0, 0..16, false, 1.0, &mut Rng::new(seed));
            for c in v.tracks[0].cells.iter().filter_map(|c| c.trigger) {
                assert!(c.slice < 8, "only slices the bar already used");
            }
        }
    }
}
