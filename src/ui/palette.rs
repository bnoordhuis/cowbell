//! The command palette: every command with its keys, searchable, so you
//! can find and run things without knowing the shortcut. Open it with `?`
//! or ctrl+P.

use eframe::egui::{self, Align2, Color32, FontId, Rect, Stroke, StrokeKind, pos2, vec2};

use super::{Act, View, color};
use crate::editor::Action;
use crate::synthvoice::SynthKind;

pub struct Command {
    pub name: &'static str,
    pub keys: &'static str,
    /// Extra words to find it by.
    pub tags: &'static str,
    /// `None` for entries that only explain something.
    pub act: Option<Act>,
}

fn cmd(name: &'static str, keys: &'static str, tags: &'static str, act: Act) -> Command {
    Command {
        name,
        keys,
        tags,
        act: Some(act),
    }
}

fn info(name: &'static str, keys: &'static str, tags: &'static str) -> Command {
    Command {
        name,
        keys,
        tags,
        act: None,
    }
}

pub fn commands() -> Vec<Command> {
    use Action::*;
    let e = Act::Edit;
    vec![
        cmd("Play or stop", "space", "start transport", Act::PlayToggle),
        cmd(
            "Play this pattern from the top",
            "enter",
            "start",
            Act::PlayPattern,
        ),
        cmd(
            "Follow the playhead on or off",
            "ctrl+f",
            "scroll",
            Act::ToggleFollow,
        ),
        cmd("Tempo up", "alt+↑", "bpm faster", Act::Bpm(1.0)),
        cmd("Tempo down", "alt+↓", "bpm slower", Act::Bpm(-1.0)),
        cmd(
            "Tracker view",
            "F1",
            "pattern grid",
            Act::View(View::Tracker),
        ),
        cmd(
            "Rack view: devices and cables",
            "F2",
            "patch effects",
            Act::View(View::Rack),
        ),
        cmd(
            "Tutorial on or off",
            "F12",
            "help learn lesson guide",
            Act::Tutorial,
        ),
        cmd("Save", "ctrl+s", "file project", Act::Save),
        cmd(
            "Save as",
            "ctrl+shift+s",
            "file project rename",
            Act::SaveAs,
        ),
        cmd("Open a project", "ctrl+o", "file load", Act::Open),
        cmd(
            "Export the song to WAV",
            "ctrl+e",
            "render bounce file audio",
            Act::Export,
        ),
        cmd(
            "Load a sample (WAV) onto this track",
            "ctrl+l",
            "break import audio",
            Act::LoadSample,
        ),
        cmd(
            "Load a tuning (Scala .scl)",
            "ctrl+shift+l",
            "microtonal scale",
            Act::LoadTuning,
        ),
        cmd("Undo", "ctrl+z", "back", e(Undo)),
        cmd("Redo", "ctrl+y", "forward", e(Redo)),
        cmd("Copy the selection", "ctrl+c", "clipboard", e(Copy)),
        cmd("Paste", "ctrl+v", "clipboard", e(Paste)),
        cmd(
            "Clear the cell or selection",
            "delete",
            "erase remove",
            e(Clear),
        ),
        cmd("Select rows", "shift+↑ ↓", "selection mark", e(SelectDown)),
        cmd(
            "Roll: retrigger ramp over the selection",
            "ctrl+r",
            "snare fill ratchet",
            e(Roll),
        ),
        cmd(
            "Fill the selection between its ends",
            "ctrl+k",
            "interpolate sweep curve ramp",
            e(Interpolate),
        ),
        cmd(
            "Make variations of this pattern",
            "ctrl+g",
            "generate random mutate ideas",
            e(Variations),
        ),
        cmd(
            "Bounce the selection to audio",
            "ctrl+b",
            "resample render rechop",
            e(Bounce),
        ),
        cmd("Add a track", "ctrl+t", "column channel", e(AddTrack)),
        cmd("Edit step up", "ctrl+↑", "skip rows", e(EditStep(1))),
        cmd("Edit step down", "ctrl+↓", "skip rows", e(EditStep(-1))),
        cmd("Next order slot", "ctrl+→", "song pattern", e(NextOrder)),
        cmd(
            "Previous order slot",
            "ctrl+←",
            "song pattern",
            e(PrevOrder),
        ),
        cmd("New empty pattern", "ctrl+n", "song order", e(NewPattern)),
        cmd(
            "Duplicate this pattern",
            "ctrl+d",
            "copy song order",
            e(DuplicatePattern),
        ),
        cmd(
            "Play this pattern again in the next slot",
            "ctrl+shift+d",
            "repeat order",
            e(RepeatSlot),
        ),
        cmd(
            "Remove this order slot",
            "ctrl+delete",
            "song delete",
            e(DeleteSlot),
        ),
        cmd(
            "This slot plays the next pattern",
            "ctrl+shift+→",
            "order",
            e(SlotPattern(1)),
        ),
        cmd(
            "This slot plays the previous pattern",
            "ctrl+shift+←",
            "order",
            e(SlotPattern(-1)),
        ),
        cmd(
            "More rows per beat",
            "ctrl+shift+↑",
            "resolution finer grid",
            e(Resolution(1)),
        ),
        cmd(
            "Fewer rows per beat",
            "ctrl+shift+↓",
            "resolution coarser grid",
            e(Resolution(-1)),
        ),
        cmd(
            "Pattern a beat longer",
            "alt+→",
            "length bar meter",
            Act::LengthBeats(1),
        ),
        cmd(
            "Pattern a beat shorter",
            "alt+←",
            "length bar meter",
            Act::LengthBeats(-1),
        ),
        cmd(
            "Pattern a row longer",
            "alt+shift+→",
            "length odd meter 7/8",
            e(Length(1)),
        ),
        cmd(
            "Pattern a row shorter",
            "alt+shift+←",
            "length odd meter 7/8",
            e(Length(-1)),
        ),
        cmd(
            "Put an acid synth on this track",
            "F5",
            "303 bass instrument",
            Act::AddSynth(SynthKind::Acid),
        ),
        cmd(
            "Put an FM synth on this track",
            "F6",
            "bells keys instrument",
            Act::AddSynth(SynthKind::Fm),
        ),
        cmd(
            "Next sample or synth on this track",
            "ctrl+i",
            "instrument switch",
            e(NextInstrument),
        ),
        cmd(
            "Previous sample or synth on this track",
            "ctrl+shift+i",
            "instrument switch",
            e(PrevInstrument),
        ),
        cmd(
            "More slices (or octave up on synths)",
            "]",
            "sensitivity detect slicer",
            Act::Sensitivity(0.1),
        ),
        cmd(
            "Fewer slices (or octave down on synths)",
            "[",
            "sensitivity detect slicer",
            Act::Sensitivity(-0.1),
        ),
        info(
            "Effects: type a letter, then two hex digits, in an effect column",
            "O R V P B S C G H K",
            "offset retrigger reverse pitch crush stutter cut glide chord knob automation fx",
        ),
        info(
            "Notes on synth tracks: z-m and q-u, . for note off",
            "z s x d c …",
            "piano keyboard octave",
        ),
        info(
            "Slicer: click hear · ctrl+click enter · shift+click add · drag move · right-click remove",
            "mouse",
            "markers slices",
        ),
        info(
            "Rack: drag between jacks to patch · right-click a knob to automate it with K",
            "mouse",
            "cables patch",
        ),
    ]
}

