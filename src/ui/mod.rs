//! The main window: a transport bar, then either the tracker (slicer and
//! pattern grid, F1) or the rack (F2), and a status bar.
//!
//! Everything is drawn with the painter and nothing takes keyboard focus, so
//! every key goes to the tracker. Editing logic lives in [`crate::editor`];
//! this module only maps keys to editor actions and draws the result.

mod knob;
mod midi;
mod palette;
mod rack;
#[cfg(feature = "screenshot")]
mod screenshot;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, FontId, Key, Modifiers, Pos2, Rect, Sense, Stroke, pos2, vec2,
};

use crate::audio::{self, Output};
use crate::editor::{Action, Editor, Field};
use crate::engine::{Command, Controller, Engine, Position};
use crate::graph::Graph;
use crate::pattern::{Cell, Fx, Instrument, Song, Target};
use crate::project;
use crate::sample::Sample;
use crate::synthvoice::SynthKind;
use crate::tuning::{Tuning, note_name};
use crate::tutorial::{self, Facts, Focus, Tutorial};

/// Default slice detection sensitivity for loaded samples.
const SENSITIVITY: f32 = 0.6;
/// How close, in points, the pointer must be to grab a slice marker.
const MARKER_HIT: f32 = 6.0;
/// Width of each knob in the synth panel.
const SYNTH_KNOB_CELL: f32 = 76.0;

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
/// Characters per track column: "NNN VV FFF FFF". The first field is a note
/// name on synth tracks, so it's three wide.
const TRACK_CHARS: usize = 14;
const TRACK_GAP: usize = 3;
const ROW_NUMBER_CHARS: usize = 4;

/// Character offset of each field within a track column.
fn field_offset(field: Field) -> usize {
    match field {
        Field::Slice => 0,
        Field::Velocity => 4,
        Field::Fx(i) => 7 + 4 * i,
    }
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
        // Deliver ctrl+C/X/V as plain key events. Otherwise egui turns them
        // into clipboard events and drops ctrl+V when the system clipboard
        // holds no text, so pasting the editor's own clipboard did nothing.
        clipboard_shortcuts: false,
        ..Default::default()
    };
    eframe::run_native(
        "cowbell",
        options,
        Box::new(|_cc| Ok(Box::new(App::new(song, path)))),
    )
    .map_err(|e| e.to_string())
}

/// What a key press does, beyond editing.
#[derive(Clone, Copy, Debug)]
enum Act {
    Edit(Action),
    PlayToggle,
    PlayPattern,
    ToggleFollow,
    Bpm(f64),
    View(View),
    Save,
    SaveAs,
    Open,
    LoadSample,
    LoadTuning,
    Export,
    AddSynth(SynthKind),
    Sensitivity(f32),
    /// Change the pattern length by whole beats.
    LengthBeats(isize),
    Palette,
    Tutorial,
    RestartTutorial,
    /// Esc: skip a tutorial step.
    Escape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptKind {
    SaveAs,
    Open,
    LoadSample,
    LoadTuning,
    Export,
}

/// A one-line text prompt in the status bar, for file names.
struct Prompt {
    kind: PromptKind,
    text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Tracker,
    Rack,
}

fn key_act(key: Key, m: Modifiers) -> Option<Act> {
    use Action::*;
    let edit = |a| Some(Act::Edit(a));
    let bpm_step = if m.shift { 10.0 } else { 1.0 };
    match key {
        Key::ArrowUp if m.alt => Some(Act::Bpm(bpm_step)),
        Key::ArrowDown if m.alt => Some(Act::Bpm(-bpm_step)),
        Key::ArrowUp if m.command && m.shift => edit(Resolution(1)),
        Key::ArrowDown if m.command && m.shift => edit(Resolution(-1)),
        Key::ArrowUp if m.command => edit(EditStep(1)),
        Key::ArrowDown if m.command => edit(EditStep(-1)),
        Key::ArrowUp if m.shift => edit(SelectUp),
        Key::ArrowDown if m.shift => edit(SelectDown),
        Key::ArrowUp => edit(Up),
        Key::ArrowDown => edit(Down),
        Key::ArrowLeft if m.alt && m.shift => edit(Length(-1)),
        Key::ArrowRight if m.alt && m.shift => edit(Length(1)),
        Key::ArrowLeft if m.alt => Some(Act::LengthBeats(-1)),
        Key::ArrowRight if m.alt => Some(Act::LengthBeats(1)),
        Key::ArrowLeft if m.command && m.shift => edit(SlotPattern(-1)),
        Key::ArrowRight if m.command && m.shift => edit(SlotPattern(1)),
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
        Key::Delete if m.command => edit(DeleteSlot),
        Key::Delete | Key::Backspace => edit(Clear),
        Key::N if m.command => edit(NewPattern),
        Key::D if m.command && m.shift => edit(RepeatSlot),
        Key::D if m.command => edit(DuplicatePattern),
        Key::Space => Some(Act::PlayToggle),
        Key::Enter => Some(Act::PlayPattern),
        Key::Z if m.command && m.shift => edit(Redo),
        Key::Z if m.command => edit(Undo),
        Key::Y if m.command => edit(Redo),
        Key::C if m.command => edit(Copy),
        Key::X if m.command => edit(Cut),
        Key::V if m.command => edit(Paste),
        Key::Copy => edit(Copy),
        Key::Cut => edit(Cut),
        Key::Paste => edit(Paste),
        Key::R if m.command => edit(Roll),
        Key::T if m.command => edit(AddTrack),
        Key::F if m.command => Some(Act::ToggleFollow),
        Key::S if m.command && m.shift => Some(Act::SaveAs),
        Key::S if m.command => Some(Act::Save),
        Key::O if m.command => Some(Act::Open),
        Key::B if m.command => edit(Bounce),
        Key::L if m.command && m.shift => Some(Act::LoadTuning),
        Key::E if m.command => Some(Act::Export),
        Key::K if m.command => edit(Interpolate),
        Key::G if m.command => edit(Variations),
        Key::L if m.command => Some(Act::LoadSample),
        Key::F5 => Some(Act::AddSynth(SynthKind::Acid)),
        Key::F6 => Some(Act::AddSynth(SynthKind::Fm)),
        Key::I if m.command && m.shift => edit(PrevInstrument),
        Key::I if m.command => edit(NextInstrument),
        Key::OpenBracket => Some(Act::Sensitivity(-0.1)),
        Key::CloseBracket => Some(Act::Sensitivity(0.1)),
        Key::P if m.command => Some(Act::Palette),
        Key::F12 => Some(Act::Tutorial),
        Key::Escape => Some(Act::Escape),
        Key::F1 => Some(Act::View(View::Tracker)),
        Key::F2 => Some(Act::View(View::Rack)),
        _ => None,
    }
}

/// Turn typed text into a project path: `~/` is the home folder, relative
/// paths are from the current folder, and the extension is added if missing.
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

/// Min/max per pixel column, recomputed only when the sample or width changes.
#[derive(Default)]
struct WaveCache {
    sample: Option<Arc<Sample>>,
    width: usize,
    peaks: Vec<(f32, f32)>,
}

impl WaveCache {
    fn get(&mut self, sample: &Arc<Sample>, width: usize) -> &[(f32, f32)] {
        // Keyed on the audio, not the sample: editing markers makes a new
        // sample that shares the same frames.
        let fresh = self
            .sample
            .as_ref()
            .is_some_and(|s| Arc::ptr_eq(&s.frames, &sample.frames))
            && self.width == width;
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
    view: View,
    rack_view: rack::RackView,
    sample_rate: u32,
    /// The rack version the engine's graph was built from.
    rack_version: u64,
    /// Where the project is saved, once it has been.
    path: Option<PathBuf>,
    /// MIDI keyboards; opened on the first frame.
    midi: Option<midi::Inputs>,
    /// MIDI keys held down, to release the audition when the last one lifts.
    midi_held: Vec<u8>,
    /// `editor.revision` when last saved or opened.
    saved_revision: u64,
    prompt: Option<Prompt>,
    /// Set after a close was cancelled because of unsaved changes; a second
    /// close goes through.
    close_armed: bool,
    title: String,
    /// Slice detection sensitivity, for loading and re-detecting.
    sensitivity: f32,
    /// A slice marker being dragged: (instrument, marker, moved yet).
    marker_drag: Option<(usize, usize, bool)>,
    /// A synth knob being dragged: (instrument, param, start y, start knob
    /// position, moved yet).
    synth_drag: Option<(usize, usize, f32, f32, bool)>,
    palette: Option<palette::Palette>,
    tutorial: Option<Tutorial>,
    /// What the tutorial's current step has seen happen.
    tutorial_facts: Facts,
    /// Set after warning that starting the tutorial replaces unsaved work.
    tutorial_armed: bool,
    #[cfg(feature = "screenshot")]
    screenshot: Option<screenshot::Screenshot>,
}

impl App {
    fn new(song: Song, path: Option<PathBuf>) -> Self {
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
        let sample_rate = output.as_ref().map_or(48_000, |o| o.sample_rate);
        let rack_version = editor.song.rack.version;
        #[cfg(feature = "screenshot")]
        let view = if std::env::var("COWBELL_VIEW").as_deref() == Ok("rack") {
            View::Rack
        } else {
            View::Tracker
        };
        #[cfg(not(feature = "screenshot"))]
        let view = View::Tracker;
        #[allow(unused_mut)]
        let mut app = App {
            editor,
            controller,
            output,
            follow: true,
            last_pos: None,
            wave: WaveCache::default(),
            char_width: 8.0,
            scroll: 0,
            view,
            rack_view: rack::RackView::default(),
            sample_rate,
            rack_version,
            path,
            midi: None,
            midi_held: Vec::new(),
            saved_revision: 0,
            prompt: None,
            close_armed: false,
            title: String::new(),
            sensitivity: SENSITIVITY,
            marker_drag: None,
            synth_drag: None,
            palette: None,
            tutorial: None,
            tutorial_facts: Facts::default(),
            tutorial_armed: false,
            #[cfg(feature = "screenshot")]
            screenshot,
        };
        #[cfg(feature = "screenshot")]
        app.screenshot_setup();
        app
    }

