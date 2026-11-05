use cowbell::bounce::{self, RATE};
use cowbell::editor::{Action, Editor};
use cowbell::pattern::{Cell, Fx, Instrument};
use cowbell::{demo, synth};

/// The demo song, without its effects rack, so the bounced track (which goes
/// straight into the mixer) and the original sound the same.
fn plain_song() -> cowbell::pattern::Song {
    let mut song = demo::song(synth::demo_break(RATE, demo::BPM as f32));
    song.rack = cowbell::rack::Rack::simple(song.patterns[0].tracks.len());
    // Full velocity, so the bounce track's 7F triggers play at the same gain.
    for p in &mut song.patterns {
        for t in &mut p.tracks {
            for c in &mut t.cells {
                if let Some(tr) = &mut c.trigger {
                    tr.velocity = 0x7f;
                }
            }
        }
    }
    song
}

fn frames(song: &cowbell::pattern::Song, i: usize) -> Vec<[f32; 2]> {
    let Instrument::Sampler(s) = &song.instruments[i];
    s.frames.clone()
}

#[test]
fn bounced_rows_play_back_identically() {
    let song = plain_song();
    // Pattern 1 has retriggers, reverse, stutter, crush and a roll.
    let mut e = Editor::new(song.clone());
    e.go_to(1, 0);
    let rows = 0..e.pattern().rows;
    let before = bounce::bounce(&e.song, 1, rows.clone(), "before");
    e.apply(Action::Bounce);

    let tracks = &e.pattern().tracks;
    assert_eq!(tracks.len(), 3, "a new track");
    assert!(
        tracks[..2]
            .iter()
            .all(|t| t.cells.iter().all(|c| *c == Cell::default())),
        "originals cleared"
    );
    let triggers = tracks[2]
        .cells
        .iter()
        .filter(|c| c.trigger.is_some())
        .count();
    assert!(triggers > 3, "{triggers} slices");
    assert_eq!(frames(&e.song, 1), before.sample.frames);

    // Playing the pattern now gives the same audio as before the bounce.
    let after = bounce::bounce(&e.song, 1, rows, "after");
    let worst = before
        .sample
        .frames
        .iter()
        .zip(&after.sample.frames)
        .map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs()))
        .fold(0.0, f32::max);
    assert!(worst < 1e-6, "differs by up to {worst}");
}

#[test]
fn bounce_selection_only() {
    let mut e = Editor::new(plain_song());
    e.go_to(0, 8);
    for _ in 0..7 {
        e.apply(Action::SelectDown);
    }
    let untouched: Vec<Cell> = e.pattern().tracks[0].cells.clone();
    e.apply(Action::Bounce);
    let cells = &e.pattern().tracks[0].cells;
    assert!(cells[8..16].iter().all(|c| *c == Cell::default()));
    assert_eq!(cells[..8], untouched[..8]);
    assert_eq!(cells[16..], untouched[16..]);
    let new = &e.pattern().tracks[2].cells;
    assert!(new[8].trigger.is_some(), "slice 0 on the first bounced row");
    assert!(
        new[..8]
            .iter()
            .chain(&new[16..])
            .all(|c| *c == Cell::default())
    );
}

#[test]
fn undo_removes_the_bounce() {
    let song = plain_song();
    let mut e = Editor::new(song.clone());
    e.apply(Action::Bounce);
    assert_eq!(e.song.instruments.len(), 2);
    assert!(e.song.rack.track_device(2).is_some());
    e.apply(Action::Undo);
    assert_eq!(e.song.instruments.len(), 1);
    assert!(e.song.rack.track_device(2).is_none());
    assert_eq!(e.pattern().tracks.len(), 2);
    assert_eq!(
        e.pattern().tracks[0].cells,
        song.patterns[0].tracks[0].cells
    );
    e.apply(Action::Redo);
    assert_eq!(e.song.instruments.len(), 2);
}

#[test]
fn bounced_slices_can_be_rearranged() {
    let mut e = Editor::new(plain_song());
    e.apply(Action::Bounce);
    // Reverse a slice on the bounce track, like you would after re-chopping.
    let row = e.pattern().tracks[2]
        .cells
        .iter()
        .position(|c| c.trigger.is_some_and(|t| t.slice == 1))
        .unwrap();
    e.go_to(0, row);
    e.cursor.track = 2;
    e.cursor.field = cowbell::editor::Field::Fx(0);
    e.apply(Action::Char('v'));
    assert_eq!(e.pattern().tracks[2].cells[row].fx[0], Some(Fx::Reverse));
}
