//! Steps selected in the clip editor, and copying, clearing and pasting
//! them. Plain functions on a clip's cells; the clip editor draws and
//! drives them.
//!
//! A selection is some rows of a clip and either a range of lanes (notes or
//! slices) or every lane. Only a selection of every lane takes the rows'
//! effects along: effects belong to a step, not to one of its notes.

use std::ops::{Range, RangeInclusive};

use crate::song::Cell;

#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub rows: Range<usize>,
    /// The notes or slices, lowest to highest; `None` for every lane.
    pub values: Option<RangeInclusive<u16>>,
}

impl Selection {
    /// Whether the note or slice `value` in `row` is in it.
    pub fn contains(&self, row: usize, value: u16) -> bool {
        self.rows.contains(&row) && self.values.as_ref().is_none_or(|v| v.contains(&value))
    }
}

/// Copied steps: what [`copy`] takes and [`paste`] puts back.
#[derive(Clone, Debug, PartialEq)]
pub struct Steps {
    /// Notes (from a synth clip) or slices (from a sample clip).
    pub synth: bool,
    /// As in [`Selection::values`].
    pub values: Option<RangeInclusive<u16>>,
    /// One per row, from the selection's first.
    pub cells: Vec<Cell>,
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
    let cells = cells[rows]
        .iter()
        .map(|c| match &sel.values {
            None => *c,
            Some(v) => only(c, v),
        })
        .collect();
    Steps {
        synth,
        values: sel.values.clone(),
        cells,
    }
}

/// `cells` with the selected notes (and, for every lane, effects) removed.
pub fn clear(cells: &[Cell], sel: &Selection) -> Vec<Cell> {
    let mut out = cells.to_vec();
    let rows = sel.rows.start.min(out.len())..sel.rows.end.min(out.len());
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
    out
}

/// `cells` with `steps` put in from `row` on, replacing what was in their
/// lanes there. Rows past the clip's end are left out. Returns the cells
/// and the selection they now fill.
pub fn paste(cells: &[Cell], steps: &Steps, row: usize) -> (Vec<Cell>, Selection) {
    let end = (row + steps.cells.len()).min(cells.len());
    let sel = Selection {
        rows: row.min(end)..end,
        values: steps.values.clone(),
    };
    let mut out = clear(cells, &sel);
    for (r, cell) in (row..end).zip(&steps.cells) {
        match steps.values {
            None => out[r] = *cell,
            Some(_) => {
                for note in cell.hits() {
                    out[r].add(note);
                }
            }
        }
    }
    (out, sel)
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
            },
            true,
        );
        let (out, _) = paste(&cells(), &whole, 2);
        assert_eq!(out[2], cells()[0]);
    }
}
