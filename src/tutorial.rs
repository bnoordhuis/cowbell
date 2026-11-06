//! The tutorial: lessons that walk through making a track, one step at a
//! time. No UI code; the window draws the current step.
//!
//! A step is done when the song or app is in the state it asks for, not
//! when particular keys were pressed, so any way of getting there counts
//! and you can wander off and come back. Checks compare against the song
//! as it was when the step started (the baseline).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::editor::Editor;
use crate::pattern::{Cell, Fx, Instrument, Pattern, Song};
use crate::rack::{Kind, Rack};
use crate::sample::Sample;
use crate::synthvoice::SynthKind;

/// What the tutorial can see of the app, besides the editor.
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts {
    pub playing: bool,
    /// Playback ran at some point since the step started.
    pub played: bool,
    pub rack_view: bool,
    /// A slice or note was auditioned since the step started.
    pub auditioned: bool,
    pub palette_open: bool,
    /// The song is saved to a file, with no changes since.
    pub saved: bool,
    /// The song was exported since the step started.
    pub exported: bool,
}

pub struct Ctx<'a> {
    pub editor: &'a Editor,
    pub baseline: &'a Song,
    pub facts: Facts,
}

/// Where a step wants you to look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    None,
    Transport,
    Slicer,
    Grid,
    Cell { track: usize, row: usize },
    Rack,
}

impl Focus {
    /// The slicer and the synth panel share a place above the grid.
    pub const INSTRUMENT: Focus = Focus::Slicer;
}

pub struct Step {
    pub text: &'static str,
    pub keys: &'static str,
    pub focus: Focus,
    pub done: fn(&Ctx) -> bool,
}

pub struct Lesson {
    pub title: &'static str,
    pub steps: Vec<Step>,
}

/// The tutorial song's tracks.
pub const BRK: usize = 0;
pub const SNR: usize = 1;

/// A break played straight on one track, and an empty track for snares.
pub fn song() -> Song {
    let bpm = 172.0;
    let brk = crate::synth::demo_break(48_000, bpm as f32);
    let row_frames = 48_000.0 * 60.0 / bpm / 4.0;
    let mut pattern = Pattern::new(4, 8, &[("brk", 0), ("snr", 0)]);
    for (i, &start) in brk.slices.iter().enumerate() {
        let row = (start as f64 / row_frames).round() as usize;
        if let Some(c) = pattern.tracks[BRK].cells.get_mut(row) {
            *c = Cell::hit(i as u16, 0x6e);
        }
    }
    Song {
        bpm,
        instruments: vec![Instrument::Sampler(Arc::new(brk))],
        patterns: vec![pattern],
        order: vec![0],
        rack: Rack::simple(2),
        tuning: Default::default(),
        automation: Vec::new(),
    }
}

fn cells(song: &Song, track: usize) -> &[Cell] {
    song.order
        .first()
        .and_then(|&p| song.patterns.get(p))
        .and_then(|p| p.tracks.get(track))
        .map_or(&[], |t| &t.cells)
}

/// A track's cells in pattern `p`, wherever that is in the order.
fn cells_in(song: &Song, p: usize, track: usize) -> &[Cell] {
    song.patterns
        .get(p)
        .and_then(|p| p.tracks.get(track))
        .map_or(&[], |t| &t.cells)
}

fn sample(song: &Song, track: usize) -> Option<&Sample> {
    let inst = song
        .order
        .first()
        .and_then(|&p| song.patterns.get(p))?
        .tracks
        .get(track)?
        .instrument;
    match song.instruments.get(inst)? {
        Instrument::Sampler(s) => Some(s),
        Instrument::Synth(_) => None,
    }
}

fn slice_count(song: &Song, track: usize) -> usize {
    sample(song, track).map_or(0, |s| s.slices.len())
}

/// How many cells on `track` have an effect matching `f`.
fn count_fx(song: &Song, track: usize, f: impl Fn(Fx) -> bool) -> usize {
    cells(song, track).iter().filter(|c| c.fx().any(&f)).count()
}

fn track_count(song: &Song) -> usize {
    song.order
        .first()
        .and_then(|&p| song.patterns.get(p))
        .map_or(0, |p| p.tracks.len())
}

fn all_tracks_fx(song: &Song, f: impl Fn(Fx) -> bool + Copy) -> usize {
    (0..track_count(song)).map(|t| count_fx(song, t, f)).sum()
}

/// The kind of synth a track plays, if it plays one.
fn synth_kind(song: &Song, track: usize) -> Option<SynthKind> {
    let inst = song
        .order
        .first()
        .and_then(|&p| song.patterns.get(p))?
        .tracks
        .get(track)?
        .instrument;
    match song.instruments.get(inst)? {
        Instrument::Synth(p) => Some(p.kind),
        Instrument::Sampler(_) => None,
    }
}

/// The first track playing a synth of `kind`.
fn synth_track(song: &Song, kind: SynthKind) -> Option<usize> {
    (0..track_count(song)).find(|&t| synth_kind(song, t) == Some(kind))
}

/// Notes (triggers) on the first track playing `kind`, with velocity in
/// `vel`.
fn synth_notes(song: &Song, kind: SynthKind, vel: std::ops::RangeInclusive<u8>) -> usize {
    synth_track(song, kind).map_or(0, |t| {
        cells(song, t)
            .iter()
            .filter(|c| c.trigger.is_some_and(|t| vel.contains(&t.velocity)))
            .count()
    })
}

fn synth_fx(song: &Song, kind: SynthKind, f: impl Fn(Fx) -> bool) -> usize {
    synth_track(song, kind).map_or(0, |t| count_fx(song, t, f))
}

