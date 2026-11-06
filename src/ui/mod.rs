//! The main window: a transport bar with the File and Edit menus, then the
//! arrangement (tracks and their clips on a timeline) or the rack, and a
//! status line. Everything is done with the mouse.

mod arrange;
mod knob;
mod rack;
#[cfg(feature = "screenshot")]
mod screenshot;

use std::ops::Range;
use std::path::{Path, PathBuf};

use eframe::egui::{self, Align2, Color32, FontId, Key, RichText, vec2};

use crate::audio::{self, Output};
use crate::document::Document;
use crate::engine::{Command, Controller, Engine};
use crate::graph::Graph;
use crate::project;
use crate::song::{Song, TICKS_PER_BAR, TRACK_VOLUME, Target, Tick};
use arrange::Edit;

mod color {
    use eframe::egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(0x16, 0x17, 0x1a);
    pub const PANEL: Color32 = Color32::from_rgb(0x1d, 0x1e, 0x22);
    pub const BEAT: Color32 = Color32::from_rgb(0x1b, 0x1c, 0x20);
    pub const BAR: Color32 = Color32::from_rgb(0x22, 0x23, 0x28);
    pub const CURSOR_ROW: Color32 = Color32::from_rgb(0x2c, 0x2d, 0x33);
    pub const TEXT: Color32 = Color32::from_rgb(0xc9, 0xc6, 0xbd);
    pub const DIM: Color32 = Color32::from_rgb(0x5f, 0x5e, 0x5a);
    pub const MUTED: Color32 = Color32::from_rgb(0x88, 0x87, 0x80);
    pub const ACCENT: Color32 = Color32::from_rgb(0xef, 0x9f, 0x27);
    pub const ERROR: Color32 = Color32::from_rgb(0xf0, 0x95, 0x95);
    pub const TRACKS: [Color32; 4] = [
        Color32::from_rgb(0x5d, 0xca, 0xa5),
        Color32::from_rgb(0xed, 0x93, 0xb1),
        Color32::from_rgb(0xaf, 0xa9, 0xec),
        Color32::from_rgb(0x85, 0xb7, 0xeb),
    ];
}