    /// Dev only: `COWBELL_TUTORIAL=n` starts the tutorial n steps in, and
    /// `COWBELL_PALETTE=query` opens the command palette with a search.
    #[cfg(feature = "screenshot")]
    fn screenshot_setup(&mut self) {
        if self.screenshot.is_none() {
            return;
        }
        if let Some(n) = std::env::var("COWBELL_TUTORIAL")
            .ok()
            .and_then(|n| n.parse::<usize>().ok())
        {
            // The script may have edited the demo song; replace it anyway.
            self.tutorial_armed = true;
            self.toggle_tutorial();
            for _ in 0..n {
                self.run(Act::Escape);
            }
            screenshot::apply_keys(&mut self.editor);
        }
        if let Ok(q) = std::env::var("COWBELL_PALETTE") {
            let mut p = palette::Palette::new();
            p.query = q;
            self.palette = Some(p);
        }
    }

    fn font() -> FontId {
        FontId::monospace(FONT_SIZE)
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
        self.editor.revision != self.saved_revision
    }

    fn save(&mut self, path: PathBuf) {
        match project::save(&self.editor.song, &path) {
            Ok(()) => {
                self.saved_revision = self.editor.revision;
                self.close_armed = false;
                self.editor.message = Some(format!("saved {}", path.display()));
                self.path = Some(path);
            }
            Err(e) => self.editor.message = Some(format!("couldn't save: {e}")),
        }
    }

    fn open(&mut self, path: PathBuf) {
        match project::load(&path) {
            Ok(song) => {
                let _ = self.controller.send(Command::Stop);
                self.editor = Editor::new(song.clone());
                let _ = self.controller.send(Command::SwapSong(Box::new(song)));
                // Force a new graph, whatever version the loaded rack has.
                self.rack_version = self.editor.song.rack.version.wrapping_add(1);
                self.saved_revision = self.editor.revision;
                self.close_armed = false;
                self.scroll = 0;
                self.editor.message = Some(format!("opened {}", path.display()));
                self.path = Some(path);
            }
            Err(e) => self.editor.message = Some(format!("couldn't open: {e}")),
        }
    }

