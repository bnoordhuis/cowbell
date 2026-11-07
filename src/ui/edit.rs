//! Changes to the song from the views, made by the window with undo.
//!
//! The views never change the song themselves. Each frame they report
//! [`Edit`]s: one-shot ones, each an undo step, and "live" ones from a drag,
//! made on the song as it was when the drag started so a whole drag is one
//! undo step.

use std::sync::Arc;

use crate::engine::MAX_TRACKS;
use crate::rack::{DeviceId, Kind};
use crate::song::{Cell, ClipRef, Instrument, Song, Tick};

/// One change to the song.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    MoveClip {
        clip: ClipRef,
        track: usize,
        start: Tick,
    },
    ResizeClip {
        clip: ClipRef,
        rows: usize,
    },
    NewClip {
        track: usize,
        start: Tick,
    },
    DuplicateClip(ClipRef),
    DeleteClip(ClipRef),
    /// Replace a clip's cells; there must be as many as it has rows.
    SetCells(ClipRef, Vec<Cell>),
    /// Change a clip's grid, in rows per beat.
    Resolution(ClipRef, u32),
    /// A new track playing this instrument.
    AddTrack(usize),
    DuplicateTrack(usize),
    DeleteTrack(usize),
    RenameTrack(usize, String),
    Volume(usize, f32),
    Mute(usize),
    Solo(usize),
    /// Turn a knob of a synth instrument.
    SynthParam {
        instrument: usize,
        param: usize,
        value: f32,
    },
    /// Turn a knob in the rack.
    RackParam {
        device: DeviceId,
        param: usize,
        value: f32,
    },
    /// Add an effect at the end of a track's chain.
    AddEffect(usize, Kind),
    /// Take an effect out of its chain.
    RemoveEffect(DeviceId),
}

impl Edit {
    /// Mixing changes, which aren't worth an undo step.
    pub fn is_mixing(&self) -> bool {
        matches!(self, Edit::Mute(_) | Edit::Solo(_))
    }
}

/// What to select after an edit.
#[derive(Debug, PartialEq)]
pub enum Select {
    Keep,
    Clip(ClipRef),
    Track(usize),
}