/// Open the window. `path` is where the project is saved; it needn't exist
/// yet.
pub fn run(song: Song, path: Option<PathBuf>) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // The size to restore to when un-maximized.
            .with_inner_size([1100.0, 720.0])
            .with_maximized(true)
            .with_title("cowbell"),
        ..Default::default()
    };
    eframe::run_native(
        "cowbell",
        options,
        Box::new(|_cc| Ok(Box::new(App::new(song, path)))),
    )
    .map_err(|e| e.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Arrange,
    Rack,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptKind {
    Open,
    SaveAs,
    Export,
}

struct Prompt {
    kind: PromptKind,
    text: String,
    /// Focus the text field on the first frame only.
    focused: bool,
}

/// A project path from typed text: `.cowbell` is added if missing.
fn project_path(text: &str) -> PathBuf {
    let mut path = typed_path(text);
    if path.extension().is_none_or(|e| e != project::EXTENSION) {
        let mut name = path.into_os_string();
        name.push(".");
        name.push(project::EXTENSION);
        path = name.into();
    }
    path
}

/// Typed text as a path: trimmed, and `~/` is the home folder.
fn typed_path(text: &str) -> PathBuf {
    let text = text.trim();
    match text.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map_or_else(|| PathBuf::from(text), |h| Path::new(&h).join(rest)),
        None => PathBuf::from(text),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Icon {
    ToStart,
    Play,
    Stop,
    Loop,
}

/// A transport button with a drawn icon (the default fonts have no
/// transport symbols). `on` lights it up.
fn icon_button(ui: &mut egui::Ui, icon: Icon, on: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(40.0, 28.0), egui::Sense::click());
    let visuals = ui.style().interact_selectable(&response, on);
    let fill = if on { color::ACCENT } else { visuals.bg_fill };
    let ink = if on { color::BG } else { color::TEXT };
    let p = ui.painter();
    p.rect_filled(rect, 4.0, fill);
    let c = rect.center();
    let stroke = egui::Stroke::new(2.0, ink);
    match icon {
        Icon::ToStart => {
            p.line_segment([c + vec2(-6.0, -6.0), c + vec2(-6.0, 6.0)], stroke);
            let tri = vec![c + vec2(6.0, -6.0), c + vec2(6.0, 6.0), c + vec2(-4.0, 0.0)];
            p.add(egui::Shape::convex_polygon(tri, ink, egui::Stroke::NONE));
        }
        Icon::Play => {
            let tri = vec![
                c + vec2(-5.0, -7.0),
                c + vec2(7.0, 0.0),
                c + vec2(-5.0, 7.0),
            ];
            p.add(egui::Shape::convex_polygon(tri, ink, egui::Stroke::NONE));
        }
        Icon::Stop => {
            p.rect_filled(egui::Rect::from_center_size(c, vec2(11.0, 11.0)), 1.0, ink);
        }
        Icon::Loop => {
            // Two arrows chasing each other round a rounded box.
            let r = egui::Rect::from_center_size(c, vec2(16.0, 10.0));
            p.rect_stroke(r, 4.0, stroke, egui::StrokeKind::Middle);
            let gap = egui::Rect::from_center_size(r.center_top(), vec2(5.0, 4.0));
            p.rect_filled(gap, 0.0, fill);
            let tip = r.center_top() + vec2(2.5, 0.0);
            let head = vec![
                tip + vec2(-4.0, -4.0),
                tip + vec2(-4.0, 4.0),
                tip + vec2(1.0, 0.0),
            ];
            p.add(egui::Shape::convex_polygon(head, ink, egui::Stroke::NONE));
        }
    }
    response
}

/// What to select after an edit.
#[derive(Debug, PartialEq)]
enum Select {
    Keep,
    Clip(crate::song::ClipRef),
    Track(usize),
}

/// Make an arrangement edit on `song`, or say why it can't be done.
fn apply(song: &mut Song, edit: &Edit) -> Result<Select, String> {
    /// A new clip: a bar of sixteenths, or what fits.
    const NEW_RPB: u32 = 4;
    const NEW_ROWS: usize = 16;
    let full = || format!("can't: {} tracks is the most", crate::engine::MAX_TRACKS);
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
            // The one that took its place; `select` keeps it in range.
            Ok(Select::Track(t))
        }
        Edit::RenameTrack(t, ref name) => {
            let track = song.tracks.get_mut(t).ok_or("no such track")?;
            track.name = name.clone();
            Ok(Select::Keep)
        }
        Edit::Volume(t, v) => song
            .set_volume(t, v)
            .map(|_| Select::Keep)
            .ok_or_else(|| "no such track".to_owned()),
        Edit::Mute(t) | Edit::Solo(t) => {
            let track = song.tracks.get_mut(t).ok_or("no such track")?;
            if matches!(edit, Edit::Mute(_)) {
                track.mute ^= true;
            } else {
                track.solo ^= true;
            }
            Ok(Select::Keep)
        }
    }
}

struct App {
    doc: Document,
    controller: Controller,
    output: Result<Output, String>,
    sample_rate: u32,
    /// The rack version the engine's graph was built from.
    rack_version: u64,
    /// `doc.revision` the engine's song is from.
    sent_revision: u64,
    /// `doc.revision` when last saved or opened.
    saved_revision: u64,
    /// Where the project is saved, once it has been.
    path: Option<PathBuf>,
    view: View,
    arrange: arrange::ArrangeView,
    rack_view: rack::RackView,
    /// Keep the playhead in view while playing.
    follow: bool,
    /// The loop range to bring back when the loop is switched on again.
    loop_memory: Range<Tick>,
    prompt: Option<Prompt>,
    message: Option<String>,
    /// Set after a close was cancelled because of unsaved changes; a second
    /// close goes through.
    close_armed: bool,
    title: String,
    /// The song as it was when the current drag in the arrangement
    /// started, and whether the drag has changed anything yet.
    gesture: Option<(Song, bool)>,
    #[cfg(feature = "screenshot")]
    screenshot: Option<screenshot::Screenshot>,
}

