//! Steps selected in the clip editor, and copying, clearing and pasting
//! them. Plain functions on a clip's cells; the clip editor draws and
//! drives them.
//!
//! A selection is some rows of a clip and either a range of lanes (notes or
//! slices) or every lane, and on synth clips maybe the sustain pedal too.
//! Only a selection of every lane takes the rows' effects along: effects
//! belong to a step, not to one of its notes. The pedal goes along as
//! whether it's down, row by row, so a stretch of it pastes the same
//! wherever it lands.

use std::ops::{Range, RangeInclusive};

use crate::song::{Cell, Fx, pedal_rows, with_pedal_rows};

/// A selection's values when it's just the pedal: no notes at all.
pub const NO_VALUES: RangeInclusive<u16> = RangeInclusive::new(1, 0);

#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub rows: Range<usize>,
    /// The notes or slices, lowest to highest; `None` for every lane, and
    /// [`NO_VALUES`] for none of them (just the pedal).
    pub values: Option<RangeInclusive<u16>>,
    /// The pedal, along with `values`. Every lane takes it anyway.
    pub pedal: bool,
}

impl Selection {
    /// Whether the note or slice `value` in `row` is in it.
    pub fn contains(&self, row: usize, value: u16) -> bool {
        self.rows.contains(&row) && self.values.as_ref().is_none_or(|v| v.contains(&value))
    }

    /// Whether it takes the pedal.
    pub fn has_pedal(&self) -> bool {
        self.pedal || self.values.is_none()
    }
}

/// Copied steps: what [`copy`] takes and [`paste`] puts back.
#[derive(Clone, Debug, PartialEq)]
pub struct Steps {
    /// Notes (from a synth clip) or slices (from a sample clip).
    pub synth: bool,
    /// As in [`Selection::values`].
    pub values: Option<RangeInclusive<u16>>,
    /// As in [`Selection::pedal`].
    pub pedal: bool,
    /// One per row, from the selection's first. With the pedal, it's down
    /// where their pedal effects say, starting from up.
    pub cells: Vec<Cell>,
}

impl Steps {
    fn selection(&self, rows: Range<usize>) -> Selection {
        Selection {
            rows,
            values: self.values.clone(),
            pedal: self.pedal,
        }
    }
}

/// `cell` without its pedal.
fn without_pedal(mut cell: Cell) -> Cell {
    for f in &mut cell.fx {
        if matches!(f, Some(Fx::Pedal(_))) {
            *f = None;
        }
    }
    cell
}

/// A cell's notes in `values` only, without its effects.
fn only(cell: &Cell, values: &RangeInclusive<u16>) -> Cell {
    let mut kept = Cell::default();
    for note in cell.hits().filter(|n| values.contains(&n.slice)) {
        kept.add(note);
    }
    kept
}

/// The selected steps of `cells`.
pub fn copy(cells: &[Cell], sel: &Selection, synth: bool) -> Steps {
    let rows = sel.rows.start.min(cells.len())..sel.rows.end.min(cells.len());
    let mut copied: Vec<Cell> = cells[rows.clone()]
        .iter()
        .map(|c| match &sel.values {
            None => without_pedal(*c),
            Some(v) => only(c, v),
        })
        .collect();
    if sel.has_pedal() {
        let (down, change) = pedal_rows(cells);
        copied = with_pedal_rows(&copied, &down[rows.clone()], &change[rows]);
    }
    Steps {
        synth,
        values: sel.values.clone(),
        pedal: sel.pedal,
        cells: copied,
    }
}

/// `cells` with the selected notes (and, for every lane, effects) removed,
/// and the pedal up there if it's selected.
pub fn clear(cells: &[Cell], sel: &Selection) -> Vec<Cell> {
    let mut out = cells.to_vec();
    let rows = sel.rows.start.min(out.len())..sel.rows.end.min(out.len());
    let pedal = sel.has_pedal().then(|| {
        let (mut down, mut change) = pedal_rows(cells);
        down[rows.clone()].fill(false);
        change[rows.clone()].fill(false);
        (down, change)
    });
    for row in rows {
        match &sel.values {
            None => out[row] = Cell::default(),
            Some(v) => {
                let cell = &mut out[row];
                let gone: Vec<u16> = cell
                    .hits()
                    .map(|n| n.slice)
                    .filter(|s| v.contains(s))
                    .collect();
                for slice in gone {
                    cell.remove(slice);
                }
            }
        }
    }
    match pedal {
        Some((down, change)) => with_pedal_rows(&out, &down, &change),
        None => out,
    }
}

