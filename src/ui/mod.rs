//! The tracker window: transport bar, slicer, pattern grid and status bar.
//!
//! Everything is drawn with the painter and nothing takes keyboard focus, so
//! every key goes to the tracker. Editing logic lives in [`crate::editor`];
//! this module only maps keys to editor actions and draws the result.

#[cfg(feature = "screenshot")]
mod screenshot;

use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, FontId, Key, Modifiers, Rect, Sense, Stroke, pos2, vec2,
};

use crate::audio::{self, Output};
use crate::editor::{Action, Editor, Field};
use crate::engine::{Command, Controller, Engine, Position};
use crate::pattern::{Cell, Instrument, Song};
use crate::sample::Sample;

mod color {
    use eframe::egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(0x16, 0x17, 0x1a);
    pub const PANEL: Color32 = Color32::from_rgb(0x1d, 0x1e, 0x22);
    pub const BEAT: Color32 = Color32::from_rgb(0x1b, 0x1c, 0x20);
    pub const BAR: Color32 = Color32::from_rgb(0x22, 0x23, 0x28);
    pub const CURSOR_ROW: Color32 = Color32::from_rgb(0x2c, 0x2d, 0x33);
    pub const SELECTION: Color32 = Color32::from_rgb(0x4b, 0x15, 0x28);
    pub const TEXT: Color32 = Color32::from_rgb(0xc9, 0xc6, 0xbd);
    pub const DIM: Color32 = Color32::from_rgb(0x5f, 0x5e, 0x5a);
    pub const MUTED: Color32 = Color32::from_rgb(0x88, 0x87, 0x80);
    pub const ACCENT: Color32 = Color32::from_rgb(0xef, 0x9f, 0x27);
    pub const WAVE: Color32 = Color32::from_rgb(0x5d, 0xca, 0xa5);
    pub const ERROR: Color32 = Color32::from_rgb(0xf0, 0x95, 0x95);
    pub const TRACKS: [Color32; 4] = [
        Color32::from_rgb(0x5d, 0xca, 0xa5),
        Color32::from_rgb(0xed, 0x93, 0xb1),
        Color32::from_rgb(0xaf, 0xa9, 0xec),
        Color32::from_rgb(0x85, 0xb7, 0xeb),
    ];
}

const FONT_SIZE: f32 = 13.0;
const ROW_HEIGHT: f32 = 17.0;
/// Characters per track column: "SS VV FFF FFF".
const TRACK_CHARS: usize = 13;
const TRACK_GAP: usize = 3;
const ROW_NUMBER_CHARS: usize = 4;

/// Character offset of each field within a track column.
fn field_offset(field: Field) -> usize {
    match field {
        Field::Slice => 0,
        Field::Velocity => 3,
        Field::Fx(i) => 6 + 4 * i,
    }
}

pub fn run(song: Song) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_title("cowbell"),
        ..Default::default()
    };
    eframe::run_native(
        "cowbell",
        options,
        Box::new(|_cc| Ok(Box::new(App::new(song)))),
    )
    .map_err(|e| e.to_string())
}

/// What a key press does, beyond editing.
enum Act {
    Edit(Action),
    PlayToggle,
    PlayPattern,
    ToggleFollow,
    Bpm(f64),
}

fn key_act(key: Key, m: Modifiers) -> Option<Act> {
    use Action::*;
    let edit = |a| Some(Act::Edit(a));
    let bpm_step = if m.shift { 10.0 } else { 1.0 };
    match key {
        Key::ArrowUp if m.alt => Some(Act::Bpm(bpm_step)),
        Key::ArrowDown if m.alt => Some(Act::Bpm(-bpm_step)),
        Key::ArrowUp if m.command => edit(EditStep(1)),
        Key::ArrowDown if m.command => edit(EditStep(-1)),
        Key::ArrowUp if m.shift => edit(SelectUp),
        Key::ArrowDown if m.shift => edit(SelectDown),
        Key::ArrowUp => edit(Up),
        Key::ArrowDown => edit(Down),
        Key::ArrowLeft if m.command => edit(PrevOrder),
        Key::ArrowRight if m.command => edit(NextOrder),
        Key::ArrowLeft => edit(Left),
        Key::ArrowRight => edit(Right),
        Key::PageUp => edit(PageUp),
        Key::PageDown => edit(PageDown),
        Key::Home => edit(Top),
        Key::End => edit(Bottom),
        Key::Tab if m.shift => edit(PrevTrack),
        Key::Tab => edit(NextTrack),
        Key::Delete | Key::Backspace => edit(Clear),
        Key::Space => Some(Act::PlayToggle),
        Key::Enter => Some(Act::PlayPattern),
        Key::Z if m.command && m.shift => edit(Redo),
        Key::Z if m.command => edit(Undo),
        Key::Y if m.command => edit(Redo),
        Key::C if m.command => edit(Copy),
        Key::V if m.command => edit(Paste),
        Key::R if m.command => edit(Roll),
        Key::T if m.command => edit(AddTrack),
        Key::F if m.command => Some(Act::ToggleFollow),
        _ => None,
    }
}

