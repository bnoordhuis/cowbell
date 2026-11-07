//! Picking files with the mouse: a folder listing, shared with the browser,
//! and the file dialog for opening, saving and exporting.
//!
//! In the dialog, click a folder to go into it (the first row goes up), click a file to pick it and
//! double-click to pick it and confirm. The name field takes a typed name or
//! path too (`~/` is the home folder).

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};

use super::color;
use crate::project;

/// A folder or a file in a listing.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
}

/// What's in `dir`: its folders, then its files ending in `ext`, each sorted
/// by name ignoring case. Hidden ones (starting with a dot) are left out.
pub fn list(dir: &Path, ext: &str) -> std::io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let path = e.path();
            let dir = path.is_dir();
            let wanted = path
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case(ext));
            (dir || wanted).then_some(Entry { name, path, dir })
        })
        .collect();
    entries.sort_by_key(|e| (!e.dir, e.name.to_lowercase()));
    Ok(entries)
}

/// The first row of a listing: a way up to `dir`'s parent, named, so it
/// says where it goes. Returns the parent when clicked.
pub fn parent_row(ui: &mut egui::Ui, dir: &Path) -> Option<PathBuf> {
    let parent = dir.parent()?;
    let name = parent.file_name().map_or_else(
        || parent.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let label = RichText::new(format!("‹  up to {name}")).color(color::MUTED);
    ui.selectable_label(false, label)
        .on_hover_text(parent.display().to_string())
        .clicked()
        .then(|| parent.to_owned())
}

/// Typed text as a path: trimmed, and `~/` is the home folder.
pub fn typed_path(text: &str) -> PathBuf {
    let text = text.trim();
    match text.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map_or_else(|| PathBuf::from(text), |h| Path::new(&h).join(rest)),
        None => PathBuf::from(text),
    }
}

/// `path` with `.ext` added, unless it already ends in it.
pub fn with_extension(path: PathBuf, ext: &str) -> PathBuf {
    if path
        .extension()
        .is_some_and(|x| x.eq_ignore_ascii_case(ext))
    {
        return path;
    }
    let mut name = path.into_os_string();
    name.push(".");
    name.push(ext);
    name.into()
}

/// What a file dialog is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Open,
    SaveAs,
    Export,
}

impl Purpose {
    fn ext(self) -> &'static str {
        match self {
            Purpose::Open | Purpose::SaveAs => project::EXTENSION,
            Purpose::Export => "wav",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Purpose::Open => "Open a project",
            Purpose::SaveAs => "Save the project as",
            Purpose::Export => "Export the song as a WAV file",
        }
    }

    fn button(self) -> &'static str {
        match self {
            Purpose::Open => "Open",
            Purpose::SaveAs => "Save",
            Purpose::Export => "Export",
        }
    }
}

/// What the person did with the dialog this frame.
#[derive(Debug, PartialEq)]
pub enum Answer {
    Chose(PathBuf),
    Cancelled,
}

pub struct Dialog {
    pub purpose: Purpose,
    dir: PathBuf,
    /// The file name, or a path typed in.
    name: String,
    entries: Vec<Entry>,
    error: Option<String>,
    focused: bool,
}

impl Dialog {
    pub fn new(purpose: Purpose, dir: &Path, name: &str) -> Self {
        let mut d = Dialog {
            purpose,
            dir: PathBuf::new(),
            name: name.to_owned(),
            entries: Vec::new(),
            error: None,
            focused: false,
        };
        d.cd(dir);
        d
    }

    fn cd(&mut self, dir: &Path) {
        let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_owned());
        match list(&dir, self.purpose.ext()) {
            Ok(entries) => {
                self.entries = entries;
                self.dir = dir;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{}: {e}", dir.display())),
        }
    }

