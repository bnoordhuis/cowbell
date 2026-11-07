//! Dev-only screenshots, for checking the UI without a person at the screen.
//!
//! `COWBELL_SCREENSHOT=out.ppm` saves the window as a binary PPM and exits.
//! `COWBELL_VIEW=rack` shows the rack instead of the arrangement,
//! `COWBELL_SELECT=track,clip` selects a clip for the clip editor, and
//! `COWBELL_TAB=devices` shows the devices tab instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;

pub struct Screenshot {
    path: String,
    frames: u32,
    requested: bool,
    done: Arc<AtomicBool>,
}

impl Screenshot {
    pub fn from_env() -> Option<Self> {
        let path = std::env::var("COWBELL_SCREENSHOT").ok()?;
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