fn devices(song: &Song, kind: Kind) -> usize {
    song.rack.devices.iter().filter(|d| d.kind == kind).count()
}

/// Is there a cable from output `out` of a device matching `from` into
/// input `inp` of one matching `to`? `None` for `inp` means any input.
fn cable(
    song: &Song,
    from: impl Fn(Kind) -> bool,
    out: usize,
    to: impl Fn(Kind) -> bool,
    inp: Option<usize>,
) -> bool {
    let kind = |id| song.rack.device(id).map(|d| d.kind);
    song.rack.cables.iter().any(|c| {
        kind(c.from.device).is_some_and(&from)
            && c.from.port == out
            && kind(c.to.device).is_some_and(&to)
            && inp.is_none_or(|p| c.to.port == p)
    })
}

/// A device of `kind` whose knobs differ from the baseline's.
fn knob_turned(now: &Song, before: &Song, kind: Kind) -> bool {
    now.rack.devices.iter().filter(|d| d.kind == kind).any(|d| {
        before
            .rack
            .device(d.id)
            .is_some_and(|b| b.params != d.params)
    })
}

/// Tracks with an automation target.
fn bound_tracks(song: &Song) -> impl Iterator<Item = usize> + '_ {
    (0..track_count(song)).filter(|&t| song.automation(t).is_some())
}

fn bound_fx(song: &Song, f: impl Fn(Fx) -> bool + Copy) -> usize {
    bound_tracks(song).map(|t| count_fx(song, t, f)).sum()
}

fn knobs(f: Fx) -> bool {
    matches!(f, Fx::Knob(_))
}

/// An FM synth whose detune knob differs from the baseline's.
fn detuned(now: &Song, before: &Song) -> bool {
    const DETUNE: usize = 8;
    now.instruments
        .iter()
        .zip(&before.instruments)
        .any(|(a, b)| match (a, b) {
            (Instrument::Synth(a), Instrument::Synth(b)) => {
                a.kind == SynthKind::Fm && a.params.get(DETUNE) != b.params.get(DETUNE)
            }
            _ => false,
        })
}

fn snare_at(song: &Song, row: usize) -> Option<crate::pattern::Trigger> {
    cells(song, SNR)
        .get(row)
        .and_then(|c| c.trigger)
        .filter(|t| t.slice == 2)
}

