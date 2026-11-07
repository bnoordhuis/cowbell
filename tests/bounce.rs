use cowbell::bounce;
use cowbell::demo;
use cowbell::song::{Cell, Fx, Instrument, Song, TICKS_PER_BEAT};

/// The demo song's drums: without its effects rack and at full volume
/// everywhere, so the bounced track (which goes straight into the mixer)
/// and the originals sound the same. The synths are muted, as you'd bounce
/// a break, but stay in the song: a bounce makes the seventh track, which
/// needs a second mixer to be heard.
fn plain_song() -> Song {
    let mut song = demo::song(demo::kit());
    song.rack = cowbell::rack::Rack::simple(song.tracks.len());
    for t in &mut song.tracks[2..] {
        t.mute = true;
    }
    // Full velocity, so the bounce track's 7F triggers play at the same gain.
    for t in &mut song.tracks {
        for c in &mut t.clips {
            for cell in &mut c.cells {
                for tr in cell.notes.iter_mut().flatten() {
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

/// The first break clip that has a hit with this effect.
fn clip_with(song: &Song, fx: fn(&Fx) -> bool) -> usize {
    song.tracks[0]
        .clips
        .iter()
        .position(|c| c.cells.iter().any(|cell| cell.fx().any(|f| fx(&f))))
        .expect("the demo has one")
}

#[test]
fn bounced_clip_plays_back_identically() {
    let mut song = plain_song();
    // A break clip with retriggers, reverse, stutter and crush, with a snare
    // roll under it.
    let i = clip_with(&song, |f| matches!(f, Fx::Crush(_)));
    let clip = song.tracks[0].clips[i].clone();
    for (what, fx) in [
        (
            "retrigger",
            (|f: &Fx| matches!(f, Fx::Retrig(_))) as fn(&Fx) -> bool,
        ),
        ("reverse", |f| *f == Fx::Reverse),
        ("stutter", |f| matches!(f, Fx::Stutter(_))),
    ] {
        assert!(
            clip.cells.iter().any(|c| c.fx().any(|f| fx(&f))),
            "no {what}"
        );
    }
    let span = clip.span();
    assert!(
        song.tracks[1]
            .clips
            .iter()
            .any(|c| c.start < span.end && span.start < c.end()),
        "a roll under it"
    );
    let tracks = song.tracks.len();
    let rows = clip.cells.len();
    let before = bounce::bounce(&song, clip.start, rows, clip.rows_per_beat, "before");
    let t = bounce::bounce_to_track(&mut song, clip.start, rows, clip.rows_per_beat).unwrap();

    assert_eq!(t, tracks, "a new track");
    assert_eq!(
        song.rack.track_device(t).map(|d| d.kind),
        Some(cowbell::rack::Kind::Track(t))
    );
    let mut left = Vec::new();
    for track in &song.tracks[..t] {
        for c in &track.clips {
            for r in 0..c.cells.len() {
                if span.contains(&c.row_tick(r)) && c.cells[r] != Cell::default() {
                    left.push(format!("{} row {r}", track.name));
                }
            }
        }
    }
    assert!(left.is_empty(), "originals cleared, but not {left:?}");
    let new = &song.tracks[t].clips[0];
    assert_eq!((new.start, new.cells.len()), (clip.start, rows));
    let triggers = new.cells.iter().filter(|c| c.has_hit()).count();
    assert!(triggers > 3, "{triggers} slices");
    assert_eq!(
        frames(&song, song.instruments.len() - 1),
        *before.sample.frames
    );

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
    // The first clip of the groove, which comes back later unchanged.
    let i = clip_with(&song, |f| matches!(f, Fx::Cut(_)));
    let groove = song.tracks[0].clips[i].clone();
    let again = (i + 1..song.tracks[0].clips.len())
        .find(|&j| song.tracks[0].clips[j].cells == groove.cells)
        .expect("the groove comes back");
    let tracks = song.tracks.len();
    // Rows 8..16 of it: beats 2 to 4.
    let start = groove.row_tick(8);
    bounce::bounce_to_track(&mut song, start, 8, groove.rows_per_beat).unwrap();
    let cells = &song.tracks[0].clips[i].cells;
    assert!(cells[8..16].iter().all(|c| *c == Cell::default()));
    assert_eq!(cells[..8], groove.cells[..8]);
    assert_eq!(cells[16..], groove.cells[16..]);
    let new = &song.tracks[tracks].clips[0];
    assert_eq!(new.start, groove.start + 2 * TICKS_PER_BEAT);
    assert!(new.cells[0].has_hit(), "slice 0 on the first row");
    // The same clip later in the song is untouched.
    assert_eq!(song.tracks[0].clips[again].cells, groove.cells);
}
