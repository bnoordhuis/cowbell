//! The tutorial: lessons that walk through making a track, one step at a
//! time. No UI code; the window draws the current step.
//!
//! A step is done when the song or app is in the state it asks for, not
//! when particular keys were pressed, so any way of getting there counts
//! and you can wander off and come back. Checks compare against the song
//! as it was when the step started (the baseline).

use std::sync::Arc;

use crate::editor::Editor;
use crate::pattern::{Cell, Fx, Instrument, Pattern, Song};
use crate::rack::Rack;
use crate::sample::Sample;

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

fn all_tracks_fx(song: &Song, f: impl Fn(Fx) -> bool + Copy) -> usize {
    (0..2).map(|t| count_fx(song, t, f)).sum()
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

        fn play(&mut self) {
            self.facts.playing = true;
            self.facts.played = true;
        }
    }

    type Solution = fn(&mut Sim);

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
    fn skipping_moves_on() {
        let s = song();
        let mut t = Tutorial::new(&s);
        for _ in 0..6 {
            t.skip(&s);
        }
        assert_eq!((t.lesson, t.step), (1, 0));
    }
}