pub fn lessons() -> Vec<Lesson> {
    vec![
        Lesson {
            title: "Listen and look around",
            steps: vec![
                Step {
                    text: "This is the tracker. Time runs down the screen, one row per 16th note. Press space to play.",
                    keys: "space",
                    focus: Focus::Grid,
                    done: |c| c.facts.playing,
                },
                Step {
                    text: "The orange mark on the left is the playhead. Press space again to stop.",
                    keys: "space",
                    focus: Focus::Grid,
                    done: |c| !c.facts.playing,
                },
                Step {
                    text: "Move the cursor with the arrow keys. Go down to row 08 of the brk track.",
                    keys: "↓",
                    focus: Focus::Cell { track: BRK, row: 8 },
                    done: |c| c.editor.cursor.row == 8 && c.editor.cursor.track == BRK,
                },
                Step {
                    text: "Tab jumps to the next track. Go to the snr track: it's empty, you'll fill it soon.",
                    keys: "tab",
                    focus: Focus::Grid,
                    done: |c| c.editor.cursor.track == SNR,
                },
                Step {
                    text: "F2 shows the rack, where each track runs through effects you patch with cables.",
                    keys: "F2",
                    focus: Focus::Transport,
                    done: |c| c.facts.rack_view,
                },
                Step {
                    text: "F1 brings you back to the tracker.",
                    keys: "F1",
                    focus: Focus::Rack,
                    done: |c| !c.facts.rack_view,
                },
            ],
        },
        Lesson {
            title: "Chop the break",
            steps: vec![
                Step {
                    text: "The slicer shows the break, cut into numbered slices at each hit. Click a slice to hear it.",
                    keys: "click",
                    focus: Focus::Slicer,
                    done: |c| c.facts.auditioned,
                },
                Step {
                    text: "[ makes the slicer less sensitive: quiet hits like the hats join the slice before them. Press it until the slice count (above the slicer) drops.",
                    keys: "[",
                    focus: Focus::Slicer,
                    done: |c| {
                        slice_count(&c.editor.song, c.editor.cursor.track)
                            < slice_count(c.baseline, c.editor.cursor.track)
                    },
                },
                Step {
                    text: "] makes it more sensitive again. Press it until the hats have slices of their own.",
                    keys: "]",
                    focus: Focus::Slicer,
                    done: |c| {
                        slice_count(&c.editor.song, c.editor.cursor.track)
                            > slice_count(c.baseline, c.editor.cursor.track)
                    },
                },
                Step {
                    text: "You can place markers by hand too. Shift+click inside a slice to split it there.",
                    keys: "shift+click",
                    focus: Focus::Slicer,
                    done: |c| {
                        slice_count(&c.editor.song, c.editor.cursor.track)
                            == slice_count(c.baseline, c.editor.cursor.track) + 1
                    },
                },
                Step {
                    text: "Drag a marker sideways to move it.",
                    keys: "drag",
                    focus: Focus::Slicer,
                    done: |c| {
                        let t = c.editor.cursor.track;
                        let (now, before) = (sample(&c.editor.song, t), sample(c.baseline, t));
                        matches!((now, before), (Some(a), Some(b)) if a.slices.len() == b.slices.len() && a.slices != b.slices)
                    },
                },
                Step {
                    text: "Right-click a marker to remove it. (Everything here can be undone with ctrl+z.)",
                    keys: "right-click",
                    focus: Focus::Slicer,
                    done: |c| {
                        slice_count(&c.editor.song, c.editor.cursor.track) + 1
                            == slice_count(c.baseline, c.editor.cursor.track)
                    },
                },
            ],
        },
        Lesson {
            title: "Program a beat",
            steps: vec![
                Step {
                    text: "A cell holds a slice number, a velocity and two effects. Go to row 04 of the snr track.",
                    keys: "arrows, tab",
                    focus: Focus::Cell { track: SNR, row: 4 },
                    done: |c| c.editor.cursor.track == SNR && c.editor.cursor.row == 4,
                },
                Step {
                    text: "Type 0 then 2. Slice 02 is the snare; the cursor steps down once the number is in.",
                    keys: "0 2",
                    focus: Focus::Cell { track: SNR, row: 4 },
                    done: |c| snare_at(&c.editor.song, 4).is_some(),
                },
                Step {
                    text: "Put another snare (02) on row 0C. Rows are numbered in hex: 0C is twelve.",
                    keys: "0 2",
                    focus: Focus::Cell {
                        track: SNR,
                        row: 12,
                    },
                    done: |c| snare_at(&c.editor.song, 12).is_some(),
                },
                Step {
                    text: "Make that one softer: on row 0C, move right to the velocity column and type 50.",
                    keys: "→ → 5 0",
                    focus: Focus::Cell {
                        track: SNR,
                        row: 12,
                    },
                    done: |c| snare_at(&c.editor.song, 12).is_some_and(|t| t.velocity < 0x7f),
                },
                Step {
                    text: "ctrl+↑ sets the edit step: how far the cursor jumps after you type. Set it to 2.",
                    keys: "ctrl+↑",
                    focus: Focus::Transport,
                    done: |c| c.editor.edit_step == 2,
                },
                Step {
                    text: "Listen to your beat: space to play, space to stop.",
                    keys: "space",
                    focus: Focus::Grid,
                    done: |c| c.facts.played && !c.facts.playing,
                },
            ],
        },
        Lesson {
            title: "Mangle it",
            steps: vec![
                Step {
                    text: "Effects go in the two right-hand columns: a letter, then two hex digits. On a brk hit, move right to an effect column and type v: play it backwards.",
                    keys: "→ v",
                    focus: Focus::Grid,
                    done: |c| {
                        all_tracks_fx(&c.editor.song, |f| f == Fx::Reverse)
                            > all_tracks_fx(c.baseline, |f| f == Fx::Reverse)
                    },
                },
                Step {
                    text: "R retriggers: r03 plays a hit three times within its row. Add one to a hit.",
                    keys: "r 0 3",
                    focus: Focus::Grid,
                    done: |c| {
                        let retrig = |f| matches!(f, Fx::Retrig(n) if n > 1);
                        all_tracks_fx(&c.editor.song, retrig) > all_tracks_fx(c.baseline, retrig)
                    },
                },
                Step {
                    text: "P shifts pitch by semitones: p0c is an octave up, pf4 an octave down. Add one.",
                    keys: "p 0 c",
                    focus: Focus::Grid,
                    done: |c| {
                        let pitch = |f| matches!(f, Fx::Pitch(n) if n != 0);
                        all_tracks_fx(&c.editor.song, pitch) > all_tracks_fx(c.baseline, pitch)
                    },
                },
                Step {
                    text: "A snare roll: on the snr track, go to row 18, hold shift and press ↓ to select down to row 1F, then ctrl+r.",
                    keys: "shift+↓, ctrl+r",
                    focus: Focus::Cell {
                        track: SNR,
                        row: 0x18,
                    },
                    done: |c| {
                        let retrig = |f| matches!(f, Fx::Retrig(_));
                        count_fx(&c.editor.song, SNR, retrig)
                            >= count_fx(c.baseline, SNR, retrig) + 4
                    },
                },
                Step {
                    text: "There's much more. Press ? to search every command; try typing \"chord\" or \"variations\".",
                    keys: "?",
                    focus: Focus::None,
                    done: |c| c.facts.palette_open,
                },
                Step {
                    text: "Close the search with esc, then play what you made.",
                    keys: "esc, space",
                    focus: Focus::Grid,
                    done: |c| c.facts.played && !c.facts.palette_open,
                },
            ],
        },
        Lesson {
            title: "Patch the rack",
            steps: vec![
                Step {
                    text: "Each track has a device in the rack, cabled into the mixer and on to the master. Press F2 to see it.",
                    keys: "F2",
                    focus: Focus::Transport,
                    done: |c| c.facts.rack_view,
                },
                Step {
                    text: "Click + crusher at the top to add a bitcrusher.",
                    keys: "click + crusher",
                    focus: Focus::Rack,
                    done: |c| {
                        devices(&c.editor.song, Kind::Crusher) > devices(c.baseline, Kind::Crusher)
                    },
                },
                Step {
                    text: "Drag the plug of brk's cable out of the mixer and drop it on the crusher's in jack.",
                    keys: "drag",
                    focus: Focus::Rack,
                    done: |c| {
                        cable(
                            &c.editor.song,
                            |k| k == Kind::Track(BRK),
                            0,
                            |k| k == Kind::Crusher,
                            Some(0),
                        )
                    },
                },
                Step {
                    text: "The crusher isn't going anywhere yet. Drag from its out jack to a free mixer input.",
                    keys: "drag",
                    focus: Focus::Rack,
                    done: |c| {
                        cable(
                            &c.editor.song,
                            |k| k == Kind::Crusher,
                            0,
                            |k| {
                                matches!(k, Kind::Mixer | Kind::Master | Kind::Filter | Kind::Delay)
                            },
                            None,
                        )
                    },
                },
                Step {
                    text: "Drag the crusher's bits knob up or down (shift for fine steps). Fewer bits, more grit.",
                    keys: "drag a knob",
                    focus: Focus::Rack,
                    done: |c| knob_turned(&c.editor.song, c.baseline, Kind::Crusher),
                },
                Step {
                    text: "Now the snare: add a filter, move snr's plug into the filter's in, and cable the filter's out into the mixer.",
                    keys: "click + filter, drag",
                    focus: Focus::Rack,
                    done: |c| {
                        let s = &c.editor.song;
                        cable(
                            s,
                            |k| k == Kind::Track(SNR),
                            0,
                            |k| k == Kind::Filter,
                            Some(0),
                        ) && cable(s, |k| k == Kind::Filter, 0, |_| true, None)
                    },
                },
                Step {
                    text: "Add an LFO and set its shape to s&h. Then cable brk's gate (it pulses on every hit) into the LFO's clock.",
                    keys: "click + lfo, drag",
                    focus: Focus::Rack,
                    done: |c| {
                        cable(
                            &c.editor.song,
                            |k| k == Kind::Track(BRK),
                            1,
                            |k| k == Kind::Lfo,
                            Some(0),
                        )
                    },
                },
                Step {
                    text: "Cable the LFO's out into the filter's cutoff jack: the break now picks a new snare tone on every hit.",
                    keys: "drag",
                    focus: Focus::Rack,
                    done: |c| {
                        cable(
                            &c.editor.song,
                            |k| k == Kind::Lfo,
                            0,
                            |k| k == Kind::Filter,
                            Some(1),
                        )
                    },
                },
                Step {
                    text: "Play it and listen to the crushed break and the wobbling snares. Space to play, space to stop.",
                    keys: "space",
                    focus: Focus::None,
                    done: |c| c.facts.played && !c.facts.playing,
                },
            ],
        },
        Lesson {
            title: "Acid bass",
            steps: vec![
                Step {
                    text: "Back to the tracker with F1. ctrl+t adds a track for the bass.",
                    keys: "F1, ctrl+t",
                    focus: Focus::Grid,
                    done: |c| {
                        !c.facts.rack_view && track_count(&c.editor.song) > track_count(c.baseline)
                    },
                },
                Step {
                    text: "Tab over to the new track and press F5: it now plays an acid synth. Its knobs replace the slicer.",
                    keys: "tab, F5",
                    focus: Focus::Grid,
                    done: |c| synth_track(&c.editor.song, SynthKind::Acid).is_some(),
                },
                Step {
                    text: "Synth tracks take notes. The bottom letter row is a piano: z is C, s C#, x D… q-u is the octave above, [ ] shift octaves. Type four notes or more.",
                    keys: "z x c v b n m, [ ]",
                    focus: Focus::Grid,
                    done: |c| {
                        synth_notes(&c.editor.song, SynthKind::Acid, 0..=127)
                            >= synth_notes(c.baseline, SynthKind::Acid, 0..=127) + 4
                    },
                },
                Step {
                    text: "Notes louder than 64 are accented: punchier, with more filter. Give some notes a velocity of 40 so the others stand out.",
                    keys: "→ 4 0",
                    focus: Focus::Grid,
                    done: |c| {
                        synth_notes(&c.editor.song, SynthKind::Acid, 0..=0x64)
                            > synth_notes(c.baseline, SynthKind::Acid, 0..=0x64)
                    },
                },
                Step {
                    text: "Slides: put g10 in an effect column on a note, and it glides there from the note before.",
                    keys: "g 1 0",
                    focus: Focus::Grid,
                    done: |c| {
                        let glide = |f| matches!(f, Fx::Glide(_));
                        synth_fx(&c.editor.song, SynthKind::Acid, glide)
                            > synth_fx(c.baseline, SynthKind::Acid, glide)
                    },
                },
                Step {
                    text: "A note rings until the next one. Type . on an empty row to stop it there (a note off, shown as ===).",
                    keys: ".",
                    focus: Focus::Grid,
                    done: |c| {
                        let off = |f| f == Fx::Cut(0);
                        synth_fx(&c.editor.song, SynthKind::Acid, off)
                            > synth_fx(c.baseline, SynthKind::Acid, off)
                    },
                },
                Step {
                    text: "Turn the cutoff and reso knobs above the grid, and play. Tweak while it plays.",
                    keys: "drag knobs, space",
                    focus: Focus::INSTRUMENT,
                    done: |c| c.facts.played && !c.facts.playing,
                },
            ],
        },
        Lesson {
            title: "FM bells",
            steps: vec![
                Step {
                    text: "One more track for bells: ctrl+t.",
                    keys: "ctrl+t",
                    focus: Focus::Grid,
                    done: |c| track_count(&c.editor.song) > track_count(c.baseline),
                },
                Step {
                    text: "Tab to it and press F6 for an FM synth: bright, glassy tones.",
                    keys: "tab, F6",
                    focus: Focus::Grid,
                    done: |c| synth_track(&c.editor.song, SynthKind::Fm).is_some(),
                },
                Step {
                    text: "Chords: type a note, then h47 in its effect column. The note plays with notes 4 and 7 semitones above: a major chord (h37 is minor).",
                    keys: "q, ↑, → → → h 4 7",
                    focus: Focus::Grid,
                    done: |c| {
                        let chords = |s: &Song| {
                            synth_track(s, SynthKind::Fm).map_or(0, |t| {
                                cells(s, t)
                                    .iter()
                                    .filter(|c| {
                                        c.trigger.is_some()
                                            && c.fx().any(|f| matches!(f, Fx::Chord(_)))
                                    })
                                    .count()
                            })
                        };
                        chords(&c.editor.song) > chords(c.baseline)
                    },
                },
                Step {
                    text: "Drag the detune knob: a few cents off makes it shimmer and sound a little broken.",
                    keys: "drag detune",
                    focus: Focus::INSTRUMENT,
                    done: |c| detuned(&c.editor.song, c.baseline),
                },
                Step {
                    text: "Leave twelve-tone tuning behind: ctrl+shift+l, type stretched (or slendro, pelog, just) and enter.",
                    keys: "ctrl+shift+l",
                    focus: Focus::Transport,
                    done: |c| {
                        let t = &c.editor.song.tuning;
                        !t.is_equal_temperament() && *t != c.baseline.tuning
                    },
                },
                Step {
                    text: "Play it. The tuning applies to every synth track; sample tracks stay as they are.",
                    keys: "space",
                    focus: Focus::Grid,
                    done: |c| c.facts.played && !c.facts.playing,
                },
            ],
        },
        Lesson {
            title: "Movement",
            steps: vec![
                Step {
                    text: "Knobs can follow the pattern. Go to the acid track and right-click its cutoff knob: K effects on that track now turn it.",
                    keys: "right-click a knob",
                    focus: Focus::INSTRUMENT,
                    done: |c| {
                        let s = &c.editor.song;
                        (0..track_count(s)).any(|t| {
                            s.automation(t).is_some() && s.automation(t) != c.baseline.automation(t)
                        })
                    },
                },
                Step {
                    text: "On that track, go to row 00, an effect column, and type k00: the knob all the way down.",
                    keys: "→ → → k 0 0",
                    focus: Focus::Grid,
                    done: |c| {
                        let f = |f| f == Fx::Knob(0);
                        bound_fx(&c.editor.song, f) > bound_fx(c.baseline, f)
                    },
                },
                Step {
                    text: "Same column, row 1F: type kff, all the way up.",
                    keys: "k f f",
                    focus: Focus::Grid,
                    done: |c| {
                        let f = |f| f == Fx::Knob(0xff);
                        bound_fx(&c.editor.song, f) > bound_fx(c.baseline, f)
                    },
                },
                Step {
                    text: "Select from row 00 to 1F in that column (shift+↑ or ↓), then ctrl+k fills in the values between: a sweep.",
                    keys: "shift+↑, ctrl+k",
                    focus: Focus::Grid,
                    done: |c| bound_fx(&c.editor.song, knobs) >= bound_fx(c.baseline, knobs) + 4,
                },
                Step {
                    text: "Play the sweep. This works for any knob in the rack too: right-click it while the cursor is on a track.",
                    keys: "space",
                    focus: Focus::Grid,
                    done: |c| c.facts.played && !c.facts.playing,
                },
            ],
        },
        Lesson {
            title: "Variations and structure",
            steps: vec![
                Step {
                    text: "On the brk track, press ctrl+g: four variations of the pattern, mild to wild, go right after it in the order list (top bar).",
                    keys: "ctrl+g",
                    focus: Focus::Transport,
                    done: |c| c.editor.song.order.len() >= c.baseline.order.len() + 4,
                },
                Step {
                    text: "ctrl+→ steps through the slots; Enter plays the one you're on. Pick one you don't like and drop it with ctrl+delete.",
                    keys: "ctrl+→, enter, ctrl+delete",
                    focus: Focus::Transport,
                    done: |c| c.editor.song.order.len() < c.baseline.order.len(),
                },
                Step {
                    text: "ctrl+d copies the current pattern into a new slot, to change without touching the original.",
                    keys: "ctrl+d",
                    focus: Focus::Transport,
                    done: |c| c.editor.song.patterns.len() > c.baseline.patterns.len(),
                },
                Step {
                    text: "Odd meters: alt+shift+← takes a row off the pattern. Make it a few rows short and it lands early, like a skipped beat.",
                    keys: "alt+shift+←",
                    focus: Focus::Transport,
                    done: |c| {
                        let p = c.editor.pattern_index();
                        c.baseline
                            .patterns
                            .get(p)
                            .is_some_and(|b| c.editor.pattern().rows < b.rows)
                    },
                },
                Step {
                    text: "Go back to the first slot (ctrl+←) and play: the song runs through every slot in order.",
                    keys: "ctrl+←, space",
                    focus: Focus::Transport,
                    done: |c| c.facts.played && !c.facts.playing,
                },
            ],
        },
        Lesson {
            title: "Bounce and re-chop",
            steps: vec![
                Step {
                    text: "On the brk track, select rows 00 to 0F and press ctrl+b. Those rows of every track, effects and all, become one new sample on a new track.",
                    keys: "shift+↓, ctrl+b",
                    focus: Focus::Grid,
                    done: |c| {
                        let s = &c.editor.song;
                        s.instruments.len() > c.baseline.instruments.len()
                            && track_count(s) > track_count(c.baseline)
                    },
                },
                Step {
                    text: "It sounds the same, but now it's audio you can chop. On the new track, change a slice number to play a different piece.",
                    keys: "0-f",
                    focus: Focus::Grid,
                    done: |c| {
                        let (p, t) = (
                            c.editor.pattern_index(),
                            track_count(&c.editor.song).saturating_sub(1),
                        );
                        track_count(c.baseline) == t + 1
                            && cells_in(&c.editor.song, p, t) != cells_in(c.baseline, p, t)
                    },
                },
                Step {
                    text: "Mangle a slice of the bounce: reverse it (v), retrigger it (r) or pitch it (p). Bounce again for deeper damage.",
                    keys: "v, r, p",
                    focus: Focus::Grid,
                    done: |c| {
                        let (p, t) = (
                            c.editor.pattern_index(),
                            track_count(&c.editor.song).saturating_sub(1),
                        );
                        let fx = |s| {
                            cells_in(s, p, t)
                                .iter()
                                .filter(|c| c.fx().next().is_some())
                                .count()
                        };
                        fx(&c.editor.song) > fx(c.baseline)
                    },
                },
                Step {
                    text: "Have a listen.",
                    keys: "space",
                    focus: Focus::Grid,
                    done: |c| c.facts.played && !c.facts.playing,
                },
            ],
        },
        Lesson {
            title: "Save and export",
            steps: vec![
                Step {
                    text: "Keep your work: ctrl+s, type a name and press enter. Samples you made are saved next to it.",
                    keys: "ctrl+s",
                    focus: Focus::Transport,
                    done: |c| c.facts.saved,
                },
                Step {
                    text: "ctrl+e exports the song to a WAV file, with the tails of echoes and releases.",
                    keys: "ctrl+e",
                    focus: Focus::Transport,
                    done: |c| c.facts.exported,
                },
            ],
        },
    ]
}

