//! The audio engine: a sample-accurate row scheduler driving one voice per
//! track, with the track outputs running through the rack's [`Graph`].
//!
//! The engine lives on the audio thread. Everything else talks to it through
//! a [`Controller`], over lock-free queues. The engine never allocates or
//! frees memory: a replaced song or graph is sent back over a queue so the
//! controller thread can drop it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

use crate::graph::{BLOCK, Graph};
use crate::pattern::{Fx, Instrument, Song, Trigger};
use crate::rack::DeviceId;
use crate::voice::Voice;

pub const MAX_TRACKS: usize = 16;
/// One voice per track, plus one for auditioning slices from the UI.
const AUDITION: usize = MAX_TRACKS;

pub enum Command {
    Play,
    /// Start playing at this row.
    PlayFrom(Position),
    Stop,
    /// Preview a slice, independent of the song.
    Audition {
        instrument: usize,
        slice: u16,
        velocity: u8,
    },
    SetBpm(f64),
    /// Replace the song. Playback continues from the same order position and
    /// row, as far as the new song has them. Notes keep ringing if their
    /// sample is the same in the new song.
    SwapSong(Box<Song>),
    /// Replace the device graph, after the rack was re-patched. Filter,
    /// delay and LFO state carries over to devices that are still there.
    SwapGraph(Box<Graph>),
    /// Turn a knob. Doesn't need a new graph.
    SetParam {
        device: DeviceId,
        param: usize,
        value: f32,
    },
}

/// Things the engine is done with, for the controller to free.
pub enum Garbage {
    Song(#[allow(dead_code)] Box<Song>),
    Graph(#[allow(dead_code)] Box<Graph>),
}

/// Where the engine is: `(order index, row)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub order: usize,
    pub row: usize,
}

pub struct Engine {
    out_rate: f64,
    song: Box<Song>,
    playing: bool,
    pos: Position,
    /// Output frames until the next row starts. Fractional, so rows land on
    /// the right frame even when a row isn't a whole number of frames long.
    row_clock: f64,
    voices: [Voice; MAX_TRACKS + 1],
    graph: Box<Graph>,
    /// The audition voice skips the graph and goes straight to the output.
    audition: Vec<[f32; 2]>,
    commands: Consumer<Command>,
    trash: Producer<Garbage>,
    shared_pos: Arc<AtomicU64>,
    shared_playing: Arc<AtomicBool>,
}

pub struct Controller {
    commands: Producer<Command>,
    trash: Consumer<Garbage>,
    shared_pos: Arc<AtomicU64>,
    shared_playing: Arc<AtomicBool>,
}

impl Controller {
    /// Send a command to the audio thread. Gives it back if the queue is full.
    pub fn send(&mut self, cmd: Command) -> Result<(), Command> {
        self.collect_garbage();
        self.commands
            .push(cmd)
            .map_err(|rtrb::PushError::Full(cmd)| cmd)
    }

    /// Drop songs and graphs the engine has finished with. Call this
    /// regularly.
    pub fn collect_garbage(&mut self) {
        while self.trash.pop().is_ok() {}
    }

    /// The row being played, for drawing the playhead.
    pub fn position(&self) -> Position {
        unpack(self.shared_pos.load(Ordering::Relaxed))
    }

    pub fn is_playing(&self) -> bool {
        self.shared_playing.load(Ordering::Relaxed)
    }
}

fn pack(p: Position) -> u64 {
    (p.order as u64) << 32 | p.row as u64 & 0xffff_ffff
}

fn unpack(v: u64) -> Position {
    Position {
        order: (v >> 32) as usize,
        row: (v & 0xffff_ffff) as usize,
    }
}

impl Engine {
    pub fn new(out_rate: u32, song: Song) -> (Engine, Controller) {
        let (cmd_tx, cmd_rx) = RingBuffer::new(64);
        let (trash_tx, trash_rx) = RingBuffer::new(64);
        let shared_pos = Arc::new(AtomicU64::new(0));
        let shared_playing = Arc::new(AtomicBool::new(false));
        let graph = Box::new(Graph::build(&song.rack, out_rate));
        let engine = Engine {
            out_rate: out_rate as f64,
            song: Box::new(song),
            playing: false,
            pos: Position { order: 0, row: 0 },
            row_clock: 0.0,
            voices: Default::default(),
            graph,
            audition: vec![[0.0; 2]; BLOCK],
            commands: cmd_rx,
            trash: trash_tx,
            shared_pos: shared_pos.clone(),
            shared_playing: shared_playing.clone(),
        };
        let controller = Controller {
            commands: cmd_tx,
            trash: trash_rx,
            shared_pos,
            shared_playing,
        };
        (engine, controller)
    }

    pub fn play(&mut self) {
        self.set_playing(true);
    }

    fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        self.shared_playing.store(playing, Ordering::Relaxed);
    }

    pub fn position(&self) -> Position {
        self.pos
    }

    fn frames_per_row(&self) -> f64 {
        let rpb = self
            .song
            .order
            .get(self.pos.order)
            .and_then(|&p| self.song.patterns.get(p));
        let rpb = rpb.map_or(4, |p| p.rows_per_beat.max(1)) as f64;
        self.out_rate * 60.0 / (self.song.bpm.max(1.0) * rpb)
    }

