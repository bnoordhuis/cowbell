use cowbell::bounce::{self, RATE};
use cowbell::song::{Cell, Instrument, Song, TICKS_PER_BEAT};
use cowbell::{demo, synth};

/// The demo song, without its effects rack, so the bounced track (which goes
/// straight into the mixer) and the original sound the same.
fn plain_song() -> Song {
    let mut song = demo::song(synth::demo_break(RATE, demo::BPM as f32));
    song.rack = cowbell::rack::Rack::simple(song.tracks.len());
    // Full velocity, so the bounce track's 7F triggers play at the same gain.
    for t in &mut song.tracks {
        for c in &mut t.clips {
            for cell in &mut c.cells {
                if let Some(tr) = &mut cell.trigger {
                    tr.velocity = 0x7f;
                }
            }
        }
    }
    song
}

fn frames(song: &Song, i: usize) -> Vec<[f32; 2]> {
    let Instrument::Sampler(s) = &song.instruments[i] else {
        panic!("not a sample")
    };
    s.frames.to_vec()
}

#[test]
fn bounced_clip_plays_back_identically() {
    let mut song = plain_song();
    // The second break clip has retriggers, reverse, stutter, crush, and the
    // snare roll under it.
    let clip = song.tracks[0].clips[1].clone();
    let rows = clip.cells.len();
    let before = bounce::bounce(&song, clip.start, rows, clip.rows_per_beat, "before");
    let t = bounce::bounce_to_track(&mut song, clip.start, rows, clip.rows_per_beat).unwrap();

    assert_eq!(t, 2, "a new track");
    assert_eq!(
        song.rack.track_device(2).map(|d| d.kind),
        Some(cowbell::rack::Kind::Track(2))
    );
    assert!(
        song.tracks[..2].iter().all(|t| t
            .clip_at(clip.start)
            .is_some_and(|c| c.cells.iter().all(|c| *c == Cell::default()))),
        "originals cleared"
    );
    let new = &song.tracks[2].clips[0];
    assert_eq!((new.start, new.cells.len()), (clip.start, rows));
    let triggers = new.cells.iter().filter(|c| c.trigger.is_some()).count();
    assert!(triggers > 3, "{triggers} slices");
    assert_eq!(frames(&song, 1), *before.sample.frames);

    // Playing it now gives the same audio as before the bounce.
    let after = bounce::bounce(&song, clip.start, rows, clip.rows_per_beat, "after");
    let worst = before
        .sample
        .frames
        .iter()
        .zip(after.sample.frames.iter())
        .map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs()))
        .fold(0.0, f32::max);
    assert!(worst < 1e-6, "differs by up to {worst}");
}

#[test]
fn bounce_clears_only_its_stretch() {
    let mut song = plain_song();
    let first = song.tracks[0].clips[0].clone();
    // Rows 8..16 of the first clip: beats 2 to 4.
    let start = first.row_tick(8);
    bounce::bounce_to_track(&mut song, start, 8, first.rows_per_beat).unwrap();
    let cells = &song.tracks[0].clips[0].cells;
    assert!(cells[8..16].iter().all(|c| *c == Cell::default()));
    assert_eq!(cells[..8], first.cells[..8]);
    assert_eq!(cells[16..], first.cells[16..]);
    let new = &song.tracks[2].clips[0];
    assert_eq!(new.start, 2 * TICKS_PER_BEAT);
    assert!(new.cells[0].trigger.is_some(), "slice 0 on the first row");
    // The same clip later in the song is untouched.
    assert_eq!(song.tracks[0].clips[2].cells, first.cells);
}