pub struct Tutorial {
    lessons: Vec<Lesson>,
    pub lesson: usize,
    pub step: usize,
    baseline: Song,
}

impl Tutorial {
    /// Start at the first step, with `song` as it is now.
    pub fn new(song: &Song) -> Self {
        Tutorial {
            lessons: lessons(),
            lesson: 0,
            step: 0,
            baseline: song.clone(),
        }
    }

    pub fn lesson_count(&self) -> usize {
        self.lessons.len()
    }

    pub fn current(&self) -> Option<(&Lesson, &Step)> {
        let l = self.lessons.get(self.lesson)?;
        Some((l, l.steps.get(self.step)?))
    }

    pub fn finished(&self) -> bool {
        self.current().is_none()
    }

    /// Check the current step; move on if it's done. Returns true when it
    /// moved on, so per-step facts (`played`, `auditioned`) can be reset.
    pub fn update(&mut self, editor: &Editor, facts: Facts) -> bool {
        let Some((_, step)) = self.current() else {
            return false;
        };
        let ctx = Ctx {
            editor,
            baseline: &self.baseline,
            facts,
        };
        if !(step.done)(&ctx) {
            return false;
        }
        self.advance(&editor.song);
        true
    }

    /// Pick up at `lesson` and `step` with `song` as it was saved.
    pub fn resume(song: &Song, lesson: usize, step: usize) -> Self {
        let mut t = Tutorial::new(song);
        t.lesson = lesson.min(t.lessons.len());
        t.step = t
            .lessons
            .get(t.lesson)
            .map_or(0, |l| step.min(l.steps.len() - 1));
        t
    }