/// Min/max per pixel column, recomputed only when the sample or width changes.
#[derive(Default)]
struct WaveCache {
    sample: Option<Arc<Sample>>,
    width: usize,
    peaks: Vec<(f32, f32)>,
}

impl WaveCache {
    fn get(&mut self, sample: &Arc<Sample>, width: usize) -> &[(f32, f32)] {
        let fresh =
            self.sample.as_ref().is_some_and(|s| Arc::ptr_eq(s, sample)) && self.width == width;
        if !fresh {
            let n = sample.frames.len();
            self.peaks = (0..width)
                .map(|x| {
                    let (a, b) = (
                        x * n / width,
                        ((x + 1) * n / width).max(x * n / width + 1).min(n),
                    );
                    sample.frames[a.min(n.saturating_sub(1))..b]
                        .iter()
                        .map(|[l, r]| 0.5 * (l + r))
                        .fold((0.0f32, 0.0f32), |(lo, hi), s| (lo.min(s), hi.max(s)))
                })
                .collect();
            self.sample = Some(sample.clone());
            self.width = width;
        }
        &self.peaks
    }
}

struct App {
    editor: Editor,
    controller: Controller,
    output: Result<Output, String>,
    follow: bool,
    last_pos: Option<Position>,
    wave: WaveCache,
    char_width: f32,
    /// First row shown in the grid.
    scroll: usize,
    #[cfg(feature = "screenshot")]
    screenshot: Option<screenshot::Screenshot>,
}

impl App {
    fn new(song: Song) -> Self {
        #[allow(unused_mut)]
        let mut editor = Editor::new(song.clone());
        #[cfg(feature = "screenshot")]
        let screenshot = screenshot::Screenshot::from_env(&mut editor);
        let (output, controller) = match audio::start(song.clone()) {
            Ok((output, controller)) => (Ok(output), controller),
            // Keep editing without sound. The engine is dropped, so commands
            // just pile up in the queue until it's full.
            Err(e) => (Err(e), Engine::new(48_000, song).1),
        };
        App {
            editor,
            controller,
            output,
            follow: true,
            last_pos: None,
            wave: WaveCache::default(),
            char_width: 8.0,
            scroll: 0,
            #[cfg(feature = "screenshot")]
            screenshot,
        }
    }