/// Make an edit on `song`, or say why it can't be done.
pub fn apply(song: &mut Song, edit: &Edit) -> Result<Select, String> {
    /// A new clip: a bar of sixteenths, or what fits.
    const NEW_RPB: u32 = 4;
    const NEW_ROWS: usize = 16;
    let full = || format!("can't: {MAX_TRACKS} tracks is the most");
    let no_track = || "no such track".to_owned();
    match *edit {
        Edit::MoveClip { clip, track, start } => song
            .move_clip(clip, track, start)
            .map(Select::Clip)
            .ok_or_else(|| "no room there".to_owned()),
        Edit::ResizeClip { clip, rows } => {
            if song.resize_clip(clip, rows) {
                Ok(Select::Keep)
            } else {
                Err("no room: it would run into the next clip".into())
            }
        }
        Edit::NewClip { track, start } => song
            .new_clip(track, start, NEW_RPB, NEW_ROWS)
            .map(Select::Clip)
            .ok_or_else(|| "no room for a clip there".to_owned()),
        Edit::DuplicateClip(clip) => song
            .duplicate_clip(clip)
            .map(Select::Clip)
            .ok_or_else(|| "no room for a copy right after it".to_owned()),
        Edit::DeleteClip(clip) => {
            song.delete_clip(clip);
            Ok(Select::Track(clip.0))
        }
        Edit::SetCells(at, ref cells) => {
            let clip = song
                .tracks
                .get_mut(at.0)
                .and_then(|t| t.clips.get_mut(at.1))
                .ok_or("no such clip")?;
            if clip.cells.len() != cells.len() {
                return Err("the clip changed length".into());
            }
            clip.cells.clone_from(cells);
            Ok(Select::Keep)
        }
        Edit::Resolution(clip, rpb) => {
            if song.set_resolution(clip, rpb) {
                Ok(Select::Keep)
            } else {
                Err("no room: it would run into the next clip".into())
            }
        }
        Edit::AddTrack(instrument) => {
            let name = song
                .instruments
                .get(instrument)
                .map_or("track", |i| i.name())
                .to_owned();
            song.add_track(&name, instrument)
                .map(Select::Track)
                .ok_or_else(full)
        }
        Edit::DuplicateTrack(t) => song.duplicate_track(t).map(Select::Track).ok_or_else(full),
        Edit::DeleteTrack(t) => {
            song.delete_track(t);
            // The one that took its place; the window keeps it in range.
            Ok(Select::Track(t))
        }
        Edit::RenameTrack(t, ref name) => {
            let track = song.tracks.get_mut(t).ok_or_else(no_track)?;
            track.name = name.clone();
            Ok(Select::Keep)
        }
        Edit::Volume(t, v) => song
            .set_volume(t, v)
            .map(|_| Select::Keep)
            .ok_or_else(no_track),
        Edit::Mute(t) | Edit::Solo(t) => {
            let track = song.tracks.get_mut(t).ok_or_else(no_track)?;
            if matches!(edit, Edit::Mute(_)) {
                track.mute ^= true;
            } else {
                track.solo ^= true;
            }
            Ok(Select::Keep)
        }
        Edit::SynthParam {
            instrument,
            param,
            value,
        } => {
            let Some(Instrument::Synth(patch)) = song.instruments.get_mut(instrument) else {
                return Err("not a synth".into());
            };
            let spec = patch.kind.params().get(param).ok_or("no such knob")?;
            let value = spec.clamp(value);
            Arc::make_mut(patch).params[param] = value;
            Ok(Select::Keep)
        }
        Edit::RackParam {
            device,
            param,
            value,
        } => song
            .rack
            .set_param(device, param, value)
            .map(|_| Select::Keep)
            .ok_or_else(|| "no such knob".to_owned()),
        Edit::AddEffect(t, kind) => song
            .rack
            .append_effect(t, kind)
            .map(|_| Select::Keep)
            .ok_or_else(no_track),
        Edit::RemoveEffect(id) => {
            if song.rack.splice_out(id) {
                Ok(Select::Keep)
            } else {
                Err("can't remove that".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song::{Clip, Track};
    use crate::synthvoice::{SynthKind, SynthPatch};

    fn song() -> Song {
        let a = Track::new("a", 0).with_clip(Clip::beats(0, 4, 4));
        let b = Track::new("b", 0).with_clip(Clip::beats(0, 4, 4));
        Song::new(120.0, Vec::new(), vec![a, b])
    }

    #[test]
    fn edits_say_what_to_select() {
        let mut s = song();
        assert_eq!(
            apply(&mut s, &Edit::DuplicateClip((0, 0))),
            Ok(Select::Clip((0, 1)))
        );
        let to_b = Edit::MoveClip {
            clip: (0, 1),
            track: 1,
            start: 768,
        };
        assert_eq!(apply(&mut s, &to_b), Ok(Select::Clip((1, 1))));
        assert_eq!(apply(&mut s, &Edit::DeleteTrack(0)), Ok(Select::Track(0)));
        assert_eq!(s.tracks[0].name, "b");
        assert_eq!(apply(&mut s, &Edit::AddTrack(0)), Ok(Select::Track(1)));
        assert_eq!(s.tracks[1].name, "track", "no instrument to name it after");
    }

    #[test]
    fn edits_that_cant_be_made_say_why() {
        let mut s = song();
        let onto_b = Edit::MoveClip {
            clip: (0, 0),
            track: 1,
            start: 0,
        };
        assert_eq!(apply(&mut s, &onto_b), Err("no room there".to_owned()));
        assert!(
            apply(
                &mut s,
                &Edit::NewClip {
                    track: 0,
                    start: 10
                }
            )
            .is_err()
        );
        assert!(apply(&mut s, &Edit::RenameTrack(9, "x".into())).is_err());
        let short = Edit::SetCells((0, 0), vec![Cell::default(); 3]);
        assert!(apply(&mut s, &short).is_err(), "wrong length");
    }

    #[test]
    fn volume_rename_and_cells() {
        let mut s = song();
        apply(&mut s, &Edit::Volume(1, 0.25)).unwrap();
        assert_eq!(s.volume(1), 0.25);
        apply(&mut s, &Edit::RenameTrack(0, "kick".into())).unwrap();
        assert_eq!(s.tracks[0].name, "kick");
        let mut cells = vec![Cell::default(); 16];
        cells[4] = Cell::hit(2, 90);
        apply(&mut s, &Edit::SetCells((0, 0), cells.clone())).unwrap();
        assert_eq!(s.tracks[0].clips[0].cells, cells);
    }

    #[test]
    fn knobs_and_effects() {
        let mut s = song();
        s.instruments
            .push(Instrument::Synth(Arc::new(SynthPatch::new(
                SynthKind::Acid,
                "acid",
            ))));
        let turn = Edit::SynthParam {
            instrument: 0,
            param: 0,
            value: 1e9,
        };
        apply(&mut s, &turn).unwrap();
        let Instrument::Synth(p) = &s.instruments[0] else {
            panic!()
        };
        assert_eq!(p.params[0], SynthKind::Acid.params()[0].max, "clamped");
        apply(&mut s, &Edit::AddEffect(1, Kind::Delay)).unwrap();
        let d = s.rack.chain(1)[0];
        let set = Edit::RackParam {
            device: d,
            param: 2,
            value: 0.5,
        };
        apply(&mut s, &set).unwrap();
        assert_eq!(s.rack.device(d).unwrap().params[2], 0.5);
        apply(&mut s, &Edit::RemoveEffect(d)).unwrap();
        assert!(s.rack.chain(1).is_empty());
    }
}