    /// Skip the current step.
    pub fn skip(&mut self, song: &Song) {
        if !self.finished() {
            self.advance(song);
        }
    }

    fn advance(&mut self, song: &Song) {
        self.step += 1;
        if self.step >= self.lessons[self.lesson].steps.len() {
            self.lesson += 1;
            self.step = 0;
        }
        self.baseline = song.clone();
    }
}

/// Where tutorial progress is kept: `$XDG_STATE_HOME/cowbell`, or
/// `~/.local/state/cowbell`.
pub fn state_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME").filter(|d| !d.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    Some(base.join("cowbell"))
}

const PROGRESS: &str = "tutorial.progress";
const SONG: &str = "tutorial.cowbell";

/// Save the tutorial song and where you are in it, to resume later.
pub fn save_progress(dir: &Path, tutorial: &Tutorial, song: &Song) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    crate::project::save(song, &dir.join(SONG))?;
    let text = format!("{} {}\n", tutorial.lesson, tutorial.step);
    std::fs::write(dir.join(PROGRESS), text).map_err(|e| format!("{PROGRESS}: {e}"))
}

/// The saved tutorial, if there is one that loads.
pub fn load_progress(dir: &Path) -> Option<(Tutorial, Song)> {
    let text = std::fs::read_to_string(dir.join(PROGRESS)).ok()?;
    let mut words = text.split_whitespace().map(str::parse::<usize>);
    let (Some(Ok(lesson)), Some(Ok(step))) = (words.next(), words.next()) else {
        return None;
    };
    let song = crate::project::load(&dir.join(SONG)).ok()?;
    Some((Tutorial::resume(&song, lesson, step), song))
}