    /// Keys while a prompt is open.
    fn prompt_input(&mut self, event: &egui::Event) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        match event {
            egui::Event::Text(t) => prompt.text.extend(t.chars().filter(|c| !c.is_control())),
            egui::Event::Key {
                key: Key::Backspace,
                pressed: true,
                ..
            } => {
                prompt.text.pop();
            }
            egui::Event::Key {
                key: Key::Escape,
                pressed: true,
                ..
            } => self.prompt = None,
            egui::Event::Key {
                key: Key::Enter,
                pressed: true,
                ..
            } => {
                let prompt = self.prompt.take().unwrap();
                let path = project_path(&prompt.text);
                match prompt.kind {
                    PromptKind::SaveAs => self.save(path),
                    PromptKind::Open => self.open(path),
                    PromptKind::LoadSample => self.load_sample(typed_path(&prompt.text)),
                    PromptKind::LoadTuning => self.load_tuning(typed_path(&prompt.text)),
                    PromptKind::Export => self.export(typed_path(&prompt.text)),
                }
            }
            _ => {}
        }
    }

    fn load_sample(&mut self, path: PathBuf) {
        match Sample::load_wav(&path) {
            Ok(mut sample) => {
                sample.detect_slices(self.sensitivity, 40.0);
                self.editor.add_sample(sample);
            }
            Err(e) => self.editor.message = Some(format!("couldn't load {}: {e}", path.display())),
        }
    }

    /// Load a Scala file, or a built-in tuning by name when there's no
    /// such file.
    fn load_tuning(&mut self, path: PathBuf) {
        let builtin = (!path.exists())
            .then(|| path.to_str().and_then(crate::tuning::builtin))
            .flatten();
        let tuning = match builtin {
            Some(t) => Ok(t),
            None => std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| Tuning::from_scala(&t)),
        };
        match tuning {
            Ok(t) => {
                let name = t.name.clone();
                self.editor.set_tuning(t);
                self.editor.message = Some(format!("tuning: {name}"));
            }
            Err(e) => self.editor.message = Some(format!("couldn't load {}: {e}", path.display())),
        }
    }

    fn export(&mut self, path: PathBuf) {
        self.editor.message = Some(match crate::export::export_wav(&self.editor.song, &path) {
            Ok(seconds) => {
                self.tutorial_facts.exported = true;
                format!("exported {seconds:.1}s to {}", path.display())
            }
            Err(e) => format!("couldn't export: {e}"),
        });
    }

    /// Toggle `target` as the cursor track's automation target.
    fn bind_automation(&mut self, target: Target) {
        let t = self.editor.cursor.track;
        let track = self
            .editor
            .pattern()
            .tracks
            .get(t)
            .map_or_else(String::new, |t| t.name.clone());
        if self.editor.song.automation(t) == Some(target) {
            self.editor.set_automation(None);
            self.editor.message = Some(format!("{track}: K effects turn nothing now"));
        } else {
            self.editor.set_automation(Some(target));
            let name = self.target_name(target).unwrap_or_default();
            self.editor.message = Some(format!(
                "{track}: K effects turn {name}; put Kxx in an effect column"
            ));
        }
    }

    /// A target's knob, by device and name, for messages; and its value
    /// for a `K` parameter.
    fn target_name(&self, target: Target) -> Option<String> {
        self.target_spec(target)
            .map(|(device, spec)| format!("{device} {}", spec.name))
    }

    fn target_spec(&self, target: Target) -> Option<(String, &'static crate::rack::ParamSpec)> {
        match target {
            Target::Rack { device, param } => {
                let d = self.editor.song.rack.device(device)?;
                Some((d.kind.name().to_owned(), d.kind.params().get(param)?))
            }
            Target::Synth { param } => {
                let (_, p) = self.editor.track_synth()?;
                Some((p.name.clone(), p.kind.params().get(param)?))
            }
        }
    }

