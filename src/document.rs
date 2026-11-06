//! The song being edited, with undo. The UI changes the song through here so
//! every change bumps the revision (for saving and resending to the engine)
//! and, when it should, can be undone.

use crate::rack::Rack;
use crate::song::Song;

/// Undo steps kept.
const HISTORY: usize = 200;

pub struct Document {
    pub song: Song,
    /// Bumped on every change, undoable or not.
    pub revision: u64,
    undo: Vec<Song>,
    redo: Vec<Song>,
}

impl Document {
    pub fn new(song: Song) -> Self {
        Document {
            song,
            revision: 0,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Change the song as one undo step.
    pub fn edit<R>(&mut self, f: impl FnOnce(&mut Song) -> R) -> R {
        let before = self.song.clone();
        let r = f(&mut self.song);
        self.record(before);
        r
    }

    /// The song was changed in place; `before` is how it was, for undo.
    pub fn record(&mut self, before: Song) {
        if self.undo.len() == HISTORY {
            self.undo.remove(0);
        }
        self.undo.push(before);
        self.redo.clear();
        self.revision += 1;
    }

    /// The rack was changed in place; `before` is how it was, for undo.
    pub fn record_rack(&mut self, before: Rack) {
        let mut song = self.song.clone();
        song.rack = before;
        self.record(song);
    }

    /// The song was changed in a way that isn't worth undoing.
    pub fn changed(&mut self) {
        self.revision += 1;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(song) = self.undo.pop() else {
            return false;
        };
        self.redo.push(std::mem::replace(&mut self.song, song));
        self.revision += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(song) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.song, song));
        self.revision += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song::Track;

    fn doc() -> Document {
        Document::new(Song::new(120.0, Vec::new(), vec![Track::new("a", 0)]))
    }

    #[test]
    fn edits_undo_and_redo() {
        let mut d = doc();
        d.edit(|s| s.bpm = 170.0);
        d.edit(|s| s.tracks[0].name = "b".into());
        assert_eq!(d.revision, 2);
        assert!(d.undo());
        assert_eq!(d.song.tracks[0].name, "a");
        assert!(d.undo());
        assert_eq!(d.song.bpm, 120.0);
        assert!(!d.undo(), "nothing left");
        assert!(d.redo());
        assert_eq!(d.song.bpm, 170.0);
        d.edit(|s| s.bpm = 90.0);
        assert!(!d.can_redo(), "a new edit drops the redo steps");
    }

    #[test]
    fn rack_changes_undo() {
        let mut d = doc();
        let before = d.song.rack.clone();
        d.song.rack.add(crate::rack::Kind::Delay);
        d.record_rack(before);
        let devices = d.song.rack.devices.len();
        d.undo();
        assert_eq!(d.song.rack.devices.len(), devices - 1);
    }

    #[test]
    fn plain_changes_bump_the_revision_only() {
        let mut d = doc();
        d.song.tracks[0].mute = true;
        d.changed();
        assert_eq!(d.revision, 1);
        assert!(!d.can_undo());
    }
}