    fn handle_commands(&mut self) {
        while let Ok(cmd) = self.commands.pop() {
            match cmd {
                Command::Play => self.set_playing(true),
                Command::PlayFrom(pos) => {
                    self.pos = pos;
                    if self.pos.order >= self.song.order.len() {
                        self.pos = Position { order: 0, row: 0 };
                    }
                    self.row_clock = 0.0;
                    self.voices.iter_mut().for_each(Voice::stop);
                    self.set_playing(true);
                }
                Command::Stop => {
                    self.set_playing(false);
                    self.pos = Position { order: 0, row: 0 };
                    self.row_clock = 0.0;
                    self.voices.iter_mut().for_each(Voice::stop);
                }
                Command::Audition {
                    instrument,
                    slice,
                    velocity,
                } => {
                    if let Some(Instrument::Sampler(sample)) = self.song.instruments.get(instrument)
                    {
                        let trigger = Trigger { slice, velocity };
                        let fpr = self.frames_per_row();
                        self.voices[AUDITION].trigger(
                            instrument,
                            sample,
                            trigger,
                            std::iter::empty(),
                            self.out_rate,
                            fpr,
                        );
                    }
                }
                Command::SetBpm(bpm) => self.song.bpm = bpm,
                Command::SwapSong(song) => {
                    let old = std::mem::replace(&mut self.song, song);
                    // A voice indexes into its sample's frames, so it can only
                    // keep playing if the new song has the very same sample.
                    for voice in self.voices.iter_mut().filter(|v| v.is_active()) {
                        let same = match (
                            old.instruments.get(voice.instrument()),
                            self.song.instruments.get(voice.instrument()),
                        ) {
                            (Some(Instrument::Sampler(a)), Some(Instrument::Sampler(b))) => {
                                Arc::ptr_eq(a, b)
                            }
                            _ => false,
                        };
                        if !same {
                            voice.stop();
                        }
                    }
                    if self.pos.order >= self.song.order.len() {
                        self.pos = Position { order: 0, row: 0 };
                    }
                    self.discard(Garbage::Song(old));
                }
                Command::SwapGraph(mut graph) => {
                    graph.adopt_state(&mut self.graph);
                    let old = std::mem::replace(&mut self.graph, graph);
                    self.discard(Garbage::Graph(old));
                }
                Command::SetParam {
                    device,
                    param,
                    value,
                } => self.graph.set_param(device, param, value),
            }
        }
    }

    fn discard(&mut self, garbage: Garbage) {
        // The queue is as deep as the command queue, so this only fails if
        // the controller never collects garbage. Leaking is better than
        // freeing on the audio thread.
        if let Err(rtrb::PushError::Full(g)) = self.trash.push(garbage) {
            std::mem::forget(g);
        }
    }

    /// Fire the cells of the current row.
    fn fire_row(&mut self) {
        let Some(pattern) = self
            .song
            .order
            .get(self.pos.order)
            .and_then(|&p| self.song.patterns.get(p))
        else {
            return;
        };
        let fpr = self.frames_per_row();
        for (track, voice) in pattern.tracks.iter().zip(self.voices.iter_mut()) {
            let Some(cell) = track.cells.get(self.pos.row) else {
                continue;
            };
            let Some(Instrument::Sampler(sample)) = self.song.instruments.get(track.instrument)
            else {
                continue;
            };
            if let Some(trigger) = cell.trigger {
                voice.trigger(
                    track.instrument,
                    sample,
                    trigger,
                    cell.fx(),
                    self.out_rate,
                    fpr,
                );
            } else if let Some(Fx::Cut(n)) = cell.fx().find(|f| matches!(f, Fx::Cut(_))) {
                voice.schedule_cut(fpr * n as f64 / 16.0);
            }
        }
    }

    fn advance_row(&mut self) {
        let rows = self
            .song
            .order
            .get(self.pos.order)
            .and_then(|&p| self.song.patterns.get(p))
            .map_or(0, |p| p.rows);
        self.pos.row += 1;
        if self.pos.row >= rows {
            self.pos.row = 0;
            self.pos.order += 1;
            if self.pos.order >= self.song.order.len() {
                self.pos.order = 0;
            }
        }
    }

    /// Render interleaved stereo into `out`.
    pub fn process(&mut self, out: &mut [f32]) {
        self.handle_commands();
        for block in out.chunks_mut(2 * BLOCK) {
            let frames = block.len() / 2;
            self.render_tracks(frames);
            self.graph.process(frames);
            let master = self.graph.master();
            for ((o, m), a) in block
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(master)
                .zip(&self.audition)
            {
                *o = [m[0] + a[0], m[1] + a[1]];
            }
        }
    }

    /// Run the sequencer and voices for one block, into the graph's track
    /// inputs.
    fn render_tracks(&mut self, frames: usize) {
        self.graph.begin(frames);
        for i in 0..frames {
            if self.playing {
                while self.row_clock <= 0.0 {
                    self.fire_row();
                    self.shared_pos.store(pack(self.pos), Ordering::Relaxed);
                    self.advance_row();
                    self.row_clock += self.frames_per_row();
                }
                self.row_clock -= 1.0;
            }
            for (t, voice) in self.voices.iter_mut().enumerate() {
                let frame = match self.song.instruments.get(voice.instrument()) {
                    Some(Instrument::Sampler(sample)) if voice.is_active() => {
                        voice.next_frame(sample)
                    }
                    _ => [0.0; 2],
                };
                let hit = voice.take_hit();
                if t == AUDITION {
                    self.audition[i] = frame;
                } else {
                    self.graph.write_track(t, i, frame, hit);
                }
            }
        }
    }
}