impl App {
    fn new(song: Song, path: Option<PathBuf>) -> Self {
        let (output, controller) = match audio::start(song.clone()) {
            Ok((output, controller)) => (Ok(output), controller),
            // Keep going without sound. The engine is dropped, so commands
            // just pile up in the queue until it's full.
            Err(e) => (Err(e), Engine::new(48_000, song.clone()).1),
        };
        let sample_rate = output.as_ref().map_or(48_000, |o| o.sample_rate);
        let rack_version = song.rack.version;
        let loop_memory = song.loop_range.clone().unwrap_or(0..4 * TICKS_PER_BAR);
        #[cfg(feature = "screenshot")]
        let view = match std::env::var("COWBELL_VIEW").as_deref() {
            Ok("rack") => View::Rack,
            _ => View::Arrange,
        };
        #[cfg(not(feature = "screenshot"))]
        let view = View::Arrange;
        App {
            doc: Document::new(song),
            controller,
            output,
            sample_rate,
            rack_version,
            sent_revision: 0,
            saved_revision: 0,
            path,
            view,
            arrange: arrange::ArrangeView::default(),
            rack_view: rack::RackView::default(),
            follow: true,
            loop_memory,
            prompt: None,
            message: None,
            close_armed: false,
            title: String::new(),
            gesture: None,
            #[cfg(feature = "screenshot")]
            screenshot: screenshot::Screenshot::from_env(),
        }
    }