    fn font() -> FontId {
        FontId::monospace(FONT_SIZE)
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let (events, modifiers) = ctx.input(|i| (i.events.clone(), i.modifiers));
        for event in events {
            let act = match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => key_act(key, modifiers),
                // Text arrives alongside its key event; skip it for shortcuts
                // and for space, which is play/stop.
                egui::Event::Text(text) if !modifiers.command && !modifiers.alt => {
                    for c in text.chars().filter(|c| !c.is_whitespace()) {
                        self.editor.apply(Action::Char(c));
                    }
                    None
                }
                egui::Event::Copy => Some(Act::Edit(Action::Copy)),
                egui::Event::Cut => {
                    self.editor.apply(Action::Copy);
                    Some(Act::Edit(Action::Clear))
                }
                egui::Event::Paste(_) => Some(Act::Edit(Action::Paste)),
                _ => None,
            };
            match act {
                Some(Act::Edit(a)) => self.editor.apply(a),
                Some(Act::PlayToggle) => {
                    let c = self.editor.cursor;
                    let cmd = if self.controller.is_playing() {
                        Command::Stop
                    } else {
                        Command::PlayFrom(Position {
                            order: c.order,
                            row: c.row,
                        })
                    };
                    let _ = self.controller.send(cmd);
                }
                Some(Act::PlayPattern) => {
                    let order = self.editor.cursor.order;
                    let _ = self
                        .controller
                        .send(Command::PlayFrom(Position { order, row: 0 }));
                }
                Some(Act::ToggleFollow) => self.follow = !self.follow,
                Some(Act::Bpm(d)) => {
                    let bpm = (self.editor.song.bpm + d).clamp(20.0, 400.0);
                    self.editor.song.bpm = bpm;
                    let _ = self.controller.send(Command::SetBpm(bpm));
                }
                None => {}
            }
        }
    }

    fn sync_engine(&mut self) {
        if self.editor.dirty {
            let song = Box::new(self.editor.song.clone());
            if self.controller.send(Command::SwapSong(song)).is_ok() {
                self.editor.dirty = false;
            }
        }
        if let Some((instrument, slice)) = self.editor.audition.take() {
            let _ = self.controller.send(Command::Audition {
                instrument,
                slice,
                velocity: 110,
            });
        }
        self.controller.collect_garbage();

        let pos = self.controller.position();
        if self.follow && self.controller.is_playing() && self.last_pos != Some(pos) {
            self.editor.go_to(pos.order, pos.row);
        }
        self.last_pos = Some(pos);
    }

    fn transport(&self, ui: &mut egui::Ui) {
        let p = ui.painter();
        let r = ui.max_rect();
        let font = Self::font();
        let e = &self.editor;
        let playing = self.controller.is_playing();
        let mut x = r.left() + 8.0;
        let y = r.center().y;
        let mut put = |text: String, color: Color32| {
            let rect = p.text(pos2(x, y), Align2::LEFT_CENTER, text, font.clone(), color);
            x = rect.right() + 18.0;
        };
        put("cowbell".into(), color::ACCENT);
        put(
            if playing {
                "▶ playing".into()
            } else {
                "■ stopped".into()
            },
            color::TEXT,
        );
        put(format!("{:.1} bpm", e.song.bpm), color::TEXT);
        put(
            format!(
                "order {}/{} · pat {:02X} · {} rows/beat",
                e.cursor.order + 1,
                e.song.order.len(),
                e.pattern_index(),
                e.pattern().rows_per_beat
            ),
            color::TEXT,
        );
        put(format!("step {}", e.edit_step), color::MUTED);
        put(
            format!("follow {}", if self.follow { "on" } else { "off" }),
            color::MUTED,
        );
        let (text, c) = match &self.output {
            Ok(o) => (format!("{} · {} Hz", o.device, o.sample_rate), color::DIM),
            Err(err) => (format!("no audio: {err}"), color::ERROR),
        };
        p.text(
            pos2(r.right() - 8.0, y),
            Align2::RIGHT_CENTER,
            text,
            font,
            c,
        );
    }

    fn slicer(&mut self, ui: &mut egui::Ui) {
        let track = self.editor.pattern().tracks.get(self.editor.cursor.track);
        let instrument = track.map_or(0, |t| t.instrument);
        let Some(Instrument::Sampler(sample)) = self.editor.song.instruments.get(instrument) else {
            return;
        };
        let sample = sample.clone();
        let font = Self::font();

        let header = ui.max_rect().with_max_y(ui.max_rect().top() + 18.0);
        let current = self
            .editor
            .cell()
            .trigger
            .map_or(self.editor.last_slice, |t| t.slice) as usize;
        ui.painter().text(
            header.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            format!(
                "slicer: {} · {} slices · slice {:02X} · click to audition, ctrl+click to enter",
                sample.name,
                sample.slices.len(),
                current
            ),
            font.clone(),
            color::MUTED,
        );

        let rect = Rect::from_min_max(
            header.left_bottom() + vec2(8.0, 2.0),
            ui.max_rect().right_bottom() - vec2(8.0, 6.0),
        );
        let response = ui.allocate_rect(rect, Sense::click());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, color::PANEL);
        let n = sample.frames.len().max(1) as f32;
        let x_of = |frame: usize| rect.left() + rect.width() * frame as f32 / n;

        if let Some((start, end)) = sample.slice_bounds(current) {
            let hl = Rect::from_x_y_ranges(x_of(start)..=x_of(end), rect.y_range());
            p.rect_filled(hl, 0.0, color::ACCENT.gamma_multiply(0.18));
        }
        let mid = rect.center().y;
        let half = rect.height() * 0.45;
        for (i, &(lo, hi)) in self
            .wave
            .get(&sample, rect.width().max(1.0) as usize)
            .iter()
            .enumerate()
        {
            let x = rect.left() + i as f32 + 0.5;
            p.line_segment(
                [pos2(x, mid - hi * half), pos2(x, mid - lo * half + 1.0)],
                Stroke::new(1.0, color::WAVE),
            );
        }
        let small = FontId::monospace(11.0);
        for (i, &start) in sample.slices.iter().enumerate() {
            let x = x_of(start);
            p.line_segment(
                [pos2(x, rect.top()), pos2(x, rect.bottom())],
                Stroke::new(1.0, color::ACCENT),
            );
            p.text(
                pos2(x + 3.0, rect.top() + 2.0),
                Align2::LEFT_TOP,
                format!("{i:02X}"),
                small.clone(),
                color::ACCENT,
            );
        }

        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let frame = ((pos.x - rect.left()) / rect.width() * n) as usize;
            let slice = sample
                .slices
                .partition_point(|&s| s <= frame)
                .saturating_sub(1) as u16;
            if ui.input(|i| i.modifiers.command) {
                self.editor.apply(Action::EnterSlice(slice));
            } else {
                self.editor.last_slice = slice;
                self.editor.audition = Some((instrument, slice));
            }
        }
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let p = ui.painter_at(rect);
        let font = Self::font();
        let cw = self.char_width;
        let e = &self.editor;
        let pattern = e.pattern();
        let rpb = pattern.rows_per_beat.max(1) as usize;
        let track_x = |t: usize| {
            rect.left() + 8.0 + cw * (ROW_NUMBER_CHARS + t * (TRACK_CHARS + TRACK_GAP)) as f32
        };

        // Track names.
        for (t, track) in pattern.tracks.iter().enumerate() {
            let c = color::TRACKS[t % color::TRACKS.len()];
            p.text(
                pos2(track_x(t), rect.top() + 2.0),
                Align2::LEFT_TOP,
                &track.name,
                font.clone(),
                c,
            );
        }

        let top = rect.top() + ROW_HEIGHT + 6.0;
        let visible = ((rect.bottom() - top) / ROW_HEIGHT).floor().max(1.0) as isize;
        // Scroll only when the cursor gets near an edge, and never past the
        // end of the pattern, so short patterns are shown whole.
        let (rows, visible_rows) = (pattern.rows, visible as usize);
        let margin = (visible_rows / 4).min(4);
        let row = e.cursor.row;
        let mut scroll = self.scroll;
        if row < scroll + margin {
            scroll = row.saturating_sub(margin);
        } else if row + margin >= scroll + visible_rows {
            scroll = row + margin + 1 - visible_rows;
        }
        scroll = scroll.min(rows.saturating_sub(visible_rows));
        self.scroll = scroll;
        let first = scroll as isize;
        let playhead = self
            .controller
            .is_playing()
            .then(|| self.controller.position());
        let selection = e.selection();
        let width = track_x(pattern.tracks.len()) - rect.left();

        for i in 0..visible {
            let row = first + i;
            if row < 0 || row as usize >= pattern.rows {
                continue;
            }
            let row = row as usize;
            let y = top + i as f32 * ROW_HEIGHT;
            let band = Rect::from_min_size(pos2(rect.left(), y), vec2(width, ROW_HEIGHT));
            let bg = if row == e.cursor.row {
                Some(color::CURSOR_ROW)
            } else if row.is_multiple_of(rpb * 4) {
                Some(color::BAR)
            } else if row.is_multiple_of(rpb) {
                Some(color::BEAT)
            } else {
                None
            };
            if let Some(bg) = bg {
                p.rect_filled(band, 0.0, bg);
            }
            if playhead.is_some_and(|pos| {
                pos.row == row && e.song.order.get(pos.order) == Some(&e.pattern_index())
            }) {
                p.rect_filled(band.with_max_x(band.left() + 4.0), 0.0, color::ACCENT);
            }
            let number_color = if row.is_multiple_of(rpb) {
                color::MUTED
            } else {
                color::DIM
            };
            p.text(
                pos2(rect.left() + 8.0, y + 1.0),
                Align2::LEFT_TOP,
                format!("{row:02X}"),
                font.clone(),
                number_color,
            );

            for (t, track) in pattern.tracks.iter().enumerate() {
                let x = track_x(t);
                if t == e.cursor.track && selection.as_ref().is_some_and(|s| s.contains(&row)) {
                    let sel = Rect::from_min_size(
                        pos2(x - cw * 0.5, y),
                        vec2(cw * (TRACK_CHARS as f32 + 1.0), ROW_HEIGHT),
                    );
                    p.rect_filled(sel, 0.0, color::SELECTION);
                }
                let track_color = color::TRACKS[t % color::TRACKS.len()];
                for (field, text, c) in cell_fields(&track.cells[row], track_color) {
                    let pos = pos2(x + cw * field_offset(field) as f32, y + 1.0);
                    p.text(pos, Align2::LEFT_TOP, text, font.clone(), c);
                }
            }
        }

        // The cursor digit, drawn last so it sits on top.
        let c = e.cursor;
        let i = c.row as isize - first;
        if (0..visible).contains(&i) && c.track < pattern.tracks.len() {
            let x = track_x(c.track) + cw * (field_offset(c.field) + c.digit) as f32;
            let y = top + i as f32 * ROW_HEIGHT;
            let cell = Rect::from_min_size(pos2(x, y), vec2(cw, ROW_HEIGHT));
            p.rect_filled(cell, 1.0, color::ACCENT);
            let ch = cell_fields(&pattern.tracks[c.track].cells[c.row], color::BG)
                .into_iter()
                .find(|(f, ..)| *f == c.field)
                .and_then(|(_, text, _)| text.chars().nth(c.digit))
                .unwrap_or(' ');
            p.text(pos2(x, y + 1.0), Align2::LEFT_TOP, ch, font, color::BG);
        }
    }

    fn status(&self, ui: &mut egui::Ui) {
        let p = ui.painter();
        let r = ui.max_rect();
        let font = Self::font();
        let e = &self.editor;
        let cell = e.cell();
        let name = e
            .pattern()
            .tracks
            .get(e.cursor.track)
            .map_or("", |t| t.name.as_str());
        let mut words = vec![format!("row {:02X}", e.cursor.row), name.to_owned()];
        match cell.trigger {
            Some(t) => words.push(format!(
                "slice {:02X}, velocity {:02X}",
                t.slice, t.velocity
            )),
            None => words.push("empty".into()),
        }
        words.extend(cell.fx().map(|fx| fx.describe()));
        p.text(
            r.left_top() + vec2(8.0, 4.0),
            Align2::LEFT_TOP,
            words.join(" · "),
            font.clone(),
            color::TEXT,
        );

        let hints = [
            ("0-f", "enter"),
            ("ORVPBSC", "effect"),
            ("space", "play/stop"),
            ("shift+↕", "select"),
            ("ctrl+r", "roll"),
            ("ctrl+z", "undo"),
            ("ctrl+c/v", "copy/paste"),
            ("ctrl+←→", "order"),
            ("ctrl+↕", "step"),
            ("alt+↕", "bpm"),
            ("ctrl+t", "add track"),
        ];
        let mut x = r.left() + 8.0;
        let mut y = r.top() + 24.0;
        for (key, what) in hints {
            let width = (key.chars().count() + what.chars().count()) as f32 * self.char_width + 5.0;
            if x + width > r.right() - 8.0 {
                x = r.left() + 8.0;
                y += ROW_HEIGHT;
            }
            let k = p.text(
                pos2(x, y),
                Align2::LEFT_TOP,
                key,
                font.clone(),
                color::ACCENT,
            );
            let w = p.text(
                pos2(k.right() + 5.0, y),
                Align2::LEFT_TOP,
                what,
                font.clone(),
                color::MUTED,
            );
            x = w.right() + 16.0;
        }
    }
}

