//! Dev-only screenshots, for checking the UI without a person at the screen.
//!
//! `COWBELL_SCREENSHOT=out.ppm` saves the window as a binary PPM and exits.
//! `COWBELL_KEYS` is a key script run first, to set up what's on screen:
//! plain characters are typed, and `<name>` is a key, e.g.
//! `<ctrl-right><down><down>03<sdown><sdown><roll>`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;

use crate::editor::{Action, Editor};

pub struct Screenshot {
    path: String,
    frames: u32,
    requested: bool,
    done: Arc<AtomicBool>,
}

impl Screenshot {
    pub fn from_env(editor: &mut Editor) -> Option<Self> {
        let path = std::env::var("COWBELL_SCREENSHOT").ok()?;
        if let Ok(keys) = std::env::var("COWBELL_KEYS") {
            for action in parse_keys(&keys) {
                editor.apply(action);
            }
            // Don't make a noise for slices typed by the script.
            editor.audition = None;
        }
        Some(Screenshot {
            path,
            frames: 0,
            requested: false,
            done: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Call once per frame.
    pub fn update(&mut self, ctx: &egui::Context) {
        self.frames += 1;
        if self.done.load(Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // Give layout a few frames to settle (fonts, panel sizes).
        if self.frames >= 4 && !self.requested {
            self.requested = true;
            let (path, done, ctx2) = (self.path.clone(), self.done.clone(), ctx.clone());
            let callback = egui::ScreenshotCallback::new(move |image| {
                if let Err(e) = write_ppm(&path, &image) {
                    eprintln!("{path}: {e}");
                }
                done.store(true, Ordering::Relaxed);
                ctx2.request_repaint();
            });
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(callback));
        }
        ctx.request_repaint();
    }
}

fn write_ppm(path: &str, image: &egui::ColorImage) -> std::io::Result<()> {
    let [w, h] = image.size;
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    out.reserve(w * h * 3);
    for p in &image.pixels {
        out.extend_from_slice(&[p.r(), p.g(), p.b()]);
    }
    std::fs::write(path, out)
}

fn parse_keys(script: &str) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut rest = script;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
        {
            let action = match &rest[1..end] {
                "up" => Action::Up,
                "down" => Action::Down,
                "left" => Action::Left,
                "right" => Action::Right,
                "sup" => Action::SelectUp,
                "sdown" => Action::SelectDown,
                "tab" => Action::NextTrack,
                "ctrl-right" => Action::NextOrder,
                "ctrl-left" => Action::PrevOrder,
                "roll" => Action::Roll,
                "bounce" => Action::Bounce,
                "undo" => Action::Undo,
                "redo" => Action::Redo,
                "del" => Action::Clear,
                "pgdn" => Action::PageDown,
                other => {
                    eprintln!("COWBELL_KEYS: unknown key <{other}>");
                    rest = &rest[end + 1..];
                    continue;
                }
            };
            actions.push(action);
            rest = &rest[end + 1..];
        } else {
            actions.push(Action::Char(c));
            rest = &rest[c.len_utf8()..];
        }
    }
    actions
}
