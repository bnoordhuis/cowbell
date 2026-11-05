//! The audio engine: a sample-accurate row scheduler driving one voice per
//! track.
//!
//! The engine lives on the audio thread. Everything else talks to it through
//! a [`Controller`], over lock-free queues. The engine never allocates or
//! frees memory: a replaced song is sent back over a queue so the controller
//! thread can drop it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

use crate::pattern::{Fx, Instrument, Song};
use crate::voice::Voice;

pub const MAX_TRACKS: usize = 16;

pub enum Command {
    Play,
    Stop,
    SetBpm(f64),
    /// Replace the song. Playback continues from the same order position and
    /// row, as far as the new song has them.
    SwapSong(Box<Song>),
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
    voices: [Voice; MAX_TRACKS],
    commands: Consumer<Command>,
    trash: Producer<Box<Song>>,
    shared_pos: Arc<AtomicU64>,
}

pub struct Controller {
    commands: Producer<Command>,
    trash: Consumer<Box<Song>>,
    shared_pos: Arc<AtomicU64>,
}

impl Controller {
    /// Send a command to the audio thread. Gives it back if the queue is full.
    pub fn send(&mut self, cmd: Command) -> Result<(), Command> {
        self.collect_garbage();
        self.commands.push(cmd).map_err(|rtrb::PushError::Full(cmd)| cmd)
    }

    /// Drop songs the engine has finished with. Call this regularly.
    pub fn collect_garbage(&mut self) {
        while self.trash.pop().is_ok() {}
    }

    /// The row being played, for drawing the playhead.
    pub fn position(&self) -> Position {
        unpack(self.shared_pos.load(Ordering::Relaxed))
    }
}

fn pack(p: Position) -> u64 {
    (p.order as u64) << 32 | p.row as u64 & 0xffff_ffff
}

fn unpack(v: u64) -> Position {
    Position { order: (v >> 32) as usize, row: (v & 0xffff_ffff) as usize }
}

impl Engine {
    pub fn new(out_rate: u32, song: Song) -> (Engine, Controller) {
        let (cmd_tx, cmd_rx) = RingBuffer::new(64);
        let (trash_tx, trash_rx) = RingBuffer::new(64);
        let shared_pos = Arc::new(AtomicU64::new(0));
        let engine = Engine {
            out_rate: out_rate as f64,
            song: Box::new(song),
            playing: false,
            pos: Position { order: 0, row: 0 },
            row_clock: 0.0,
            voices: Default::default(),
            commands: cmd_rx,
            trash: trash_tx,
            shared_pos: shared_pos.clone(),
        };
        let controller = Controller { commands: cmd_tx, trash: trash_rx, shared_pos };
        (engine, controller)
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    pub fn position(&self) -> Position {
        self.pos
    }

    fn frames_per_row(&self) -> f64 {
        let rpb = self.song.order.get(self.pos.order).and_then(|&p| self.song.patterns.get(p));
        let rpb = rpb.map_or(4, |p| p.rows_per_beat.max(1)) as f64;
        self.out_rate * 60.0 / (self.song.bpm.max(1.0) * rpb)
    }

    fn handle_commands(&mut self) {
        while let Ok(cmd) = self.commands.pop() {
            match cmd {
                Command::Play => self.playing = true,
                Command::Stop => {
                    self.playing = false;
                    self.pos = Position { order: 0, row: 0 };
                    self.row_clock = 0.0;
                    self.voices.iter_mut().for_each(Voice::stop);
                }
                Command::SetBpm(bpm) => self.song.bpm = bpm,
                Command::SwapSong(song) => {
                    let old = std::mem::replace(&mut self.song, song);
                    // Old voices may point at instruments or slices that no
                    // longer exist.
                    self.voices.iter_mut().for_each(Voice::stop);
                    if self.pos.order >= self.song.order.len() {
                        self.pos = Position { order: 0, row: 0 };
                    }
                    // The queue is as deep as the command queue, so this only
                    // fails if the controller never collects garbage. Leaking
                    // is better than freeing on the audio thread.
                    if let Err(rtrb::PushError::Full(old)) = self.trash.push(old) {
                        std::mem::forget(old);
                    }
                }
            }
        }
    }

    /// Fire the cells of the current row.
    fn fire_row(&mut self) {
        let Some(pattern) = self.song.order.get(self.pos.order).and_then(|&p| self.song.patterns.get(p))
        else {
            return;
        };
        let fpr = self.frames_per_row();
        for (track, voice) in pattern.tracks.iter().zip(self.voices.iter_mut()) {
            let Some(cell) = track.cells.get(self.pos.row) else { continue };
            let Some(Instrument::Sampler(sample)) = self.song.instruments.get(track.instrument) else {
                continue;
            };
            if let Some(trigger) = cell.trigger {
                voice.trigger(track.instrument, sample, trigger, cell.fx(), self.out_rate, fpr);
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
        for frame in out.chunks_exact_mut(2) {
            if self.playing {
                while self.row_clock <= 0.0 {
                    self.fire_row();
                    self.shared_pos.store(pack(self.pos), Ordering::Relaxed);
                    self.advance_row();
                    self.row_clock += self.frames_per_row();
                }
                self.row_clock -= 1.0;
            }
            let mut mix = [0.0f32; 2];
            for voice in self.voices.iter_mut().filter(|v| v.is_active()) {
                let Some(Instrument::Sampler(sample)) = self.song.instruments.get(voice.instrument()) else {
                    continue;
                };
                let [l, r] = voice.next_frame(sample);
                mix[0] += l;
                mix[1] += r;
            }
            frame[0] = mix[0];
            frame[1] = mix[1];
        }
    }
}