/// `cells` with `steps` put in from `row` on, replacing what was in their
/// lanes there. Rows past the clip's end are left out. Returns the cells
/// and the selection they now fill.
pub fn paste(cells: &[Cell], steps: &Steps, row: usize) -> (Vec<Cell>, Selection) {
    let end = (row + steps.cells.len()).min(cells.len());
    let sel = steps.selection(row.min(end)..end);
    let mut out = clear(cells, &sel);
    let (mut down, mut change) = pedal_rows(&out);
    for (r, cell) in (row..end).zip(&steps.cells) {
        match steps.values {
            None => out[r] = without_pedal(*cell),
            Some(_) => {
                for note in cell.hits() {
                    out[r].add(note);
                }
            }
        }
    }
    if sel.has_pedal() {
        let (from_down, from_change) = pedal_rows(&steps.cells);
        down[sel.rows.clone()].copy_from_slice(&from_down[..sel.rows.len()]);
        change[sel.rows.clone()].copy_from_slice(&from_change[..sel.rows.len()]);
    }
    (with_pedal_rows(&out, &down, &change), sel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song::{Fx, Trigger};

    fn notes(c: &Cell) -> Vec<u16> {
        let mut v: Vec<u16> = c.hits().map(|n| n.slice).collect();
        v.sort_unstable();
        v
    }

    /// Four rows: a chord with a cut, a note, nothing, a note.
    fn cells() -> Vec<Cell> {
        vec![
            Cell::hit(60, 100)
                .with_note(Trigger::new(64, 90))
                .with_fx(Fx::Cut(8)),
            Cell::hit(67, 80),
            Cell::default(),
            Cell::hit(48, 70),
        ]
    }

    #[test]
    fn a_range_of_lanes_takes_only_its_notes_and_no_effects() {
        let sel = Selection {
            rows: 0..2,
            values: Some(62..=70),
            pedal: false,
        };
        let s = copy(&cells(), &sel, true);
        assert_eq!(s.cells.len(), 2);
        assert_eq!(notes(&s.cells[0]), [64]);
        assert_eq!(s.cells[0].fx().count(), 0, "effects stay behind");
        assert_eq!(notes(&s.cells[1]), [67]);
        assert_eq!(
            s.cells[0].hits().next().unwrap().velocity,
            90,
            "velocity kept"
        );

        let left = clear(&cells(), &sel);
        assert_eq!(notes(&left[0]), [60]);
        assert_eq!(left[0].fx().count(), 1);
        assert!(!left[1].has_hit());
        assert_eq!(notes(&left[3]), [48], "outside the rows");
    }

    #[test]
    fn every_lane_takes_whole_steps() {
        let sel = Selection {
            rows: 0..1,
            values: None,
            pedal: false,
        };
        let s = copy(&cells(), &sel, true);
        assert_eq!(s.cells[0], cells()[0]);
        assert_eq!(clear(&cells(), &sel)[0], Cell::default());
        assert!(sel.contains(0, 1) && !sel.contains(1, 60));
    }

    #[test]
    fn pasting_replaces_its_lanes_and_stops_at_the_end() {
        let sel = Selection {
            rows: 0..2,
            values: Some(62..=70),
            pedal: false,
        };
        let s = copy(&cells(), &sel, true);
        // Two rows from row 3: only one fits.
        let (out, filled) = paste(&cells(), &s, 3);
        assert_eq!(filled.rows, 3..4);
        assert_eq!(
            notes(&out[3]),
            [48, 64],
            "48 is outside the lanes, so it stays"
        );
        // Over a note in its lanes: that's replaced.
        let (out, _) = paste(&cells(), &s, 1);
        assert_eq!(notes(&out[1]), [64]);
        assert_eq!(notes(&out[2]), [67]);
        // Whole steps replace whole steps.
        let whole = copy(
            &cells(),
            &Selection {
                rows: 0..1,
                values: None,
                pedal: false,
            },
            true,
        );
        let (out, _) = paste(&cells(), &whole, 2);
        assert_eq!(out[2], cells()[0]);
    }

    #[test]
    fn the_pedal_copies_as_where_its_down_and_pastes_the_same() {
        use crate::song::{PEDAL_CHANGE, PEDAL_DOWN, PEDAL_UP};
        // Down from row 1, changed at 3, up at 5.
        let mut c = vec![Cell::default(); 8];
        c[1] = Cell::hit(60, 100).with_fx(Fx::Pedal(PEDAL_DOWN));
        c[3] = Cell::default().with_fx(Fx::Pedal(PEDAL_CHANGE));
        c[5] = Cell::default().with_fx(Fx::Pedal(PEDAL_UP));
        let pedals = |c: &[Cell]| c.iter().map(Cell::pedal).collect::<Vec<_>>();
        let just_pedal = Selection {
            rows: 2..5,
            values: Some(NO_VALUES),
            pedal: true,
        };
        let s = copy(&c, &just_pedal, true);
        assert_eq!(
            pedals(&s.cells),
            [Some(PEDAL_DOWN), Some(PEDAL_CHANGE), None],
            "down from its first row"
        );
        assert!(s.cells.iter().all(|c| !c.has_hit()), "no notes");
        // Cleared: up over those rows, back down after, the note kept.
        let left = clear(&c, &just_pedal);
        assert_eq!(
            pedals(&left),
            [
                None,
                Some(PEDAL_DOWN),
                Some(PEDAL_UP),
                None,
                None,
                None,
                None,
                None
            ]
        );
        assert!(left[1].has_hit());
        // Pasted onto a clip with no pedal: down for those three rows.
        let (out, filled) = paste(&vec![Cell::default(); 8], &s, 4);
        assert_eq!(
            filled,
            Selection {
                rows: 4..7,
                ..just_pedal.clone()
            }
        );
        assert_eq!(
            pedals(&out),
            [
                None,
                None,
                None,
                None,
                Some(PEDAL_DOWN),
                Some(PEDAL_CHANGE),
                None,
                Some(PEDAL_UP)
            ]
        );
        // Notes without the pedal leave it alone.
        let notes_only = Selection {
            rows: 0..8,
            values: Some(0..=127),
            pedal: false,
        };
        assert_eq!(pedals(&clear(&c, &notes_only)), pedals(&c));
        // Whole steps take it too, and clearing them keeps it right after.
        let whole = Selection {
            rows: 0..2,
            values: None,
            pedal: false,
        };
        let left = clear(&c, &whole);
        assert_eq!(left[2].pedal(), Some(PEDAL_DOWN), "still down from row 2");
    }
}