/// The text of each field of a cell, with its color.
fn cell_fields(cell: &Cell, track_color: Color32) -> Vec<(Field, String, Color32)> {
    let mut out = Vec::with_capacity(4);
    match cell.trigger {
        Some(t) => {
            out.push((
                Field::Slice,
                format!("{:02X}", t.slice.min(0xff)),
                track_color,
            ));
            out.push((Field::Velocity, format!("{:02X}", t.velocity), color::TEXT));
        }
        None => {
            out.push((Field::Slice, "··".into(), color::DIM));
            out.push((Field::Velocity, "··".into(), color::DIM));
        }
    }
    for (i, fx) in cell.fx.iter().enumerate() {
        out.push(match fx {
            Some(fx) => (
                Field::Fx(i),
                format!("{}{:02X}", fx.letter(), fx.param()),
                color::ACCENT,
            ),
            None => (Field::Fx(i), "···".into(), color::DIM),
        });
    }
    out
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_visuals(egui::Visuals::dark());
        self.char_width = ctx.fonts_mut(|f| f.glyph_width(&Self::font(), '0'));
        self.handle_input(&ctx);
        self.sync_engine();

        let frame = |fill| egui::Frame::new().fill(fill);
        egui::Panel::top("transport")
            .exact_size(30.0)
            .frame(frame(color::BG))
            .show(ui, |ui| self.transport(ui));
        egui::Panel::top("slicer")
            .exact_size(130.0)
            .frame(frame(color::BG))
            .show(ui, |ui| self.slicer(ui));
        egui::Panel::bottom("status")
            .exact_size(64.0)
            .frame(frame(color::PANEL))
            .show(ui, |ui| self.status(ui));
        egui::CentralPanel::default()
            .frame(frame(color::BG))
            .show(ui, |ui| self.grid(ui));

        if self.controller.is_playing() {
            ctx.request_repaint();
        }
        #[cfg(feature = "screenshot")]
        if let Some(s) = &mut self.screenshot {
            s.update(&ctx);
        }
    }
}
