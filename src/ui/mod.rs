//! The main window: a transport bar with the File and Edit menus, then the
//! arrangement (tracks and their clips on a timeline) or the rack, and a
//! status line. Everything is done with the mouse.

mod arrange;
mod browser;
mod devices;
mod edit;
mod files;
mod knob;
mod rack;
#[cfg(feature = "screenshot")]
mod screenshot;
mod steps;

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, FontId, Key, RichText, vec2};

use crate::audio::{self, Output};
use crate::document::Document;
use crate::engine::{Command, Controller, Engine};
use crate::graph::Graph;
use crate::project;
use crate::sample::Sample;
use crate::song::{Instrument, Song, TICKS_PER_BAR, TRACK_VOLUME, Target, Tick, Track};
use crate::synthvoice::SynthPatch;
use edit::{Edit, Select, apply};

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

/// The bottom panel's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Clip,
    Devices,
}

/// What to do once unsaved changes have been let go of.
#[derive(Debug, PartialEq)]
enum Discard {
    New,
    Open(PathBuf),
}

/// Slice detection for samples added from the browser.
const SENSITIVITY: f32 = 0.6;
const MIN_SLICE_MS: f32 = 40.0;

/// A new, empty project: the drum kit on one poly track, so its hits ring
/// out over each other.
fn new_song() -> Song {
    let kit = Instrument::Sampler(Arc::new(crate::demo::kit()));
    let mut drums = Track::new("drums", 0);
    drums.poly = true;
    Song::new(crate::demo::BPM, vec![kit], vec![drums])
}