/// Forget saved progress, so the tutorial starts from the first lesson.
pub fn clear_progress(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(PROGRESS));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Action, Field};

    /// Plays the tutorial: an editor plus the facts the app would report.
    struct Sim {
        editor: Editor,
        facts: Facts,
    }

    impl Sim {
        fn keys(&mut self, actions: &[Action]) {
            actions.iter().for_each(|&a| self.editor.apply(a));
        }

        fn type_str(&mut self, s: &str) {
            s.chars().for_each(|c| self.editor.apply(Action::Char(c)));
        }

        fn go(&mut self, track: usize, row: usize, field: Field) {
            self.editor.go_to(0, row);
            self.editor.cursor.track = track;
            self.editor.cursor.field = field;
            self.editor.cursor.digit = 0;
        }

        /// Change the rack as a mouse edit would.
        fn rack(&mut self, f: impl FnOnce(&mut Rack)) {
            let before = self.editor.song.rack.clone();
            f(&mut self.editor.song.rack);
            self.editor.record_rack(before);
        }

        fn play(&mut self) {
            self.facts.playing = true;
            self.facts.played = true;
        }
    }

    type Solution = fn(&mut Sim);

    fn dev(r: &Rack, kind: Kind) -> crate::rack::DeviceId {
        r.devices.iter().find(|d| d.kind == kind).unwrap().id
    }

    fn jack(device: crate::rack::DeviceId, port: usize) -> crate::rack::Jack {
        crate::rack::Jack { device, port }
    }

    /// One way to do each step, as a person might.
    fn solutions() -> Vec<Vec<Solution>> {
        vec![
            vec![
                |s| s.play(),
                |s| s.facts.playing = false,
                |s| s.keys(&[Action::Down; 8]),
                |s| s.keys(&[Action::NextTrack]),
                |s| s.facts.rack_view = true,
                |s| s.facts.rack_view = false,
            ],
            vec![
                |s| s.facts.auditioned = true,
                |s| {
                    let (i, _) = s.editor.track_sample().unwrap();
                    s.editor.redetect(i, 0.2);
                },
                |s| {
                    let (i, _) = s.editor.track_sample().unwrap();
                    s.editor.redetect(i, 0.6);
                },
                |s| {
                    let (i, sample) = s.editor.track_sample().unwrap();
                    let frame = (sample.slices[1] + sample.slices[2]) / 2;
                    s.editor.insert_marker(i, frame);
                },
                |s| {
                    let (i, sample) = s.editor.track_sample().unwrap();
                    let to = sample.slices[2] + 50;
                    s.editor.move_marker(i, 2, to, true);
                },
                |s| {
                    let (i, _) = s.editor.track_sample().unwrap();
                    s.editor.delete_marker(i, 3);
                },
            ],
            vec![
                |s| s.go(SNR, 4, Field::Slice),
                |s| s.type_str("02"),
                |s| {
                    s.go(SNR, 12, Field::Slice);
                    s.type_str("02");
                },
                |s| {
                    s.go(SNR, 12, Field::Velocity);
                    s.type_str("50");
                },
                |s| s.keys(&[Action::EditStep(1)]),
                |s| {
                    s.play();
                    s.facts.playing = false;
                },
            ],
            vec![
                |s| {
                    s.go(BRK, 0, Field::Fx(0));
                    s.type_str("v");
                },
                |s| {
                    s.go(BRK, 2, Field::Fx(0));
                    s.type_str("r03");
                },
                |s| {
                    s.go(BRK, 4, Field::Fx(1));
                    s.type_str("p0c");
                },
                |s| {
                    s.go(SNR, 0x18, Field::Slice);
                    s.keys(&[Action::SelectDown; 7]);
                    s.keys(&[Action::Roll]);
                },
                |s| s.facts.palette_open = true,
                |s| {
                    s.facts.palette_open = false;
                    s.play();
                },
            ],
            vec![
                |s| s.facts.rack_view = true,
                |s| {
                    s.rack(|r| {
                        r.add(Kind::Crusher);
                    })
                },
                |s| {
                    s.rack(|r| {
                        let brk = r.track_device(BRK).unwrap().id;
                        let cr = dev(r, Kind::Crusher);
                        let to = r.cables.iter().find(|c| c.from.device == brk).unwrap().to;
                        r.disconnect(to);
                        r.connect(jack(brk, 0), jack(cr, 0)).unwrap();
                    })
                },
                |s| {
                    s.rack(|r| {
                        let (cr, mix) = (dev(r, Kind::Crusher), dev(r, Kind::Mixer));
                        r.connect(jack(cr, 0), jack(mix, 0)).unwrap();
                    })
                },
                |s| {
                    s.rack(|r| {
                        let cr = dev(r, Kind::Crusher);
                        r.set_param(cr, 0, 3.0);
                    })
                },
                |s| {
                    s.rack(|r| {
                        let f = r.add(Kind::Filter);
                        let (snr, mix) = (r.track_device(SNR).unwrap().id, dev(r, Kind::Mixer));
                        r.connect(jack(snr, 0), jack(f, 0)).unwrap();
                        r.connect(jack(f, 0), jack(mix, 1)).unwrap();
                    })
                },
                |s| {
                    s.rack(|r| {
                        let lfo = r.add(Kind::Lfo);
                        r.set_param(lfo, 1, 2.0);
                        let brk = r.track_device(BRK).unwrap().id;
                        r.connect(jack(brk, 1), jack(lfo, 0)).unwrap();
                    })
                },
                |s| {
                    s.rack(|r| {
                        let (lfo, f) = (dev(r, Kind::Lfo), dev(r, Kind::Filter));
                        r.connect(jack(lfo, 0), jack(f, 1)).unwrap();
                    })
                },
                |s| {
                    s.play();
                    s.facts.playing = false;
                },
            ],
            vec![
                |s| {
                    s.facts.rack_view = false;
                    s.keys(&[Action::AddTrack]);
                },
                |s| {
                    s.go(2, 0, Field::Slice);
                    s.keys(&[Action::AddSynth(SynthKind::Acid)]);
                },
                |s| {
                    s.go(2, 0, Field::Slice);
                    s.type_str("zxcv");
                },
                |s| {
                    s.go(2, 1, Field::Velocity);
                    s.type_str("40");
                },
                |s| {
                    s.go(2, 2, Field::Fx(0));
                    s.type_str("g10");
                },
                |s| {
                    s.go(2, 6, Field::Slice);
                    s.type_str(".");
                },
                |s| {
                    s.play();
                    s.facts.playing = false;
                },
            ],
            vec![
                |s| s.keys(&[Action::AddTrack]),
                |s| {
                    s.go(3, 0, Field::Slice);
                    s.keys(&[Action::AddSynth(SynthKind::Fm)]);
                },
                |s| {
                    s.go(3, 0, Field::Slice);
                    s.type_str("q");
                    s.go(3, 0, Field::Fx(0));
                    s.type_str("h47");
                },
                |s| {
                    s.go(3, 0, Field::Slice);
                    let (i, p) = s.editor.track_synth().unwrap();
                    let v = p.params[8] + 7.0;
                    s.editor.set_synth_param(i, 8, v, true);
                },
                |s| {
                    s.editor
                        .set_tuning(crate::tuning::builtin("stretched").unwrap())
                },
                |s| {
                    s.play();
                    s.facts.playing = false;
                },
            ],
            vec![
                |s| {
                    s.go(2, 0, Field::Slice);
                    s.editor
                        .set_automation(Some(crate::pattern::Target::Synth { param: 1 }));
                },
                |s| {
                    s.go(2, 0, Field::Fx(1));
                    s.type_str("k00");
                },
                |s| {
                    s.go(2, 0x1f, Field::Fx(1));
                    s.type_str("kff");
                },
                |s| {
                    s.go(2, 0, Field::Fx(1));
                    s.keys(&[Action::SelectDown; 0x1f]);
                    s.keys(&[Action::Interpolate]);
                },
                |s| {
                    s.play();
                    s.facts.playing = false;
                },
            ],
            vec![
                |s| {
                    s.go(BRK, 0, Field::Slice);
                    s.keys(&[Action::Variations]);
                },
                |s| {
                    s.keys(&[Action::NextOrder, Action::NextOrder]);
                    s.keys(&[Action::DeleteSlot]);
                },
                |s| s.keys(&[Action::DuplicatePattern]),
                |s| s.keys(&[Action::Length(-1), Action::Length(-1)]),
                |s| {
                    s.editor.go_to(0, 0);
                    s.play();
                    s.facts.playing = false;
                },
            ],
            vec![
                |s| {
                    s.go(BRK, 0, Field::Slice);
                    s.keys(&[Action::SelectDown; 15]);
                    s.keys(&[Action::Bounce]);
                },
                |s| {
                    let t = s.editor.pattern().tracks.len() - 1;
                    // Swap the second hit for the first slice.
                    let row = s.editor.pattern().tracks[t]
                        .cells
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.trigger.is_some())
                        .nth(1)
                        .unwrap()
                        .0;
                    s.go(t, row, Field::Slice);
                    s.type_str("00");
                },
                |s| {
                    let t = s.editor.pattern().tracks.len() - 1;
                    s.go(t, 4, Field::Fx(0));
                    s.type_str("v");
                },
                |s| {
                    s.play();
                    s.facts.playing = false;
                },
            ],
            vec![|s| s.facts.saved = true, |s| s.facts.exported = true],
        ]
    }

    #[test]
    fn every_step_can_be_done_and_isnt_done_before() {
        let song = song();
        let mut sim = Sim {
            editor: Editor::new(song.clone()),
            facts: Facts::default(),
        };
        let mut tut = Tutorial::new(&song);
        let all = solutions();
        assert_eq!(all.len(), tut.lesson_count(), "a solution per lesson");
        for (l, steps) in all.iter().enumerate() {
            assert_eq!(
                steps.len(),
                lessons()[l].steps.len(),
                "lesson {l}: a solution per step"
            );
            for (k, solve) in steps.iter().enumerate() {
                let label = format!("lesson {} step {}", l + 1, k + 1);
                assert_eq!((tut.lesson, tut.step), (l, k), "{label}");
                assert!(
                    !tut.update(&sim.editor, sim.facts),
                    "{label} was done before doing it"
                );
                solve(&mut sim);
                assert!(
                    tut.update(&sim.editor, sim.facts),
                    "{label} not done by its solution"
                );
                sim.facts.played = false;
                sim.facts.auditioned = false;
                sim.facts.exported = false;
            }
        }
        assert!(tut.finished());
    }

    #[test]
    fn tutorial_song_has_the_break_and_an_empty_snare_track() {
        let s = song();
        let hits = cells(&s, BRK)
            .iter()
            .filter(|c| c.trigger.is_some())
            .count();
        assert_eq!(hits, slice_count(&s, BRK), "every slice on its row");
        assert!(cells(&s, SNR).iter().all(|c| *c == Cell::default()));
    }

    #[test]
    fn progress_saves_and_resumes() {
        let dir = std::env::temp_dir().join(format!("cowbell-tutorial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_progress(&dir).is_none());
        let mut song = song();
        song.bpm = 140.0;
        let mut t = Tutorial::new(&song);
        for _ in 0..8 {
            t.skip(&song);
        }
        save_progress(&dir, &t, &song).unwrap();
        let (r, loaded) = load_progress(&dir).unwrap();
        assert_eq!((r.lesson, r.step), (1, 2));
        assert_eq!(loaded.bpm, 140.0);
        clear_progress(&dir);
        assert!(load_progress(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_clamps_out_of_range_positions() {
        let s = song();
        let t = Tutorial::resume(&s, 1, 99);
        assert_eq!((t.lesson, t.step), (1, lessons()[1].steps.len() - 1));
        assert!(Tutorial::resume(&s, 99, 0).finished());
    }

    #[test]
    fn skipping_moves_on() {
        let s = song();
        let mut t = Tutorial::new(&s);
        for _ in 0..6 {
            t.skip(&s);
        }
        assert_eq!((t.lesson, t.step), (1, 0));
    }
}