pub struct Palette {
    pub query: String,
    pub selected: usize,
    commands: Vec<Command>,
}

impl Palette {
    pub fn new() -> Self {
        Palette {
            query: String::new(),
            selected: 0,
            commands: commands(),
        }
    }

    /// Commands matching every word of the query, in name, keys or tags.
    pub fn matches(&self) -> Vec<&Command> {
        let words: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.commands
            .iter()
            .filter(|c| {
                let hay = format!("{} {} {}", c.name, c.keys, c.tags).to_lowercase();
                words.iter().all(|w| hay.contains(w.as_str()))
            })
            .collect()
    }

    /// The selected command's action, if it has one.
    pub fn chosen(&self) -> Option<Act> {
        self.matches().get(self.selected).and_then(|c| c.act)
    }

    pub fn move_selection(&mut self, delta: isize) {
        let n = self.matches().len().max(1) as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    pub fn draw(&self, ctx: &egui::Context, font: &FontId) {
        let screen = ctx.content_rect();
        let width = (screen.width() - 32.0).min(760.0);
        let shown = self.matches();
        let rows = shown.len().clamp(1, 16);
        let row_h = 22.0;
        let rect = Rect::from_min_size(
            pos2(screen.center().x - width / 2.0, screen.top() + 60.0),
            vec2(width, 40.0 + rows as f32 * row_h + 10.0),
        );
        let p = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("palette"),
        ));
        p.rect_filled(rect, 6.0, color::PANEL);
        p.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0, color::ACCENT),
            StrokeKind::Inside,
        );
        let q = if self.query.is_empty() {
            "type to search commands".to_owned()
        } else {
            self.query.clone()
        };
        let qc = if self.query.is_empty() {
            color::DIM
        } else {
            color::TEXT
        };
        p.text(
            rect.left_top() + vec2(14.0, 12.0),
            Align2::LEFT_TOP,
            format!("? {q}▏"),
            font.clone(),
            qc,
        );
        p.text(
            rect.right_top() + vec2(-14.0, 12.0),
            Align2::RIGHT_TOP,
            "↑↓ choose · enter run · esc close",
            font.clone(),
            color::DIM,
        );
        if shown.is_empty() {
            p.text(
                rect.left_top() + vec2(14.0, 42.0),
                Align2::LEFT_TOP,
                "nothing matches",
                font.clone(),
                color::DIM,
            );
        }
        // Keep the selection in view.
        let first = self.selected.saturating_sub(rows - 1);
        for (i, c) in shown.iter().enumerate().skip(first).take(rows) {
            let y = rect.top() + 40.0 + (i - first) as f32 * row_h;
            let band =
                Rect::from_min_size(pos2(rect.left() + 4.0, y), vec2(rect.width() - 8.0, row_h));
            if i == self.selected {
                p.rect_filled(band, 3.0, color::CURSOR_ROW);
            }
            let name_color: Color32 = if c.act.is_some() {
                color::TEXT
            } else {
                color::MUTED
            };
            p.text(
                band.left_center() + vec2(10.0, 0.0),
                Align2::LEFT_CENTER,
                c.name,
                font.clone(),
                name_color,
            );
            p.text(
                band.right_center() - vec2(10.0, 0.0),
                Align2::RIGHT_CENTER,
                c.keys,
                font.clone(),
                color::ACCENT,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_by_name_keys_or_tags() {
        let mut p = Palette::new();
        p.query = "chord".into();
        assert!(p.matches().iter().any(|c| c.name.starts_with("Effects")));
        p.query = "ctrl+g".into();
        assert_eq!(p.matches().len(), 1);
        p.query = "save AS".into();
        assert!(matches!(p.chosen(), Some(Act::SaveAs)));
        p.query = "zzz nothing".into();
        assert!(p.matches().is_empty() && p.chosen().is_none());
    }

    #[test]
    fn every_key_in_the_palette_is_described() {
        // Each command has a name and a key or "mouse".
        for c in commands() {
            assert!(!c.name.is_empty() && !c.keys.is_empty(), "{}", c.name);
        }
    }
}
