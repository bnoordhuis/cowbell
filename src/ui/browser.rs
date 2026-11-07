//! The browser, left of the arrangement: instruments to start a track
//! with, and a folder of samples.
//!
//! Click an instrument to add a track playing it. Click a folder to go into
//! it; double-click a sample, or pick it and click "Add", to add a track
//! playing it, sliced on its hits.

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};

use super::color;
use super::files::{self, Entry};
use crate::synthvoice::SynthKind;

/// What to start a new track with.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    Synth(SynthKind),
    /// The built-in drum kit, as a sliced break.
    Kit,
    Sample(PathBuf),
}

pub struct Browser {
    dir: PathBuf,
    entries: Vec<Entry>,
    selected: Option<PathBuf>,
    error: Option<String>,
}

impl Browser {
    pub fn new(dir: &Path) -> Self {
        let mut b = Browser {
            dir: PathBuf::new(),
            entries: Vec::new(),
            selected: None,
            error: None,
        };
        b.cd(dir);
        b
    }

    fn cd(&mut self, dir: &Path) {
        let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_owned());
        match files::list(&dir, "wav") {
            Ok(entries) => {
                self.entries = entries;
                self.dir = dir;
                self.error = None;
                self.selected = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<Pick> {
        let mut pick = None;
        let heading = |ui: &mut egui::Ui, text: &str| {
            ui.label(RichText::new(text).size(12.0).strong().color(color::MUTED));
        };
        let item = |ui: &mut egui::Ui, name: &str, what: &str| {
            let text = format!("{name}\n{what}");
            let button = egui::Button::new(RichText::new(text).size(14.0))
                .min_size(egui::vec2(ui.available_width(), 44.0));
            ui.add(button).clicked()
        };

        heading(ui, "INSTRUMENTS");
        if item(ui, "Drum kit", "a CC0 kit, sliced like a break") {
            pick = Some(Pick::Kit);
        }
        if item(ui, "Acid synth", "squelchy bass and leads") {
            pick = Some(Pick::Synth(SynthKind::Acid));
        }
        if item(ui, "FM synth", "bells, pads, basses") {
            pick = Some(Pick::Synth(SynthKind::Fm));
        }

        ui.add_space(14.0);
        heading(ui, "SAMPLES");
        let mut go = None;
        ui.horizontal(|ui| {
            let up = self.dir.parent().map(Path::to_owned);
            if ui
                .add_enabled(up.is_some(), egui::Button::new("Up"))
                .clicked()
            {
                go = up;
            }
            let name = self.dir.file_name().map_or_else(
                || self.dir.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            ui.label(RichText::new(name).color(color::TEXT))
                .on_hover_text(self.dir.display().to_string());
        });
        if let Some(e) = &self.error {
            ui.label(RichText::new(e).color(color::ERROR));
        }
        let add_h = 60.0;
        let list_h = (ui.available_height() - add_h).max(80.0);
        egui::ScrollArea::vertical()
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.entries.is_empty() {
                    ui.label(RichText::new("no folders or .wav files here").color(color::DIM));
                }
                for e in &self.entries {
                    let label = if e.dir {
                        RichText::new(format!("{}/", e.name)).color(color::TEXT)
                    } else {
                        RichText::new(&e.name).color(color::ACCENT)
                    };
                    let selected = self.selected.as_ref() == Some(&e.path);
                    let r = ui.selectable_label(selected, label);
                    if e.dir && r.clicked() {
                        go = Some(e.path.clone());
                    } else if !e.dir && r.clicked() {
                        self.selected = Some(e.path.clone());
                    }
                    if !e.dir && r.double_clicked() {
                        pick = Some(Pick::Sample(e.path.clone()));
                    }
                }
            });
        ui.add_space(6.0);
        let add =
            egui::Button::new("Add as a track").min_size(egui::vec2(ui.available_width(), 32.0));
        if ui.add_enabled(self.selected.is_some(), add).clicked() {
            pick = self.selected.clone().map(Pick::Sample);
        }
        ui.label(
            RichText::new("double-click a sample to add it")
                .size(12.0)
                .color(color::DIM),
        );
        if let Some(dir) = go {
            self.cd(&dir);
        }
        pick
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_folder_forgets_the_pick() {
        let dir = std::env::temp_dir().join(format!("cowbell-browser-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("hit.wav"), b"").unwrap();
        let mut b = Browser::new(&dir);
        assert_eq!(b.entries.len(), 2);
        b.selected = Some(dir.join("hit.wav"));
        b.cd(&dir.join("sub"));
        assert_eq!(b.selected, None);
        assert!(b.entries.is_empty());
        b.cd(&dir.join("missing"));
        assert_eq!(
            b.dir,
            std::path::absolute(dir.join("sub")).unwrap(),
            "stays put"
        );
        assert!(b.error.is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