/// Where the browser starts: the built-in samples when running from the
/// source tree, else the project's folder, else the current one.
fn samples_folder(project: Option<&Path>) -> PathBuf {
    let assets = Path::new("assets/drums");
    if assets.is_dir() {
        return assets.to_owned();
    }
    project
        .and_then(Path::parent)
        .filter(|d| !d.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_owned)
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
    /// The bottom panel: which tab, and the views on it.
    tab: Tab,
    steps: steps::StepView,
    devices: devices::DevicesView,
    /// Keep the playhead in view while playing.
    follow: bool,
    /// The loop range to bring back when the loop is switched on again.
    loop_memory: Range<Tick>,
    browser: browser::Browser,
    dialog: Option<files::Dialog>,
    /// Asking whether to drop unsaved changes, and what for.
    confirm: Option<Discard>,
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
        #[allow(unused_mut)]
        let mut app = App {
            doc: Document::new(song),
            controller,
            output,
            sample_rate,
            rack_version,
            sent_revision: 0,
            saved_revision: 0,
            path: path.clone(),
            view,
            arrange: arrange::ArrangeView::default(),
            rack_view: rack::RackView::default(),
            tab: Tab::Clip,
            steps: steps::StepView::default(),
            devices: devices::DevicesView::default(),
            follow: true,
            loop_memory,
            browser: browser::Browser::new(&samples_folder(path.as_deref())),
            dialog: None,
            confirm: None,
            message: None,
            close_armed: false,
            title: String::new(),
            gesture: None,
            #[cfg(feature = "screenshot")]
            screenshot: screenshot::Screenshot::from_env(),
        };
        #[cfg(feature = "screenshot")]
        app.screenshot_setup();
        app
    }

    /// Dev only: `COWBELL_SELECT=track,clip` selects a clip,
    /// `COWBELL_TAB=devices` shows the devices tab, and
    /// `COWBELL_DIALOG=open|save|export` opens a file dialog.
    #[cfg(feature = "screenshot")]
    fn screenshot_setup(&mut self) {
        if let Ok(sel) = std::env::var("COWBELL_SELECT")
            && let Some((t, c)) = sel.split_once(',')
            && let (Ok(t), Ok(c)) = (t.parse(), c.parse())
        {
            self.select(Select::Clip((t, c)));
        }
        if std::env::var("COWBELL_TAB").as_deref() == Ok("devices") {
            self.tab = Tab::Devices;
        }
        match std::env::var("COWBELL_DIALOG").as_deref() {
            Ok("open") => self.ask(files::Purpose::Open),
            Ok("save") => self.ask(files::Purpose::SaveAs),
            Ok("export") => self.ask(files::Purpose::Export),
            _ => {}
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
                self.start(song);
                self.say(format!("opened {}", path.display()));
                self.path = Some(path);
            }
            Err(e) => self.say(format!("couldn't open: {e}")),
        }
    }

    /// Start afresh on `song`: stopped, at the start, nothing to undo, and
    /// nothing unsaved.
    fn start(&mut self, song: Song) {
        let _ = self.controller.send(Command::Stop);
        let _ = self.controller.send(Command::Locate(0));
        self.gesture = None;
        self.loop_memory = song.loop_range.clone().unwrap_or(0..4 * TICKS_PER_BAR);
        self.doc = Document::new(song);
        // Resend the song and rebuild the graph, whatever revision and rack
        // version the new document starts at.
        self.sent_revision = u64::MAX;
        self.rack_version = self.doc.song.rack.version.wrapping_add(1);
        self.saved_revision = self.doc.revision;
        self.close_armed = false;
        self.arrange = arrange::ArrangeView::default();
        self.path = None;
    }

    /// Do `what`, first asking if that would lose unsaved changes (unless
    /// `asked` already).
    fn discard_then(&mut self, what: Discard, asked: bool) {
        if self.unsaved() && !asked {
            self.confirm = Some(what);
            return;
        }
        match what {
            Discard::New => {
                self.start(new_song());
                self.say("new project");
            }
            Discard::Open(path) => self.open(path),
        }
    }

    fn export(&mut self, path: PathBuf) {
        let message = match crate::export::export_wav(&self.doc.song, &path) {
            Ok(seconds) => format!("exported {seconds:.1}s to {}", path.display()),
            Err(e) => format!("couldn't export: {e}"),
        };
        self.say(message);
    }

    fn ask(&mut self, purpose: files::Purpose) {
        let dir = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .filter(|d| !d.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_owned);
        let stem = self.path.as_deref().and_then(Path::file_stem).map_or_else(
            || "untitled".to_owned(),
            |s| s.to_string_lossy().into_owned(),
        );
        let name = match purpose {
            files::Purpose::Open => String::new(),
            files::Purpose::SaveAs => format!("{stem}.{}", project::EXTENSION),
            files::Purpose::Export => format!("{stem}.wav"),
        };
        self.dialog = Some(files::Dialog::new(purpose, &dir, &name));
    }

    /// Add a track playing a new instrument, as one undo step. `poly`: its
    /// notes ring over each other.
    fn add_instrument(&mut self, instrument: Instrument, name: &str, poly: bool) {
        self.finish_gesture();
        let t = self.doc.edit(|s| {
            s.instruments.push(instrument);
            let i = s.instruments.len() - 1;
            let t = s.add_track(name, i);
            match t {
                Some(t) => s.tracks[t].poly = poly,
                None => {
                    s.instruments.pop();
                }
            }
            t
        });
        match t {
            Some(t) => {
                self.select(Select::Track(t));
                self.tab = Tab::Devices;
                self.say(format!("added {name}: double-click its lane for a clip"));
            }
            None => {
                // Nothing changed, so there's nothing to undo either.
                self.doc.undo();
                self.say(format!(
                    "can't: {} tracks is the most",
                    crate::engine::MAX_TRACKS
                ));
            }
        }
    }

    /// Play a sample file, outside the song.
    fn preview(&mut self, path: &std::path::Path) {
        match Sample::load_wav(path) {
            Ok(sample) => {
                let _ = self
                    .controller
                    .send(Command::Preview(Some(Arc::new(sample))));
            }
            Err(e) => {
                let _ = self.controller.send(Command::Preview(None));
                self.say(format!("can't play {}: {e}", path.display()));
            }
        }
    }

    fn pick(&mut self, pick: browser::Pick) {
        match pick {
            browser::Pick::Kit => {
                // Kit hits ring out over each other.
                let kit = Instrument::Sampler(Arc::new(crate::demo::kit()));
                self.add_instrument(kit, "drums", true);
            }
            browser::Pick::Synth(kind) => {
                let patch = SynthPatch::new(kind, kind.name());
                // Acid lines slide from note to note, so one at a time.
                let poly = kind != crate::synthvoice::SynthKind::Acid;
                self.add_instrument(Instrument::Synth(Arc::new(patch)), kind.name(), poly);
            }
            browser::Pick::Sample(path) => match Sample::load_wav(&path) {
                Ok(mut sample) => {
                    sample.detect_slices(SENSITIVITY, MIN_SLICE_MS);
                    let name = path
                        .file_stem()
                        .map_or_else(|| "sample".to_owned(), |s| s.to_string_lossy().into_owned());
                    let slices = sample.slices.len();
                    // A one-shot rings out; a loop with slices gets chopped.
                    let poly = slices == 1;
                    self.add_instrument(Instrument::Sampler(Arc::new(sample)), &name, poly);
                    if self
                        .message
                        .as_deref()
                        .is_some_and(|m| m.starts_with("added"))
                    {
                        self.say(format!(
                            "added {name}, {slices} slices: double-click its lane for a clip"
                        ));
                    }
                }
                Err(e) => self.say(format!("couldn't load {}: {e}", path.display())),
            },
        }
    }

    fn undo(&mut self, redo: bool) {
        self.finish_gesture();
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
            if ui.button("New").clicked() {
                self.discard_then(Discard::New, false);
            }
            if ui.button("Open…").clicked() {
                self.ask(files::Purpose::Open);
            }
            if ui.button("Save").clicked() {
                match self.path.clone() {
                    Some(path) => self.save(path),
                    None => self.ask(files::Purpose::SaveAs),
                }
            }
            if ui.button("Save as…").clicked() {
                self.ask(files::Purpose::SaveAs);
            }
            ui.separator();
            if ui.button("Export WAV…").clicked() {
                self.ask(files::Purpose::Export);
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
        if out.open_clip {
            self.tab = Tab::Clip;
        }
        self.changes(out.edits, out.live, false, out.gesture_done);
        if playing || ui.input(|i| i.pointer.any_down()) {
            ui.ctx().request_repaint();
        }
    }

    /// What a view reports: one-shot edits, then the latest step of a
    /// gesture. `end_first` ends the previous gesture before anything else;
    /// `done` ends the gesture after.
    fn changes(&mut self, edits: Vec<Edit>, live: Option<Edit>, end_first: bool, done: bool) {
        if end_first {
            self.finish_gesture();
        }
        for edit in edits {
            self.finish_gesture();
            self.edit(edit);
        }
        if let Some(edit) = live {
            self.live_edit(edit);
        }
        if done {
            self.finish_gesture();
        }
    }

    /// Make the gesture so far one undo step.
    fn finish_gesture(&mut self) {
        if let Some((before, changed)) = self.gesture.take()
            && changed
        {
            self.doc.record(before);
        }
        self.steps.end_wheel();
        self.devices.end_wheel();
    }

    /// Make an edit as one undo step.
    fn edit(&mut self, edit: Edit) {
        if edit.is_mixing() {
            if apply(&mut self.doc.song, &edit).is_ok() {
                self.doc.changed();
            }
            return;
        }
        let before = self.doc.song.clone();
        match apply(&mut self.doc.song, &edit) {
            Ok(select) => {
                self.doc.record(before);
                self.select(select);
                self.knob_to_engine(&edit);
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
            self.knob_to_engine(&edit);
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

    /// Rack knob changes go straight to the engine's graph. (Synth knobs
    /// reach it with the song.)
    fn knob_to_engine(&mut self, edit: &Edit) {
        let (device, param, value) = match *edit {
            Edit::Volume(t, value) => match self.doc.song.rack.track_device(t) {
                Some(d) => (d.id, TRACK_VOLUME, value),
                None => return,
            },
            Edit::RackParam {
                device,
                param,
                value,
            } => (device, param, value),
            _ => return,
        };
        let _ = self.controller.send(Command::SetParam {
            device,
            param,
            value,
        });
    }

    /// The bottom panel: the selected clip in the clip editor, or the
    /// selected track's devices.
    fn editor_panel(&mut self, ui: &mut egui::Ui) {
        let song = &self.doc.song;
        let track = self.arrange.selected_track;
        let clip = self
            .arrange
            .selected_clip
            .filter(|&c| song.clip(c).is_some());
        let mut grid = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.selectable_value(&mut self.tab, Tab::Clip, RichText::new("Clip editor").size(15.0));
            ui.selectable_value(&mut self.tab, Tab::Devices, RichText::new("Devices").size(15.0));
            ui.add_space(16.0);
            let name = song.tracks.get(track).map_or("", |t| t.name.as_str());
            match (self.tab, clip.and_then(|c| song.clip(c).map(|x| (c, x)))) {
                (Tab::Clip, Some((at, c))) => {
                    ui.label(
                        RichText::new(format!(
                            "{name} · bar {} · {} rows",
                            arrange::position_text(c.start),
                            c.cells.len()
                        ))
                        .color(color::MUTED),
                    );
                    let mut rpb = c.rows_per_beat;
                    egui::ComboBox::from_id_salt("grid")
                        .selected_text(format!("{rpb} rows per beat"))
                        .show_ui(ui, |ui| {
                            for r in crate::song::RESOLUTIONS {
                                ui.selectable_value(&mut rpb, r, format!("{r} rows per beat"));
                            }
                        });
                    if rpb != c.rows_per_beat {
                        grid = Some(Edit::Resolution(at, rpb));
                    }
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(
                            "click or drag to add and remove hits · right-click a hit to roll it · scroll over a hit for velocity · click a lane name to hear it",
                        )
                        .color(color::DIM),
                    );
                }
                (Tab::Devices, _) if !name.is_empty() => {
                    ui.label(RichText::new(format!("{name} · instrument and effects")).color(color::MUTED));
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new("drag or scroll knobs (shift: fine) · click switches · double-click resets")
                            .color(color::DIM),
                    );
                }
                _ => {}
            }
        });
        if let Some(edit) = grid {
            self.changes(vec![edit], None, false, false);
        }
        ui.separator();
        let (_, rect) = ui.allocate_space(ui.available_size());
        let mut content = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        let song = &self.doc.song;
        match self.tab {
            Tab::Clip => {
                let Some(at) = clip else {
                    content.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new("Select a clip to edit it here, or double-click an empty spot in a lane for a new one.")
                                .color(color::MUTED),
                        );
                    });
                    return;
                };
                let pos = self.controller.position();
                let c = song.clip(at).unwrap();
                let playhead = (self.controller.is_playing() && c.span().contains(&pos))
                    .then(|| ((pos - c.start) / c.ticks_per_row()) as usize);
                let out = self.steps.show(&mut content, song, at, playhead);
                if let Some((instrument, slice)) = out.audition {
                    let _ = self.controller.send(Command::Audition {
                        instrument,
                        slice,
                        velocity: 110,
                        hold: false,
                    });
                }
                self.changes(out.edits, out.live, out.end_first, out.gesture_done);
            }
            Tab::Devices => {
                if track >= song.tracks.len() {
                    content.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new("Add a track to see its devices.").color(color::MUTED),
                        );
                    });
                    return;
                }
                let out = self.devices.show(&mut content, song, track);
                self.changes(out.edits, out.live, out.end_first, out.gesture_done);
            }
        }
        if ui.input(|i| i.pointer.any_down()) || self.controller.is_playing() {
            ui.ctx().request_repaint();
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

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(d) = &mut self.dialog
            && let Some(answer) = d.show(ctx)
        {
            let purpose = d.purpose;
            self.dialog = None;
            if let files::Answer::Chose(path) = answer {
                match purpose {
                    files::Purpose::Open => self.discard_then(Discard::Open(path), false),
                    files::Purpose::SaveAs => self.save(path),
                    files::Purpose::Export => self.export(path),
                }
            }
        }
        let Some(what) = &self.confirm else {
            return;
        };
        let doing = match what {
            Discard::New => "starting a new project".to_owned(),
            Discard::Open(p) => format!("opening {}", p.display()),
        };
        let (mut yes, mut no) = (false, false);
        let modal = egui::Modal::new(egui::Id::unique("discard")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Unsaved changes");
            ui.add_space(6.0);
            ui.label(format!(
                "{} has changes that aren't saved. They're lost by {doing}.",
                self.project_name().trim_end_matches(" *")
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                yes = ui.button("Discard them").clicked();
                no = ui.button("Cancel").clicked();
            });
        });
        if yes {
            let what = self.confirm.take().unwrap();
            self.discard_then(what, true);
        } else if no || modal.should_close() {
            self.confirm = None;
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
        if self.dialog.is_none()
            && self.confirm.is_none()
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
        if self.view == View::Arrange {
            egui::Panel::bottom("editor")
                .resizable(true)
                .default_size(330.0)
                .min_size(160.0)
                .frame(frame(color::BG).inner_margin(egui::Margin::symmetric(10, 6)))
                .show(ui, |ui| self.editor_panel(ui));
            let pick = egui::Panel::left("browser")
                .resizable(true)
                .default_size(250.0)
                .min_size(180.0)
                .frame(frame(color::PANEL).inner_margin(egui::Margin::symmetric(10, 8)))
                .show(ui, |ui| self.browser.show(ui))
                .inner;
            if let Some(path) = self.browser.take_preview() {
                self.preview(&path);
            }
            if let Some(pick) = pick {
                self.pick(pick);
            }
        }
        egui::CentralPanel::default()
            .frame(frame(color::BG))
            .show(ui, |ui| match self.view {
                View::Arrange => self.arrange(ui),
                View::Rack => self.rack(ui),
            });
        self.dialogs(&ctx);

        #[cfg(feature = "screenshot")]
        if let Some(s) = &mut self.screenshot {
            s.update(&ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_project_is_the_kit_on_one_empty_track() {
        let song = new_song();
        assert_eq!(song.tracks.len(), 1);
        assert!(song.tracks[0].clips.is_empty());
        assert!(song.tracks[0].poly, "kit hits ring out");
        assert!(matches!(song.instruments[0], Instrument::Sampler(_)));
        assert!(song.rack.track_device(0).is_some(), "it can be heard");
    }

    #[test]
    fn the_browser_starts_somewhere_sensible() {
        // Tests run from the source tree, which has the built-in samples.
        assert_eq!(samples_folder(None), Path::new("assets/drums"));
    }
}