    fn ask(&mut self, kind: PromptKind) {
        let text = match (&self.path, kind) {
            (_, PromptKind::LoadSample | PromptKind::LoadTuning) => String::new(),
            (Some(p), PromptKind::Export) => p.with_extension("wav").display().to_string(),
            (None, PromptKind::Export) => "untitled.wav".into(),
            (Some(p), _) => p.display().to_string(),
            (None, PromptKind::SaveAs) => format!("untitled.{}", project::EXTENSION),
            (None, PromptKind::Open) => String::new(),
        };
        self.prompt = Some(Prompt { kind, text });
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let (events, modifiers) = ctx.input(|i| (i.events.clone(), i.modifiers));
        for event in events {
            if self.prompt.is_some() {
                self.prompt_input(&event);
                continue;
            }
            if self.palette.is_some() {
                self.palette_input(&event);
                continue;
            }
            // ? opens the command palette, in any view.
            if matches!(&event, egui::Event::Text(t) if t == "?") {
                self.palette = Some(palette::Palette::new());
                continue;
            }
            if matches!(event, egui::Event::Key { pressed: true, .. }) {
                self.editor.message = None;
            }
            let act = match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => key_act(key, modifiers),
                // Text arrives alongside its key event; skip it for shortcuts
                // and for space, which is play/stop.
                egui::Event::Text(text)
                    if self.view == View::Tracker && !modifiers.command && !modifiers.alt =>
                {
                    for c in text.chars().filter(|c| !c.is_whitespace()) {
                        self.editor.apply(Action::Char(c));
                    }
                    None
                }
                _ => None,
            };
            if let Some(act) = act {
                self.run(act);
            }
        }
    }

    /// MIDI keys audition what they play. While stopped, in the tracker,
    /// they also enter it at the cursor; while playing you jam along
    /// without writing anything.
    fn handle_midi(&mut self, ctx: &egui::Context) {
        #[cfg(feature = "screenshot")]
        if self.screenshot.is_some() {
            return;
        }
        let inputs = self
            .midi
            .get_or_insert_with(|| midi::Inputs::new(ctx.clone()));
        let mut connected = Vec::new();
        let msgs = inputs.poll(&mut connected);
        if !connected.is_empty() {
            self.editor.message = Some(format!("MIDI: {} connected", connected.join(", ")));
        }
        for msg in msgs {
            match msg {
                midi::Msg::NoteOn { note, velocity } => {
                    let enter = self.view == View::Tracker
                        && self.prompt.is_none()
                        && self.palette.is_none()
                        && !self.controller.is_playing();
                    let Some((instrument, slice)) = self.editor.midi_note(note, velocity, enter)
                    else {
                        continue;
                    };
                    self.tutorial_facts.auditioned = true;
                    self.midi_held.push(note);
                    let _ = self.controller.send(Command::Audition {
                        instrument,
                        slice,
                        velocity,
                        hold: true,
                    });
                }
                midi::Msg::NoteOff { note } => {
                    let was_held = !self.midi_held.is_empty();
                    self.midi_held.retain(|&n| n != note);
                    if was_held && self.midi_held.is_empty() {
                        let _ = self.controller.send(Command::Release);
                    }
                }
            }
        }
    }

    fn palette_input(&mut self, event: &egui::Event) {
        let Some(p) = &mut self.palette else { return };
        match event {
            egui::Event::Text(t) => {
                p.query.extend(t.chars().filter(|c| !c.is_control()));
                p.selected = 0;
            }
            egui::Event::Key {
                key, pressed: true, ..
            } => match key {
                Key::Backspace => {
                    p.query.pop();
                    p.selected = 0;
                }
                Key::ArrowDown => p.move_selection(1),
                Key::ArrowUp => p.move_selection(-1),
                Key::Escape => self.palette = None,
                Key::Enter => {
                    let act = p.chosen();
                    self.palette = None;
                    if let Some(act) = act {
                        self.run(act);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn toggle_tutorial(&mut self) {
        if self.tutorial.is_some() {
            self.save_tutorial();
            self.tutorial = None;
            self.editor.message = Some("tutorial closed; F12 picks up where you left off".into());
            return;
        }
        if self.unsaved() && !self.tutorial_armed {
            self.tutorial_armed = true;
            self.editor.message =
                Some("couldn't start: the tutorial replaces this song. ctrl+s to save it, or F12 again to go ahead".into());
            return;
        }
        let saved = self
            .tutorial_dir()
            .and_then(|d| tutorial::load_progress(&d));
        let (tut, song) = match saved {
            Some((tut, song)) => {
                self.editor.message = Some(match tut.current() {
                    Some(_) => format!(
                        "resumed the tutorial at lesson {}, step {}. To start over: ? restart",
                        tut.lesson + 1,
                        tut.step + 1
                    ),
                    None => "you finished the tutorial. To start over: ? restart".into(),
                });
                (tut, song)
            }
            None => {
                let song = tutorial::song();
                (Tutorial::new(&song), song)
            }
        };
        let _ = self.controller.send(Command::Stop);
        let message = self.editor.message.take();
        self.editor = Editor::new(song.clone());
        self.editor.message = message;
        let _ = self
            .controller
            .send(Command::SwapSong(Box::new(song.clone())));
        self.rack_version = self.editor.song.rack.version.wrapping_add(1);
        self.saved_revision = self.editor.revision;
        self.path = None;
        self.view = View::Tracker;
        self.scroll = 0;
        self.tutorial = Some(tut);
        self.tutorial_facts = Facts::default();
        self.tutorial_armed = false;
    }

    /// Start the tutorial over from the first lesson, forgetting progress.
    fn restart_tutorial(&mut self) {
        if let Some(dir) = self.tutorial_dir() {
            tutorial::clear_progress(&dir);
        }
        if self.tutorial.take().is_some() {
            // Its song is the tutorial's own; nothing of yours to lose.
            self.tutorial_armed = true;
        }
        self.toggle_tutorial();
    }

    /// Where tutorial progress goes; none while taking screenshots, so dev
    /// runs don't touch your saved progress.
    fn tutorial_dir(&self) -> Option<PathBuf> {
        #[cfg(feature = "screenshot")]
        if self.screenshot.is_some() {
            return None;
        }
        tutorial::state_dir()
    }

    fn save_tutorial(&mut self) {
        let (Some(t), Some(dir)) = (&self.tutorial, self.tutorial_dir()) else {
            return;
        };
        if let Err(e) = tutorial::save_progress(&dir, t, &self.editor.song) {
            self.editor.message = Some(format!("couldn't save tutorial progress: {e}"));
        }
    }

    fn update_tutorial(&mut self) {
        let Some(t) = &mut self.tutorial else { return };
        let playing = self.controller.is_playing();
        let f = &mut self.tutorial_facts;
        f.playing = playing;
        f.played |= playing;
        f.rack_view = self.view == View::Rack;
        f.palette_open = self.palette.is_some();
        f.saved = self.path.is_some() && self.editor.revision == self.saved_revision;
        if t.update(&self.editor, *f) {
            *f = Facts::default();
            self.save_tutorial();
        }
    }

    fn tutorial_strip(&self, ui: &mut egui::Ui) {
        let Some(t) = &self.tutorial else { return };
        let r = ui.max_rect();
        let p = ui.painter();
        let font = Self::font();
        let (title, text, keys) = match t.current() {
            Some((lesson, step)) => (
                format!(
                    "tutorial · lesson {}/{}: {} · step {}/{}",
                    t.lesson + 1,
                    t.lesson_count(),
                    lesson.title,
                    t.step + 1,
                    lesson.steps.len()
                ),
                step.text,
                step.keys,
            ),
            None => (
                "tutorial · done".to_owned(),
                "That's every lesson: you made a track from break to WAV. Keep going with this song, or F12 to close and ctrl+o to open your own.",
                "F12",
            ),
        };
        p.text(
            r.left_top() + vec2(8.0, 6.0),
            Align2::LEFT_TOP,
            title,
            font.clone(),
            color::ACCENT,
        );
        p.text(
            r.right_top() + vec2(-8.0, 6.0),
            Align2::RIGHT_TOP,
            "esc skip step · F12 close",
            font.clone(),
            color::DIM,
        );
        let galley = p.layout(text.to_owned(), font.clone(), color::TEXT, r.width() - 16.0);
        let h = galley.size().y;
        p.galley(r.left_top() + vec2(8.0, 26.0), galley, color::TEXT);
        let k = p.text(
            r.left_top() + vec2(8.0, 30.0 + h),
            Align2::LEFT_TOP,
            "keys:",
            font.clone(),
            color::MUTED,
        );
        p.text(
            k.right_top() + vec2(8.0, 0.0),
            Align2::LEFT_TOP,
            keys,
            font,
            color::ACCENT,
        );
    }

    /// What the tutorial's current step wants you to look at.
    fn tutorial_focus(&self) -> Focus {
        self.tutorial
            .as_ref()
            .and_then(|t| t.current())
            .map_or(Focus::None, |(_, s)| s.focus)
    }

    /// Do what a key (or the command palette) asked for.
    fn run(&mut self, act: Act) {
        match act {
            Act::Palette => self.palette = Some(palette::Palette::new()),
            Act::Tutorial => self.toggle_tutorial(),
            Act::Escape => {
                if let Some(t) = &mut self.tutorial {
                    t.skip(&self.editor.song);
                    self.tutorial_facts = Facts::default();
                    self.save_tutorial();
                }
            }
            Act::RestartTutorial => self.restart_tutorial(),
            // The rack view is for the mouse; editing keys stay with the
            // tracker so they can't change a pattern you can't see. Undo
            // and redo work in both, over one shared history.
            Act::Edit(a)
                if self.view == View::Rack && !matches!(a, Action::Undo | Action::Redo) => {}
            Act::Edit(a) => self.editor.apply(a),
            Act::View(v) => self.view = v,
            Act::PlayToggle => {
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
            Act::PlayPattern => {
                let order = self.editor.cursor.order;
                let _ = self
                    .controller
                    .send(Command::PlayFrom(Position { order, row: 0 }));
            }
            Act::ToggleFollow => self.follow = !self.follow,
            Act::Bpm(d) => {
                let bpm = (self.editor.song.bpm + d).clamp(20.0, 400.0);
                self.editor.song.bpm = bpm;
                self.editor.changed();
                let _ = self.controller.send(Command::SetBpm(bpm));
            }
            Act::Save => match self.path.clone() {
                Some(path) => self.save(path),
                None => self.ask(PromptKind::SaveAs),
            },
            Act::SaveAs => self.ask(PromptKind::SaveAs),
            Act::Open => self.ask(PromptKind::Open),
            Act::LoadSample => self.ask(PromptKind::LoadSample),
            Act::LengthBeats(_) if self.view == View::Rack => {}
            Act::LengthBeats(d) => {
                let rpb = self.editor.pattern().rows_per_beat as isize;
                self.editor.apply(Action::Length(d * rpb));
            }
            Act::Sensitivity(_) | Act::AddSynth(_) if self.view == View::Rack => {}
            Act::LoadTuning => self.ask(PromptKind::LoadTuning),
            Act::Export => self.ask(PromptKind::Export),
            Act::AddSynth(kind) => self.editor.apply(Action::AddSynth(kind)),
            // On synth tracks the same keys move the octave.
            Act::Sensitivity(d) if self.editor.on_synth() => {
                self.editor.apply(Action::Octave(d.signum() as isize))
            }
            Act::Sensitivity(d) => {
                self.sensitivity = (self.sensitivity + d).clamp(0.0, 1.0);
                if let Some((instrument, _)) = self.editor.track_sample() {
                    self.editor.redetect(instrument, self.sensitivity);
                }
            }
        }
    }

    fn sync_engine(&mut self) {
        let rack = &self.editor.song.rack;
        if rack.version != self.rack_version {
            let graph = Box::new(Graph::build(rack, self.sample_rate));
            if self.controller.send(Command::SwapGraph(graph)).is_ok() {
                self.rack_version = rack.version;
            }
        }
        if self.editor.dirty {
            let song = Box::new(self.editor.song.clone());
            if self.controller.send(Command::SwapSong(song)).is_ok() {
                self.editor.dirty = false;
            }
        }
        if let Some((instrument, slice)) = self.editor.audition.take() {
            self.tutorial_facts.auditioned = true;
            let _ = self.controller.send(Command::Audition {
                instrument,
                slice,
                velocity: 110,
                hold: false,
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
        // Draw left to right; `gap` is the space after.
        let mut put = |text: String, color: Color32, gap: f32| {
            let rect = p.text(pos2(x, y), Align2::LEFT_CENTER, text, font.clone(), color);
            x = rect.right() + gap;
        };
        put("cowbell".into(), color::ACCENT, 18.0);
        for (view, label) in [(View::Tracker, "F1 tracker"), (View::Rack, "F2 rack")] {
            let c = if self.view == view {
                color::TEXT
            } else {
                color::DIM
            };
            put(label.into(), c, 18.0);
        }
        put(
            if playing {
                "▶ playing".into()
            } else {
                "■ stopped".into()
            },
            color::TEXT,
            18.0,
        );
        put(self.project_name(), color::TEXT, 18.0);
        put(format!("{:.1} bpm", e.song.bpm), color::TEXT, 18.0);
        // The order list, current slot highlighted. Long lists show the
        // slots around the cursor.
        put("order".into(), color::MUTED, 8.0);
        let first = e.cursor.order.saturating_sub(6);
        let last = (first + 13).min(e.song.order.len());
        if first > 0 {
            put("…".into(), color::DIM, 6.0);
        }
        for (i, &pat) in e.song.order.iter().enumerate().take(last).skip(first) {
            let (text, c) = if i == e.cursor.order {
                (format!("[{pat:02X}]"), color::ACCENT)
            } else {
                (format!("{pat:02X}"), color::TEXT)
            };
            let more = last < e.song.order.len();
            put(text, c, if i + 1 == last && !more { 18.0 } else { 6.0 });
        }
        if last < e.song.order.len() {
            put("…".into(), color::DIM, 18.0);
        }
        let pat = e.pattern();
        put(
            format!("{} rows · {} rows/beat", pat.rows, pat.rows_per_beat),
            color::TEXT,
            18.0,
        );
        put(format!("step {}", e.edit_step), color::MUTED, 18.0);
        if !e.song.tuning.is_equal_temperament() {
            put(
                format!("tuning: {}", e.song.tuning.name),
                color::MUTED,
                18.0,
            );
        }
        put(
            format!("follow {}", if self.follow { "on" } else { "off" }),
            color::MUTED,
            18.0,
        );
        let (mut text, c) = match &self.output {
            Ok(o) => (format!("{} · {} Hz", o.device, o.sample_rate), color::DIM),
            Err(err) => (format!("no audio: {err}"), color::ERROR),
        };
        let keyboards: Vec<&str> = self.midi.iter().flat_map(|m| m.names()).collect();
        if !keyboards.is_empty() {
            text = format!("midi: {} · {text}", keyboards.join(", "));
        }
        p.text(
            pos2(r.right() - 8.0, y),
            Align2::RIGHT_CENTER,
            text,
            font,
            c,
        );
    }

    /// The slicer for sample tracks, the synth's knobs for synth tracks.
    fn instrument_panel(&mut self, ui: &mut egui::Ui) {
        if self.editor.on_synth() {
            self.synth_panel(ui);
        } else {
            self.slicer(ui);
        }
    }

    fn synth_panel(&mut self, ui: &mut egui::Ui) {
        let Some((instrument, patch)) = self.editor.track_synth() else {
            return;
        };
        let patch = patch.clone();
        let font = Self::font();
        let small = FontId::monospace(11.0);
        let area = ui.max_rect();
        let header = area.with_max_y(area.top() + 18.0);
        let p = ui.painter();
        let tuning = &self.editor.song.tuning;
        p.text(
            header.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            format!(
                "synth: {} · {} · octave {} · tuning: {}",
                patch.name,
                patch.kind.name(),
                self.editor.octave,
                tuning.name
            ),
            font,
            color::MUTED,
        );
        p.text(
            header.right_center() - vec2(8.0, 0.0),
            Align2::RIGHT_CENTER,
            "notes: z-m and q-u · . note off · [ ] octave · drag knobs (shift: fine) · double-click resets",
            small.clone(),
            color::DIM,
        );

        let rect = Rect::from_min_max(
            header.left_bottom() + vec2(8.0, 2.0),
            area.right_bottom() - vec2(8.0, 6.0),
        );
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let specs = patch.kind.params();
        let center = |i: usize| {
            pos2(
                rect.left() + 16.0 + (i as f32 + 0.5) * SYNTH_KNOB_CELL,
                rect.top() + 34.0,
            )
        };
        let knob_at = |pos: Pos2| (0..specs.len()).find(|&i| center(i).distance(pos) <= knob::HIT);
        let shift = ui.input(|i| i.modifiers.shift);

        if response.drag_started()
            && let Some(pos) = ui.input(|i| i.pointer.press_origin())
            && let Some(i) = knob_at(pos)
        {
            let start = specs[i].to_knob(patch.params[i]);
            self.synth_drag = Some((instrument, i, pos.y, start, false));
        }
        if response.dragged()
            && let Some((inst, param, start_y, start_knob, moved)) = &mut self.synth_drag
            && let Some(pos) = response.interact_pointer_pos()
        {
            let spec = &specs[*param];
            let range = if shift {
                knob::FINE_DRAG_RANGE
            } else {
                knob::DRAG_RANGE
            };
            let value = spec.from_knob(*start_knob + (*start_y - pos.y) / range);
            self.editor.set_synth_param(*inst, *param, value, !*moved);
            *moved = true;
            if shift {
                // Re-anchor so switching to fine control doesn't jump.
                *start_y = pos.y;
                *start_knob = spec.to_knob(value);
            }
        }
        if response.drag_stopped() {
            self.synth_drag = None;
        }
        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(i) = knob_at(pos)
        {
            self.editor
                .set_synth_param(instrument, i, specs[i].default, true);
        }
        if response.secondary_clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(i) = knob_at(pos)
        {
            self.bind_automation(Target::Synth { param: i });
        }

        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, color::PANEL);
        let patch = self.editor.track_synth().map_or(patch, |(_, p)| p.clone());
        for (i, spec) in specs.iter().enumerate() {
            let automated = self.editor.song.automation(self.editor.cursor.track)
                == Some(Target::Synth { param: i });
            knob::draw(&p, center(i), spec, patch.params[i], &small, automated);
        }
    }

    fn slicer(&mut self, ui: &mut egui::Ui) {
        let Some((instrument, sample)) = self.editor.track_sample() else {
            return;
        };
        let sample = sample.clone();
        let font = Self::font();
        let small = FontId::monospace(11.0);

        let header = ui.max_rect().with_max_y(ui.max_rect().top() + 18.0);
        let current = self
            .editor
            .cell()
            .trigger
            .map_or(self.editor.last_slice, |t| t.slice) as usize;
        let p = ui.painter();
        p.text(
            header.left_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            format!(
                "slicer: {} · {} slices · slice {:02X} · sensitivity {:.1}",
                sample.name,
                sample.slices.len(),
                current,
                self.sensitivity
            ),
            font.clone(),
            color::MUTED,
        );
        p.text(
            header.right_center() - vec2(8.0, 0.0),
            Align2::RIGHT_CENTER,
            "click hear · ctrl+click enter · shift+click add · drag move · right-click remove · [ ] sensitivity",
            small.clone(),
            color::DIM,
        );

        let rect = Rect::from_min_max(
            header.left_bottom() + vec2(8.0, 2.0),
            ui.max_rect().right_bottom() - vec2(8.0, 6.0),
        );
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let n = sample.frames.len().max(1);
        let x_of = |frame: usize| rect.left() + rect.width() * frame as f32 / n as f32;
        let frame_at =
            |x: f32| (((x - rect.left()) / rect.width()).clamp(0.0, 1.0) * n as f32) as usize;
        let within = (MARKER_HIT / rect.width() * n as f32) as usize;
        let modifiers = ui.input(|i| i.modifiers);

        // Marker editing. Marker 0 is the start of the sample and stays put.
        if response.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
            && let Some(m) = sample.marker_near(frame_at(p.x), within).filter(|&m| m > 0)
        {
            self.marker_drag = Some((instrument, m, false));
        }
        if response.dragged()
            && let Some((inst, m, moved)) = &mut self.marker_drag
            && let Some(p) = response.interact_pointer_pos()
        {
            self.editor.move_marker(*inst, *m, frame_at(p.x), !*moved);
            *moved = true;
        }
        if response.drag_stopped() {
            self.marker_drag = None;
        }
        if let Some(pos) = response.interact_pointer_pos() {
            let frame = frame_at(pos.x);
            if response.secondary_clicked() {
                if let Some(m) = sample.marker_near(frame, within) {
                    self.editor.delete_marker(instrument, m);
                }
            } else if response.clicked() && modifiers.shift {
                if let Some(k) = self.editor.insert_marker(instrument, frame) {
                    self.editor.last_slice = k as u16;
                    self.editor.audition = Some((instrument, k as u16));
                }
            } else if response.clicked() {
                let slice = sample
                    .slices
                    .partition_point(|&s| s <= frame)
                    .saturating_sub(1) as u16;
                if modifiers.command {
                    self.editor.apply(Action::EnterSlice(slice));
                } else {
                    self.editor.last_slice = slice;
                    self.editor.audition = Some((instrument, slice));
                }
            }
        }

        // Draw the sample as it is now, after any edit above.
        let sample = self
            .editor
            .track_sample()
            .map_or(sample, |(_, s)| s.clone());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 4.0, color::PANEL);
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
        // Markers you can grab get thicker under the pointer.
        let hover = response
            .hover_pos()
            .and_then(|h| sample.marker_near(frame_at(h.x), within));
        for (i, &start) in sample.slices.iter().enumerate() {
            let x = x_of(start);
            let active =
                self.marker_drag.is_some_and(|(_, m, _)| m == i) || (hover == Some(i) && i > 0);
            let width = if active { 3.0 } else { 1.0 };
            p.line_segment(
                [pos2(x, rect.top()), pos2(x, rect.bottom())],
                Stroke::new(width, color::ACCENT),
            );
            p.text(
                pos2(x + 3.0, rect.top() + 2.0),
                Align2::LEFT_TOP,
                format!("{i:02X}"),
                small.clone(),
                color::ACCENT,
            );
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

        // Track names, and the sample each plays.
        for (t, track) in pattern.tracks.iter().enumerate() {
            let c = color::TRACKS[t % color::TRACKS.len()];
            let r = p.text(
                pos2(track_x(t), rect.top() + 2.0),
                Align2::LEFT_TOP,
                &track.name,
                font.clone(),
                c,
            );
            if let Some(inst) = e.song.instruments.get(track.instrument) {
                let room = (TRACK_CHARS + TRACK_GAP - 2).saturating_sub(track.name.chars().count());
                let name: String = inst.name().chars().take(room).collect();
                p.text(
                    r.right_top() + vec2(cw, 0.0),
                    Align2::LEFT_TOP,
                    name,
                    font.clone(),
                    color::DIM,
                );
            }
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
                let synth = e
                    .song
                    .instruments
                    .get(track.instrument)
                    .is_some_and(Instrument::is_synth);
                for (field, text, c) in cell_fields(&track.cells[row], track_color, synth) {
                    let pos = pos2(x + cw * field_offset(field) as f32, y + 1.0);
                    p.text(pos, Align2::LEFT_TOP, text, font.clone(), c);
                }
            }
        }

        // The cell a tutorial step is about.
        if let Focus::Cell { track, row } = self.tutorial_focus() {
            let i = row as isize - first;
            if (0..visible).contains(&i) && track < pattern.tracks.len() {
                let y = top + i as f32 * ROW_HEIGHT;
                let r = Rect::from_min_size(
                    pos2(track_x(track) - cw * 0.5, y),
                    vec2(cw * (TRACK_CHARS as f32 + 1.0), ROW_HEIGHT),
                );
                p.rect_stroke(
                    r,
                    2.0,
                    Stroke::new(2.0, color::ACCENT),
                    egui::StrokeKind::Outside,
                );
            }
        }

        // The cursor digit, drawn last so it sits on top.
        let c = e.cursor;
        let i = c.row as isize - first;
        if (0..visible).contains(&i) && c.track < pattern.tracks.len() {
            let x = track_x(c.track) + cw * (field_offset(c.field) + c.digit) as f32;
            let y = top + i as f32 * ROW_HEIGHT;
            // A synth's note is one cursor position, three characters wide.
            let synth = e.on_synth();
            let whole = synth && c.field == Field::Slice;
            let width = if whole { 3.0 } else { 1.0 };
            let cell = Rect::from_min_size(pos2(x, y), vec2(cw * width, ROW_HEIGHT));
            p.rect_filled(cell, 1.0, color::ACCENT);
            let text = cell_fields(&pattern.tracks[c.track].cells[c.row], color::BG, synth)
                .into_iter()
                .find(|(f, ..)| *f == c.field)
                .map(|(_, text, _)| text)
                .unwrap_or_default();
            let text: String = if whole {
                text
            } else {
                text.chars().nth(c.digit).into_iter().collect()
            };
            p.text(pos2(x, y + 1.0), Align2::LEFT_TOP, text, font, color::BG);
        }
    }

    /// A prompt or message that takes over the first status line.
    fn notice(&self) -> Option<(String, Color32)> {
        if let Some(prompt) = &self.prompt {
            let what = match prompt.kind {
                PromptKind::SaveAs => "save as",
                PromptKind::Open => "open",
                PromptKind::LoadSample => "load sample (WAV) onto this track",
                PromptKind::LoadTuning => {
                    "load tuning: a Scala .scl file, or just, stretched, slendro, pelog, 19-tet, 12-tet"
                }
                PromptKind::Export => "export the song to WAV",
            };
            return Some((
                format!("{what}: {}▏   enter to confirm, esc to cancel", prompt.text),
                color::ACCENT,
            ));
        }
        let m = self.editor.message.as_ref()?;
        let failed = m.starts_with("couldn't") || m.starts_with("can't") || m.contains("no free");
        Some((m.clone(), if failed { color::ERROR } else { color::ACCENT }))
    }

    fn rack_status(&self, ui: &mut egui::Ui) {
        let p = ui.painter();
        let r = ui.max_rect();
        let font = Self::font();
        let (text, c) = match (self.notice(), &self.rack_view.message) {
            (Some(n), _) => n,
            (None, Some(m)) => (m.clone(), color::ERROR),
            (None, None) => (
                format!(
                    "{} devices · {} cables",
                    self.editor.song.rack.devices.len(),
                    self.editor.song.rack.cables.len()
                ),
                color::TEXT,
            ),
        };
        p.text(
            r.left_top() + vec2(8.0, 4.0),
            Align2::LEFT_TOP,
            text,
            font.clone(),
            c,
        );
        let hints = "drag between jacks to patch · drag a plug out to unplug · drag knobs (shift: fine) · double-click resets · ctrl+z/y undo/redo";
        p.text(
            r.left_top() + vec2(8.0, 24.0),
            Align2::LEFT_TOP,
            hints,
            font,
            color::MUTED,
        );
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
        // A K effect is described by the knob it turns, when there is one.
        let target = e
            .song
            .automation(e.cursor.track)
            .and_then(|t| self.target_spec(t));
        words.extend(cell.fx().map(|fx| match (fx, &target) {
            (Fx::Knob(n), Some((device, spec))) => {
                format!(
                    "{device} {} → {}",
                    spec.name,
                    spec.format(spec.from_knob(n as f32 / 255.0))
                )
            }
            (Fx::Knob(_), None) => {
                "K does nothing yet: right-click a knob to pick what it turns".into()
            }
            _ => fx.describe(),
        }));
        let (text, c) = self.notice().unwrap_or((words.join(" · "), color::TEXT));
        p.text(
            r.left_top() + vec2(8.0, 4.0),
            Align2::LEFT_TOP,
            text,
            font.clone(),
            c,
        );

        // The essentials; ? lists everything else.
        let hints = [
            ("?", "all commands"),
            ("F12", "tutorial"),
            ("space", "play/stop"),
            ("0-f", "enter"),
            ("ORVPBSC", "effect"),
            ("shift+↕", "select"),
            ("ctrl+z/y", "undo/redo"),
            ("ctrl+s", "save"),
            ("ctrl+e", "export wav"),
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
/// On synth tracks the first field is a note name, and a note-off (a cut
/// with no note) shows as `===`.
fn cell_fields(cell: &Cell, track_color: Color32, synth: bool) -> Vec<(Field, String, Color32)> {
    let mut out = Vec::with_capacity(4);
    match cell.trigger {
        Some(t) if synth => {
            out.push((Field::Slice, note_name(t.slice.min(127) as u8), track_color));
            out.push((Field::Velocity, format!("{:02X}", t.velocity), color::TEXT));
        }
        Some(t) => {
            out.push((
                Field::Slice,
                format!("{:02X}", t.slice.min(0xff)),
                track_color,
            ));
            out.push((Field::Velocity, format!("{:02X}", t.velocity), color::TEXT));
        }
        None => {
            let note_off = synth && cell.fx[0] == Some(crate::pattern::Fx::Cut(0));
            let first = if note_off {
                ("===", color::MUTED)
            } else if synth {
                ("···", color::DIM)
            } else {
                ("··", color::DIM)
            };
            out.push((Field::Slice, first.0.into(), first.1));
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
        // A scripted screenshot mustn't pick up whatever the person at the
        // computer happens to type or click while the window is up.
        #[cfg(feature = "screenshot")]
        if self.screenshot.is_some() {
            ctx.input_mut(|i| {
                i.events.clear();
                i.pointer = Default::default();
            });
        }
        self.handle_input(&ctx);
        self.handle_midi(&ctx);
        self.sync_engine();

        let title = format!("{} - cowbell", self.project_name());
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.unsaved() && !self.close_armed {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_armed = true;
            self.editor.message = Some(
                "couldn't close: unsaved changes. ctrl+s to save, or close again to discard them"
                    .into(),
            );
        }

        self.update_tutorial();
        let frame = |fill| egui::Frame::new().fill(fill);
        let mut focus_rect = None;
        let focus = self.tutorial_focus();
        let transport = egui::Panel::top("transport")
            .exact_size(30.0)
            .frame(frame(color::BG))
            .show(ui, |ui| self.transport(ui))
            .response
            .rect;
        if focus == Focus::Transport {
            focus_rect = Some(transport);
        }
        match self.view {
            View::Tracker => {
                let slicer = egui::Panel::top("slicer")
                    .exact_size(130.0)
                    .frame(frame(color::BG))
                    .show(ui, |ui| self.instrument_panel(ui))
                    .response
                    .rect;
                egui::Panel::bottom("status")
                    .exact_size(64.0)
                    .frame(frame(color::PANEL))
                    .show(ui, |ui| self.status(ui));
                if self.tutorial.is_some() {
                    egui::Panel::bottom("tutorial")
                        .exact_size(84.0)
                        .frame(frame(color::CURSOR_ROW))
                        .show(ui, |ui| self.tutorial_strip(ui));
                }
                let grid = egui::CentralPanel::default()
                    .frame(frame(color::BG))
                    .show(ui, |ui| self.grid(ui))
                    .response
                    .rect;
                match focus {
                    Focus::Slicer => focus_rect = Some(slicer),
                    Focus::Grid => focus_rect = Some(grid),
                    _ => {}
                }
            }
            View::Rack => {
                egui::Panel::bottom("rack-status")
                    .exact_size(48.0)
                    .frame(frame(color::PANEL))
                    .show(ui, |ui| self.rack_status(ui));
                if self.tutorial.is_some() {
                    egui::Panel::bottom("tutorial")
                        .exact_size(84.0)
                        .frame(frame(color::CURSOR_ROW))
                        .show(ui, |ui| self.tutorial_strip(ui));
                }
                egui::CentralPanel::default()
                    .frame(frame(color::BG))
                    .show(ui, |ui| {
                        let names: Vec<String> = self
                            .editor
                            .pattern()
                            .tracks
                            .iter()
                            .map(|t| t.name.clone())
                            .collect();
                        let t = self.editor.cursor.track;
                        let automated = match self.editor.song.automation(t) {
                            Some(Target::Rack { device, param }) => Some((device, param)),
                            _ => None,
                        };
                        let out =
                            self.rack_view
                                .show(ui, &mut self.editor.song.rack, &names, automated);
                        if let Some((device, param)) = out.bind {
                            let target = Target::Rack { device, param };
                            self.bind_automation(target);
                        }
                        for (device, param, value) in out.params {
                            let _ = self.controller.send(Command::SetParam {
                                device,
                                param,
                                value,
                            });
                        }
                        if let Some(before) = out.undo {
                            self.editor.record_rack(before);
                        }
                    });
                // Cables follow the pointer while dragging.
                if ui.input(|i| i.pointer.any_down()) {
                    ctx.request_repaint();
                }
            }
        }

        let overlay = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::unique("focus"),
        ));
        if let Some(r) = focus_rect {
            overlay.rect_stroke(
                r.shrink(2.0),
                4.0,
                Stroke::new(2.0, color::ACCENT),
                egui::StrokeKind::Inside,
            );
        }
        if let Some(p) = &self.palette {
            p.draw(&ctx, &Self::font());
        }

        if self.controller.is_playing() || self.tutorial.is_some() {
            ctx.request_repaint();
        }
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
    fn clipboard_keys_reach_the_editor() {
        let ctrl = Modifiers::COMMAND;
        for (key, want) in [
            (Key::C, Action::Copy),
            (Key::X, Action::Cut),
            (Key::V, Action::Paste),
        ] {
            assert!(
                matches!(key_act(key, ctrl), Some(Act::Edit(a)) if a == want),
                "{key:?}"
            );
        }
    }

    #[test]
    fn typed_paths_get_the_extension() {
        assert_eq!(project_path("song"), PathBuf::from("song.cowbell"));
        assert_eq!(
            project_path(" song.cowbell "),
            PathBuf::from("song.cowbell")
        );
        assert_eq!(project_path("a.b"), PathBuf::from("a.b.cowbell"));
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(project_path("~/x"), Path::new(&home).join("x.cowbell"));
        }
    }
}