    /// The file OK would pick: the name in the folder (or the path typed),
    /// with the extension added when saving.
    pub fn chosen(&self) -> Option<PathBuf> {
        if self.name.trim().is_empty() {
            return None;
        }
        let typed = typed_path(&self.name);
        let path = if typed.is_absolute() {
            typed
        } else {
            self.dir.join(typed)
        };
        Some(match self.purpose {
            Purpose::Open => path,
            Purpose::SaveAs | Purpose::Export => with_extension(path, self.purpose.ext()),
        })
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<Answer> {
        let mut answer = None;
        let mut go = None;
        let modal = egui::Modal::new(egui::Id::unique("file dialog")).show(ctx, |ui| {
            ui.set_width(620.0);
            ui.heading(self.purpose.title());
            ui.add_space(6.0);
            ui.label(RichText::new(self.dir.display().to_string()).color(color::MUTED));
            ui.add_space(4.0);
            egui::Frame::new()
                .fill(color::BG)
                .inner_margin(6.0)
                .corner_radius(4.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .min_scrolled_height(320.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if let Some(up) = parent_row(ui, &self.dir) {
                                go = Some(up);
                            }
                            if self.entries.is_empty() {
                                ui.label(
                                    RichText::new(format!(
                                        "no folders or .{} files here",
                                        self.purpose.ext()
                                    ))
                                    .color(color::DIM),
                                );
                            }
                            for e in &self.entries {
                                let label = if e.dir {
                                    RichText::new(format!("{}/", e.name)).color(color::TEXT)
                                } else {
                                    RichText::new(&e.name).color(color::ACCENT)
                                };
                                let selected = !e.dir && self.name == e.name;
                                let r = ui.selectable_label(selected, label);
                                if e.dir && r.clicked() {
                                    go = Some(e.path.clone());
                                } else if !e.dir && r.clicked() {
                                    self.name = e.name.clone();
                                }
                                if !e.dir && r.double_clicked() {
                                    self.name = e.name.clone();
                                    answer = self.chosen().map(Answer::Chose);
                                }
                            }
                        });
                });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Name");
                let edit =
                    ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(f32::INFINITY));
                if !self.focused {
                    edit.request_focus();
                    self.focused = true;
                }
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    answer = self.chosen().map(Answer::Chose);
                }
            });
            let chosen = self.chosen();
            let warning = match (&self.error, &chosen) {
                (Some(e), _) => Some(e.clone()),
                (None, Some(p)) if self.purpose != Purpose::Open && p.exists() => {
                    Some("this replaces the file that's there".into())
                }
                (None, Some(p)) if self.purpose == Purpose::Open && !p.is_file() => {
                    Some("no such file".into())
                }
                _ => None,
            };
            if let Some(w) = warning {
                ui.label(RichText::new(w).color(color::ERROR));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let ok = chosen
                    .as_ref()
                    .is_some_and(|p| self.purpose != Purpose::Open || p.is_file());
                if ui
                    .add_enabled(ok, egui::Button::new(self.purpose.button()))
                    .clicked()
                {
                    answer = chosen.clone().map(Answer::Chose);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(Answer::Cancelled);
                }
            });
        });
        if let Some(dir) = go {
            self.cd(&dir);
        }
        if answer.is_none() && modal.should_close() {
            answer = Some(Answer::Cancelled);
        }
        // Opening a file that isn't there can't go ahead.
        match answer {
            Some(Answer::Chose(p)) if self.purpose == Purpose::Open && !p.is_file() => None,
            a => a,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cowbell-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn listings_put_folders_first_and_keep_matching_files() {
        let dir = temp_dir("list");
        for f in ["b.wav", "A.WAV", "notes.txt", ".hidden.wav", "c.cowbell"] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        std::fs::create_dir(dir.join("zz")).unwrap();
        std::fs::create_dir(dir.join(".git")).unwrap();
        let names: Vec<String> = list(&dir, "wav")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["zz", "A.WAV", "b.wav"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saving_adds_the_extension_and_takes_typed_paths() {
        let dir = temp_dir("chosen");
        let mut d = Dialog::new(Purpose::SaveAs, &dir, "song");
        assert_eq!(d.chosen(), Some(dir.join("song.cowbell")));
        d.name = "song.COWBELL".into();
        assert_eq!(d.chosen(), Some(dir.join("song.COWBELL")), "already has it");
        d.name = "/elsewhere/tune".into();
        assert_eq!(d.chosen(), Some(PathBuf::from("/elsewhere/tune.cowbell")));
        d.name = "  ".into();
        assert_eq!(d.chosen(), None);
        let e = Dialog::new(Purpose::Export, &dir, "mix");
        assert_eq!(e.chosen(), Some(dir.join("mix.wav")));
        let o = Dialog::new(Purpose::Open, &dir, "x.cowbell");
        assert_eq!(o.chosen(), Some(dir.join("x.cowbell")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_folder_keeps_the_last_one() {
        let dir = temp_dir("cd");
        let mut d = Dialog::new(Purpose::Open, &dir, "");
        d.cd(&dir.join("nope"));
        assert_eq!(d.dir, dir);
        assert!(d.error.is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