    /// The project's file name, with a star when there are unsaved changes.
    fn project_name(&self) -> String {
        let name = self.path.as_ref().and_then(|p| p.file_name()).map_or_else(
            || "untitled".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        if self.unsaved() {
            format!("{name} *")
        } else {
            name
        }
    }

    fn unsaved(&self) -> bool {
        self.doc.revision != self.saved_revision
    }

    fn say(&mut self, message: impl Into<String>) {
        self.message = Some(message.into());
    }

    fn save(&mut self, path: PathBuf) {
        match project::save(&self.doc.song, &path) {
            Ok(()) => {
                self.saved_revision = self.doc.revision;
                self.close_armed = false;
                self.say(format!("saved {}", path.display()));
                self.path = Some(path);
            }
            Err(e) => self.say(format!("couldn't save: {e}")),
        }
    }

    fn open(&mut self, path: PathBuf) {
        match project::load(&path) {
            Ok(song) => {
                let _ = self.controller.send(Command::Stop);
                let _ = self.controller.send(Command::Locate(0));
                self.loop_memory = song.loop_range.clone().unwrap_or(0..4 * TICKS_PER_BAR);
                self.doc = Document::new(song);
                // Resend the song and rebuild the graph, whatever revision
                // and rack version the new document starts at.
                self.sent_revision = u64::MAX;
                self.rack_version = self.doc.song.rack.version.wrapping_add(1);
                self.saved_revision = self.doc.revision;
                self.close_armed = false;
                self.arrange = arrange::ArrangeView::default();
                self.say(format!("opened {}", path.display()));
                self.path = Some(path);
            }
            Err(e) => self.say(format!("couldn't open: {e}")),
        }
    }

    fn export(&mut self, path: PathBuf) {
        let message = match crate::export::export_wav(&self.doc.song, &path) {
            Ok(seconds) => format!("exported {seconds:.1}s to {}", path.display()),
            Err(e) => format!("couldn't export: {e}"),
        };
        self.say(message);
    }

    fn ask(&mut self, kind: PromptKind) {
        let text = match (&self.path, kind) {
            (Some(p), PromptKind::Export) => p.with_extension("wav").display().to_string(),
            (None, PromptKind::Export) => "untitled.wav".into(),
            (Some(p), _) => p.display().to_string(),
            (None, PromptKind::SaveAs) => format!("untitled.{}", project::EXTENSION),
            (None, PromptKind::Open) => String::new(),
        };
        self.prompt = Some(Prompt {
            kind,
            text,
            focused: false,
        });
    }

    fn undo(&mut self, redo: bool) {
        let done = if redo {
            self.doc.redo()
        } else {
            self.doc.undo()
        };
        if done {
            // Knob turns don't bump the rack version, so rebuild the graph
            // to be sure it has the restored settings.
            self.rack_version = self.doc.song.rack.version.wrapping_add(1);
            self.select(Select::Keep);
        }
    }

    /// Toggle `target` as the selected track's automation target.
    fn bind_automation(&mut self, target: Target) {
        let t = self.arrange.selected_track;
        let Some(track) = self.doc.song.tracks.get(t) else {
            return;
        };
        let name = track.name.clone();
        let unbind = track.automation == Some(target);
        self.doc
            .edit(|s| s.tracks[t].automation = (!unbind).then_some(target));
        if unbind {
            self.say(format!("{name}: K effects turn nothing now"));
        } else {
            let knob = match target {
                Target::Rack { device, param } => self.doc.song.rack.device(device).and_then(|d| {
                    let spec = d.kind.params().get(param)?;
                    Some(format!("{} {}", d.kind.name(), spec.name))
                }),
                Target::Synth { .. } => None,
            };
            self.say(format!(
                "{name}: K effects turn {}",
                knob.unwrap_or_default()
            ));
        }
    }

    fn set_loop(&mut self, on: bool) {
        let range = on.then(|| self.loop_memory.clone());
        if self.doc.song.loop_range != range {
            self.doc.song.loop_range = range;
            self.doc.changed();
        }
    }

    fn play_or_stop(&mut self) {
        let cmd = if self.controller.is_playing() {
            Command::Stop
        } else {
            Command::Play
        };
        let _ = self.controller.send(cmd);
    }

    fn sync_engine(&mut self) {
        let rack = &self.doc.song.rack;
        if rack.version != self.rack_version {
            let graph = Box::new(Graph::build(rack, self.sample_rate));
            if self.controller.send(Command::SwapGraph(graph)).is_ok() {
                self.rack_version = rack.version;
            }
        }
        if self.doc.revision != self.sent_revision {
            let song = Box::new(self.doc.song.clone());
            if self.controller.send(Command::SwapSong(song)).is_ok() {
                self.sent_revision = self.doc.revision;
            }
        }
        self.controller.collect_garbage();
    }

    fn menus(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("File", |ui| {
            if ui.button("Open…").clicked() {
                self.ask(PromptKind::Open);
            }
            if ui.button("Save").clicked() {
                match self.path.clone() {
                    Some(path) => self.save(path),
                    None => self.ask(PromptKind::SaveAs),
                }
            }
            if ui.button("Save as…").clicked() {
                self.ask(PromptKind::SaveAs);
            }
            ui.separator();
            if ui.button("Export WAV…").clicked() {
                self.ask(PromptKind::Export);
            }
        });
        ui.menu_button("Edit", |ui| {
            if ui
                .add_enabled(self.doc.can_undo(), egui::Button::new("Undo"))
                .clicked()
            {
                self.undo(false);
            }
            if ui
                .add_enabled(self.doc.can_redo(), egui::Button::new("Redo"))
                .clicked()
            {
                self.undo(true);
            }
        });
    }

    fn transport(&mut self, ui: &mut egui::Ui) {
        let playing = self.controller.is_playing();
        let pos = self.controller.position();
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            self.menus(ui);
            ui.add_space(16.0);

            let to_start = self.doc.song.loop_range.as_ref().map_or(0, |r| r.start);
            if icon_button(ui, Icon::ToStart, false)
                .on_hover_text("Back to the start")
                .clicked()
            {
                let _ = self.controller.send(Command::Locate(to_start));
            }
            let play = if playing { Icon::Stop } else { Icon::Play };
            if icon_button(ui, play, playing)
                .on_hover_text(if playing {
                    "Stop (space)"
                } else {
                    "Play (space)"
                })
                .clicked()
            {
                self.play_or_stop();
            }
            let looping = self.doc.song.loop_range.is_some();
            if icon_button(ui, Icon::Loop, looping)
                .on_hover_text("Loop (drag along the ruler to set where)")
                .clicked()
            {
                self.set_loop(!looping);
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new(arrange::position_text(pos))
                    .monospace()
                    .size(18.0)
                    .color(color::ACCENT),
            );
            ui.add_space(8.0);
            let mut bpm = self.doc.song.bpm;
            let r = ui.add(
                egui::DragValue::new(&mut bpm)
                    .range(20.0..=400.0)
                    .speed(0.2)
                    .fixed_decimals(1)
                    .suffix(" bpm"),
            );
            if r.changed() {
                self.doc.song.bpm = bpm;
                self.doc.changed();
            }
            let snap = arrange::SNAPS
                .iter()
                .find(|(t, _)| *t == self.arrange.snap)
                .map_or("Beat", |(_, name)| name);
            egui::ComboBox::from_id_salt("snap")
                .selected_text(format!("Snap: {snap}"))
                .show_ui(ui, |ui| {
                    for (ticks, name) in arrange::SNAPS {
                        ui.selectable_value(&mut self.arrange.snap, ticks, name);
                    }
                });
            ui.checkbox(&mut self.follow, "Follow");
            ui.add_space(16.0);
            ui.selectable_value(&mut self.view, View::Arrange, "Arrange");
            ui.selectable_value(&mut self.view, View::Rack, "Rack");
            ui.add_space(16.0);
            ui.label(RichText::new(self.project_name()).color(color::MUTED));
        });
    }

    fn status(&self, ui: &mut egui::Ui) {
        let p = ui.painter();
        let r = ui.max_rect();
        let font = FontId::proportional(13.0);
        let (text, c) = match (&self.message, &self.output, &self.rack_view.message) {
            (Some(m), ..) => {
                let failed = m.starts_with("couldn't") || m.starts_with("can't");
                (m.clone(), if failed { color::ERROR } else { color::ACCENT })
            }
            (None, Err(e), _) => (format!("no sound: {e}"), color::ERROR),
            (None, Ok(_), Some(m)) if self.view == View::Rack => (m.clone(), color::ERROR),
            _ => (String::new(), color::TEXT),
        };
        p.text(
            r.left_center() + vec2(10.0, 0.0),
            Align2::LEFT_CENTER,
            text,
            font.clone(),
            c,
        );
        let hints = match self.view {
            View::Arrange => {
                "space plays and stops · drag clips to move them, their right edge to resize · double-click an empty spot for a new clip · right-click clips and tracks for more · drag empty space to move around · ctrl+scroll to zoom"
            }
            View::Rack => {
                "drag between jacks to patch · drag a plug out to unplug · drag or scroll knobs (shift: fine) · click switches · double-click resets · right-click a knob to automate it"
            }
        };
        p.text(
            r.right_center() - vec2(10.0, 0.0),
            Align2::RIGHT_CENTER,
            hints,
            font,
            color::MUTED,
        );
    }

    fn arrange(&mut self, ui: &mut egui::Ui) {
        let playing = self.controller.is_playing();
        let pos = self.controller.position();
        let out = self
            .arrange
            .show(ui, &self.doc.song, pos, self.follow && playing);
        if let Some(tick) = out.locate {
            let _ = self.controller.send(Command::Locate(tick));
        }
        if let Some(range) = out.loop_range {
            self.loop_memory = range;
            self.set_loop(true);
        }
        for edit in out.edits {
            self.edit(edit);
        }
        if let Some(edit) = out.live {
            self.live_edit(edit);
        }
        if out.gesture_done
            && let Some((before, changed)) = self.gesture.take()
            && changed
        {
            self.doc.record(before);
        }
        if playing || ui.input(|i| i.pointer.any_down()) {
            ui.ctx().request_repaint();
        }
    }

    /// Make an edit from the arrangement as one undo step.
    fn edit(&mut self, edit: Edit) {
        // Mute and solo are mixing, not editing: no undo step.
        match edit {
            Edit::Mute(t) | Edit::Solo(t) if t < self.doc.song.tracks.len() => {
                let track = &mut self.doc.song.tracks[t];
                if matches!(edit, Edit::Mute(_)) {
                    track.mute ^= true;
                } else {
                    track.solo ^= true;
                }
                self.doc.changed();
                return;
            }
            _ => {}
        }
        let before = self.doc.song.clone();
        match apply(&mut self.doc.song, &edit) {
            Ok(select) => {
                self.doc.record(before);
                self.select(select);
                self.volume_to_engine(&edit);
            }
            Err(why) => {
                self.doc.song = before;
                self.say(why);
            }
        }
    }

    /// One step of a drag: made on the song as it was when the drag
    /// started. A step that can't be made (a clip dragged onto another)
    /// leaves the song as the last one that could.
    fn live_edit(&mut self, edit: Edit) {
        let (before, changed) = self
            .gesture
            .get_or_insert_with(|| (self.doc.song.clone(), false));
        let mut song = before.clone();
        if let Ok(select) = apply(&mut song, &edit) {
            *changed = true;
            self.doc.song = song;
            self.doc.changed();
            self.select(select);
            self.volume_to_engine(&edit);
        }
    }

    fn select(&mut self, select: Select) {
        let a = &mut self.arrange;
        match select {
            Select::Keep => {}
            Select::Clip(c) => {
                a.selected_track = c.0;
                a.selected_clip = Some(c);
            }
            Select::Track(t) => {
                a.selected_track = t;
                a.selected_clip = None;
            }
        }
        let tracks = self.doc.song.tracks.len();
        a.selected_track = a.selected_track.min(tracks.saturating_sub(1));
        if a.selected_clip
            .is_some_and(|c| self.doc.song.clip(c).is_none())
        {
            a.selected_clip = None;
        }
    }

    /// Volume changes go straight to the engine, like knobs in the rack.
    fn volume_to_engine(&mut self, edit: &Edit) {
        if let Edit::Volume(t, value) = *edit
            && let Some(d) = self.doc.song.rack.track_device(t)
        {
            let _ = self.controller.send(Command::SetParam {
                device: d.id,
                param: TRACK_VOLUME,
                value,
            });
        }
    }

    fn rack(&mut self, ui: &mut egui::Ui) {
        let song = &mut self.doc.song;
        let names: Vec<String> = song.tracks.iter().map(|t| t.name.clone()).collect();
        let automated = match song.automation(self.arrange.selected_track) {
            Some(Target::Rack { device, param }) => Some((device, param)),
            _ => None,
        };
        let out = self.rack_view.show(ui, &mut song.rack, &names, automated);
        for (device, param, value) in out.params {
            let _ = self.controller.send(Command::SetParam {
                device,
                param,
                value,
            });
        }
        if let Some(before) = out.undo {
            self.doc.record_rack(before);
        }
        if let Some((device, param)) = out.bind {
            self.bind_automation(Target::Rack { device, param });
        }
        // Cables follow the pointer while dragging.
        if ui.input(|i| i.pointer.any_down()) {
            ui.ctx().request_repaint();
        }
    }

    fn prompt(&mut self, ctx: &egui::Context) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let title = match prompt.kind {
            PromptKind::Open => "Open project",
            PromptKind::SaveAs => "Save project as",
            PromptKind::Export => "Export the song to WAV",
        };
        let (mut confirm, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::unique("prompt")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(title);
            ui.add_space(6.0);
            let edit =
                ui.add(egui::TextEdit::singleline(&mut prompt.text).desired_width(f32::INFINITY));
            if !prompt.focused {
                edit.request_focus();
                prompt.focused = true;
            }
            if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                confirm = true;
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                confirm |= ui.button("OK").clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
        cancel |= modal.should_close();
        if confirm {
            let prompt = self.prompt.take().unwrap();
            match prompt.kind {
                PromptKind::Open => self.open(project_path(&prompt.text)),
                PromptKind::SaveAs => self.save(project_path(&prompt.text)),
                PromptKind::Export => self.export(typed_path(&prompt.text)),
            }
        } else if cancel {
            self.prompt = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_visuals(egui::Visuals::dark());
        // A scripted screenshot mustn't pick up whatever the person at the
        // computer happens to do while the window is up.
        #[cfg(feature = "screenshot")]
        if self.screenshot.is_some() {
            ctx.input_mut(|i| {
                i.events.clear();
                i.pointer = Default::default();
            });
        }
        // Space plays and stops, unless it's being typed into a dialog. It's
        // taken here, before any widget sees it, so a button that has focus
        // from a click doesn't press itself again.
        if self.prompt.is_none()
            && !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Space))
        {
            self.play_or_stop();
        }
        self.sync_engine();

        let title = format!("{} - cowbell", self.project_name());
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.unsaved() && !self.close_armed {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_armed = true;
            self.say(
                "couldn't close: unsaved changes. Save from the File menu, or close again to discard them",
            );
        }

        let frame = |fill: Color32| egui::Frame::new().fill(fill);
        egui::Panel::top("transport")
            .exact_size(44.0)
            .frame(frame(color::PANEL).inner_margin(egui::Margin::symmetric(10, 0)))
            .show(ui, |ui| self.transport(ui));
        egui::Panel::bottom("status")
            .exact_size(28.0)
            .frame(frame(color::PANEL))
            .show(ui, |ui| self.status(ui));
        egui::CentralPanel::default()
            .frame(frame(color::BG))
            .show(ui, |ui| match self.view {
                View::Arrange => self.arrange(ui),
                View::Rack => self.rack(ui),
            });
        self.prompt(&ctx);

        #[cfg(feature = "screenshot")]
        if let Some(s) = &mut self.screenshot {
            s.update(&ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song::{Clip, Track};

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
        assert_eq!(
            apply(
                &mut s,
                &Edit::MoveClip {
                    clip: (0, 1),
                    track: 1,
                    start: 768
                }
            ),
            Ok(Select::Clip((1, 1)))
        );
        assert_eq!(apply(&mut s, &Edit::DeleteTrack(0)), Ok(Select::Track(0)));
        assert_eq!(s.tracks[0].name, "b");
        assert_eq!(apply(&mut s, &Edit::AddTrack(0)), Ok(Select::Track(1)));
        assert_eq!(s.tracks[1].name, "track", "no instrument to name it after");
    }

    #[test]
    fn edits_that_cant_be_made_say_why() {
        let mut s = song();
        let e = apply(
            &mut s,
            &Edit::MoveClip {
                clip: (0, 0),
                track: 1,
                start: 0,
            },
        );
        assert_eq!(e, Err("no room there".to_owned()));
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
    }

    #[test]
    fn volume_and_rename() {
        let mut s = song();
        apply(&mut s, &Edit::Volume(1, 0.25)).unwrap();
        assert_eq!(s.volume(1), 0.25);
        apply(&mut s, &Edit::RenameTrack(0, "kick".into())).unwrap();
        assert_eq!(s.tracks[0].name, "kick");
    }
}
