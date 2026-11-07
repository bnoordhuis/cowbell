//! The audio engine: a sample-accurate tick clock firing each track's clip
//! rows, driving one voice per track, with the track outputs running through
//! the rack's [`Graph`].
//!
//! The engine lives on the audio thread. Everything else talks to it through
//! a [`Controller`], over lock-free queues. The engine never allocates or
//! frees memory: a replaced song or graph is sent back over a queue so the
//! controller thread can drop it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

use crate::graph::{BLOCK, Graph};
use crate::rack::DeviceId;
use crate::sample::Sample;
use crate::song::{
    Cell, Fx, Instrument, PEDAL_CHANGE, PEDAL_DOWN, Song, TICKS_PER_BEAT, Target, Tick, Trigger,
    pedal_at,
};
use crate::synthvoice::{MAX_CHORD, SynthTrack};
use crate::voice::Voice;

pub const MAX_TRACKS: usize = 16;
/// One voice per track, plus one for auditioning slices from the UI.
const AUDITION: usize = MAX_TRACKS;
/// Sample voices per track: a row's hits, or a poly track's ringing over
/// each other. At least `MAX_NOTES`.
const VOICES: usize = 6;

pub enum Command {
    /// Play from where the playhead is.
    Play,
    /// Start playing at this tick.
    PlayFrom(Tick),
    /// Stop sequencing. The playhead stays where it is.
    Stop,
    /// Move the playhead, playing or not.
    Locate(Tick),
    /// Preview a slice or synth note, independent of the song. A held
    /// synth note rings until [`Command::Release`]; otherwise it's short.
    Audition {
        instrument: usize,
        slice: u16,
        velocity: u8,
        hold: bool,
    },
    /// Release held audition notes.
    Release,
    /// Play a sample's first slice, outside the song, as when picking a
    /// file: a freshly loaded file is one slice, start to end. `None` stops
    /// it.
    Preview(Option<Arc<Sample>>),
    SetBpm(f64),
    /// Replace the song. Playback continues from the same tick, if the new
    /// song is that long. Notes keep ringing if their sample is the same in
    /// the new song.
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
    Sample(#[allow(dead_code)] Arc<Sample>),
}

pub struct Engine {
    out_rate: f64,
    song: Box<Song>,
    playing: bool,
    /// Whether the song starts over at the end, and the loop range loops.
    /// Off for exporting, where the sequencer stops and notes ring out.
    looping: bool,
    /// The next tick to fire.
    pos: Tick,
    /// Output frames until the next tick. Fractional, so ticks land on the
    /// right frame even when a tick isn't a whole number of frames long.
    tick_clock: f64,
    /// Sample voices per track, plus the audition's. A mono track's first
    /// voice plays its rows' first hits, so a break chopped one hit per row
    /// sounds exactly as it always has.
    voices: [[Voice; VOICES]; MAX_TRACKS + 1],
    /// When each voice was last started, to steal the oldest.
    started: [[u64; VOICES]; MAX_TRACKS + 1],
    starts: u64,
    /// Per track, the sound a voice was making when it was stolen or
    /// retriggered, fading out under the new note. Only used with declick.
    tails: [Voice; MAX_TRACKS],
    /// Synth voices per track, plus some for auditioning.
    synths: [SynthTrack; MAX_TRACKS + 1],
    graph: Box<Graph>,
    /// The audition voice skips the graph and goes straight to the output.
    audition: Vec<[f32; 2]>,
    /// A sample file playing outside the song, and its voice. It goes to
    /// the output with the audition.
    preview: Option<Arc<Sample>>,
    preview_voice: Voice,
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

    /// The tick last played (or located to), for drawing the playhead.
    pub fn position(&self) -> Tick {
        self.shared_pos.load(Ordering::Relaxed)
    }

    pub fn is_playing(&self) -> bool {
        self.shared_playing.load(Ordering::Relaxed)
    }
}

/// Output frames per row. Shared with [`crate::bounce`], which must agree
/// with the engine to the last bit.
pub fn frames_per_row(out_rate: f64, bpm: f64, rows_per_beat: u32) -> f64 {
    out_rate * 60.0 / (bpm.max(1.0) * rows_per_beat.max(1) as f64)
}

/// Output frames per tick. Shared with [`crate::bounce`] too.
pub fn frames_per_tick(out_rate: f64, bpm: f64) -> f64 {
    out_rate * 60.0 / (bpm.max(1.0) * TICKS_PER_BEAT as f64)
}

impl Engine {
    pub fn new(out_rate: u32, song: Song) -> (Engine, Controller) {
        let (cmd_tx, cmd_rx) = RingBuffer::new(64);
        let (trash_tx, trash_rx) = RingBuffer::new(64);
        let shared_pos = Arc::new(AtomicU64::new(0));
        let shared_playing = Arc::new(AtomicBool::new(false));
        let mut graph = Box::new(Graph::build(&song.rack, out_rate));
        graph.set_tempo(song.bpm);
        let engine = Engine {
            out_rate: out_rate as f64,
            song: Box::new(song),
            playing: false,
            looping: true,
            pos: 0,
            tick_clock: 0.0,
            voices: Default::default(),
            started: [[0; VOICES]; MAX_TRACKS + 1],
            starts: 0,
            tails: Default::default(),
            synths: std::array::from_fn(|_| SynthTrack::new(out_rate as f64)),
            graph,
            audition: vec![[0.0; 2]; BLOCK],
            preview: None,
            preview_voice: Voice::default(),
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

    /// Play the song once instead of looping it, ignoring the loop range.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn play(&mut self) {
        self.set_playing(true);
    }

    fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
        self.shared_playing.store(playing, Ordering::Relaxed);
    }

    pub fn position(&self) -> Tick {
        self.pos
    }

    fn locate(&mut self, tick: Tick) {
        self.pos = if tick < self.song.end() { tick } else { 0 };
        self.tick_clock = 0.0;
        self.shared_pos.store(self.pos, Ordering::Relaxed);
    }

    fn handle_commands(&mut self) {
        while let Ok(cmd) = self.commands.pop() {
            match cmd {
                Command::Play => self.set_playing(true),
                Command::PlayFrom(tick) => {
                    self.locate(tick);
                    self.stop_voices();
                    self.set_playing(true);
                }
                Command::Locate(tick) => {
                    self.locate(tick);
                    self.stop_voices();
                }
                Command::Stop => {
                    self.set_playing(false);
                    self.tick_clock = 0.0;
                    self.stop_voices();
                }
                Command::Release => self.synths[AUDITION].note_off_in(0.0),
                Command::Preview(sample) => {
                    self.preview_voice.stop();
                    if let Some(sample) = &sample {
                        let fpr = frames_per_row(self.out_rate, self.song.bpm, 4);
                        let first = Trigger::new(0, 127);
                        self.preview_voice.trigger(
                            0,
                            sample,
                            first,
                            std::iter::empty(),
                            self.out_rate,
                            fpr,
                        );
                    }
                    if let Some(old) = std::mem::replace(&mut self.preview, sample) {
                        self.discard(Garbage::Sample(old));
                    }
                }
                Command::Audition {
                    instrument,
                    slice,
                    velocity,
                    hold,
                } => match self.song.instruments.get(instrument) {
                    Some(Instrument::Sampler(sample)) => {
                        let trigger = Trigger::new(slice, velocity);
                        // Effects are timed by rows; auditions have none, so
                        // any row length will do.
                        let fpr = frames_per_row(self.out_rate, self.song.bpm, 4);
                        self.voices[AUDITION][0].trigger(
                            instrument,
                            sample,
                            trigger,
                            std::iter::empty(),
                            self.out_rate,
                            fpr,
                        );
                    }
                    Some(Instrument::Synth(_)) => {
                        let freq = self.song.tuning.freq(slice.min(127) as u8);
                        let synth = &mut self.synths[AUDITION];
                        synth.note_on(instrument, &[freq], velocity, 0.0, self.out_rate);
                        if !hold {
                            synth.note_off_in(0.3 * self.out_rate);
                        }
                    }
                    None => {}
                },
                Command::SetBpm(bpm) => {
                    self.song.bpm = bpm;
                    self.graph.set_tempo(bpm);
                }
                Command::SwapSong(song) => {
                    let old = std::mem::replace(&mut self.song, song);
                    self.graph.set_tempo(self.song.bpm);
                    // A voice indexes into its sample's frames, so it can only
                    // keep playing if the new song has the very same sample.
                    // Synths read their patch fresh every frame, so they only
                    // need their instrument to still be a synth.
                    let instruments = &self.song.instruments;
                    for synth in &mut self.synths {
                        synth.retain(|i| instruments.get(i).is_some_and(|i| i.is_synth()));
                    }
                    let voices = self
                        .voices
                        .iter_mut()
                        .flatten()
                        .chain(self.tails.iter_mut());
                    for voice in voices.filter(|v| v.is_active()) {
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
                    if self.pos >= self.song.end() {
                        self.locate(0);
                    }
                    self.discard(Garbage::Song(old));
                }
                Command::SwapGraph(mut graph) => {
                    graph.adopt_state(&mut self.graph);
                    graph.set_tempo(self.song.bpm);
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

    /// Silence everything and undo automation: rack knobs go back to where
    /// the rack has them, synth knobs to their patches (in `SynthTrack::stop`).
    fn stop_voices(&mut self) {
        self.voices.iter_mut().flatten().for_each(Voice::stop);
        self.tails.iter_mut().for_each(Voice::stop);
        self.synths.iter_mut().for_each(SynthTrack::stop);
        for d in &self.song.rack.devices {
            for (param, &value) in d.params.iter().enumerate() {
                self.graph.set_param(d.id, param, value);
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

    /// Fire every track's row that starts on the current tick.
    fn fire_tick(&mut self) {
        let tracks = self.song.tracks.len().min(MAX_TRACKS);
        for t in 0..tracks {
            let track = &self.song.tracks[t];
            let synth = &mut self.synths[t];
            let Some(clip) = track.clip_at(self.pos) else {
                // The pedal goes up between clips.
                if synth.pedal() {
                    synth.set_pedal(false);
                }
                continue;
            };
            let Some(row) = clip.row_at(self.pos) else {
                continue;
            };
            // Worked out from the clip, rather than remembered, so it's
            // right however playback got here. Before the row's notes, so
            // they ring on after a change.
            // Down when it already is (a clip starting again, say) is a
            // change too.
            let pedal = clip.cells[row].pedal();
            if pedal == Some(PEDAL_CHANGE) || (pedal == Some(PEDAL_DOWN) && synth.pedal()) {
                synth.change_pedal();
            } else {
                let down = pedal_at(&clip.cells, row);
                if down != synth.pedal() {
                    synth.set_pedal(down);
                }
            }
            let fpr = frames_per_row(self.out_rate, self.song.bpm, clip.rows_per_beat);
            let (cell, instrument, poly) = (clip.cells[row], track.instrument, track.poly);
            self.fire_cell(t, instrument, cell, fpr, poly);
        }
    }

    /// Fire one cell on track `t`, `fpr` frames per row. On a mono track its
    /// notes take over from what was sounding; on a poly one they join it.
    fn fire_cell(&mut self, t: usize, instrument: usize, cell: Cell, fpr: f64, poly: bool) {
        let pool = &mut self.voices[t];
        let tail = &mut self.tails[t];
        let synth = &mut self.synths[t];
        let cut = cell.fx().find_map(|f| match f {
            Fx::Cut(n) => Some(fpr * n as f64 / 16.0),
            _ => None,
        });
        let knob = cell.fx().find_map(|f| match f {
            Fx::Knob(n) => Some(n as f32 / 255.0),
            _ => None,
        });
        match (knob, self.song.automation(t)) {
            (Some(k), Some(Target::Rack { device, param })) => {
                let spec = self
                    .song
                    .rack
                    .device(device)
                    .and_then(|d| d.kind.params().get(param));
                if let Some(spec) = spec {
                    self.graph.set_param(device, param, spec.from_knob(k));
                }
            }
            (Some(k), Some(Target::Synth { param })) => {
                if let Some(Instrument::Synth(patch)) = self.song.instruments.get(instrument)
                    && let Some(spec) = patch.kind.params().get(param)
                {
                    synth.set_override(param, spec.from_knob(k));
                }
            }
            _ => {}
        }
        let sample = match self.song.instruments.get(instrument) {
            Some(Instrument::Sampler(sample)) => sample,
            Some(Instrument::Synth(_)) => {
                if cell.has_hit() {
                    // The track switched from a sample: that stops.
                    pool.iter_mut().for_each(Voice::stop);
                    let glide = cell.fx().find_map(|f| match f {
                        Fx::Glide(n) => Some(n as f32 * 4.0),
                        _ => None,
                    });
                    let chord = cell.fx().find_map(|f| match f {
                        Fx::Chord(n) => Some(n),
                        _ => None,
                    });
                    let tuning = &self.song.tuning;
                    // A note, plus any chord notes, each through the tuning.
                    let with_chord = |root: u16, freqs: &mut [f64; MAX_CHORD], n: &mut usize| {
                        let root = root.min(127) as u8;
                        let notes = std::iter::once(0)
                            .chain(chord.into_iter().flat_map(Fx::chord_intervals));
                        for interval in notes {
                            if *n == MAX_CHORD {
                                break;
                            }
                            freqs[*n] = tuning.freq(root.saturating_add(interval).min(127));
                            *n += 1;
                        }
                    };
                    let hold = |rows: u16| (rows > 0).then_some(rows as f64 * fpr);
                    if poly {
                        for hit in cell.hits() {
                            let (mut freqs, mut n) = ([0.0; MAX_CHORD], 0);
                            with_chord(hit.slice, &mut freqs, &mut n);
                            synth.play(
                                instrument,
                                &freqs[..n],
                                hit.velocity,
                                hold(hit.rows),
                                self.out_rate,
                            );
                        }
                    } else {
                        // The row's notes sound together, led by the first.
                        let (mut freqs, mut n) = ([0.0; MAX_CHORD], 0);
                        for hit in cell.hits() {
                            with_chord(hit.slice, &mut freqs, &mut n);
                        }
                        let first = cell.first().unwrap();
                        synth.note_on(
                            instrument,
                            &freqs[..n],
                            first.velocity,
                            glide.unwrap_or(0.0),
                            self.out_rate,
                        );
                        if let Some(frames) = hold(first.rows) {
                            synth.note_off_in(frames);
                        }
                    }
                }
                if let Some(frames) = cut {
                    if poly {
                        synth.release_all(frames);
                    } else {
                        synth.note_off_in(frames);
                    }
                }
                return;
            }
            None => return,
        };
        if cell.has_hit() && synth.is_active() {
            // The track switched from a synth: let it ring out.
            synth.release_all(0.0);
        }
        let declick = self.graph.track_declick(t);
        let started = &mut self.started[t];
        let trigger = |v: &mut Voice, hit: Trigger| {
            v.trigger(instrument, sample, hit, cell.fx(), self.out_rate, fpr);
            if hit.rows > 0 {
                v.schedule_cut(hit.rows as f64 * fpr);
            }
        };
        if !cell.has_hit() {
            if let Some(frames) = cut {
                pool[0].schedule_cut(frames);
                for v in pool[1..].iter_mut().filter(|v| v.is_active()) {
                    v.schedule_cut(frames);
                }
            }
        } else if poly {
            // Each hit on a voice of its own, the oldest if none is free.
            for hit in cell.hits() {
                let i = (0..VOICES)
                    .find(|&i| !pool[i].is_active())
                    .unwrap_or_else(|| (0..VOICES).min_by_key(|&i| started[i]).unwrap());
                if pool[i].is_active() && declick {
                    pool[i].steal_into(tail);
                }
                trigger(&mut pool[i], hit);
                self.starts += 1;
                started[i] = self.starts;
            }
        } else {
            // The row's hits take over: the first on the first voice, as a
            // single hit always has, the others alongside it.
            for v in pool[1..].iter_mut().filter(|v| v.is_active()) {
                v.schedule_cut(0.0);
            }
            for (k, hit) in cell.hits().enumerate() {
                if k == 0 && declick {
                    pool[0].steal_into(tail);
                }
                trigger(&mut pool[k], hit);
            }
        }
    }

    /// Step to the next tick: around the loop range, or back to the start
    /// at the end of the song (or stop there when not looping).
    fn advance_tick(&mut self) {
        self.pos += 1;
        if self.looping
            && let Some(range) = &self.song.loop_range
            && self.pos == range.end
            && range.start < range.end
        {
            self.pos = range.start;
            return;
        }
        if self.pos >= self.song.end() {
            self.pos = 0;
            if !self.looping {
                // Stop sequencing; voices keep ringing.
                self.set_playing(false);
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
        let mut audible = [false; MAX_TRACKS];
        for (t, a) in audible.iter_mut().enumerate() {
            *a = self.song.audible(t);
        }
        let fpt = frames_per_tick(self.out_rate, self.song.bpm);
        for i in 0..frames {
            if self.playing {
                while self.playing && self.tick_clock <= 0.0 {
                    self.fire_tick();
                    self.shared_pos.store(self.pos, Ordering::Relaxed);
                    self.advance_tick();
                    self.tick_clock += fpt;
                }
                self.tick_clock -= 1.0;
            }
            for (t, pool) in self.voices.iter_mut().enumerate() {
                let [voice, rest @ ..] = pool;
                let tail = self.tails.get_mut(t);
                let declick = tail.is_some() && self.graph.track_declick(t);
                let instruments = &self.song.instruments;
                let render = |v: &mut Voice| match instruments.get(v.instrument()) {
                    Some(Instrument::Sampler(sample)) if v.is_active() => {
                        v.next_frame(sample, declick)
                    }
                    _ => [0.0; 2],
                };
                let mut frame = [0.0; 2];
                if let Some(tail) = tail {
                    if declick && voice.retrig_due() {
                        voice.steal_into(tail);
                    }
                    // Only add a sounding tail, so declick off (or nothing
                    // to fade) leaves the voice's output bit for bit as is.
                    if tail.is_active() {
                        frame = render(tail);
                    }
                }
                let v = render(voice);
                if frame == [0.0; 2] {
                    frame = v;
                } else {
                    frame = [frame[0] + v[0], frame[1] + v[1]];
                }
                // The other voices only add anything while they sound, so a
                // track playing one hit at a time is bit for bit as before.
                let mut hit = false;
                for v in rest.iter_mut().filter(|v| v.is_active()) {
                    let s = render(v);
                    frame = [frame[0] + s[0], frame[1] + s[1]];
                    hit |= v.take_hit();
                }
                let synth = &mut self.synths[t];
                if synth.is_active() {
                    let patch = |i: usize| match instruments.get(i) {
                        Some(Instrument::Synth(p)) => Some(&**p),
                        _ => None,
                    };
                    let s = synth.next_frame(patch, self.out_rate);
                    frame = [frame[0] + s[0], frame[1] + s[1]];
                }
                let hit = hit | voice.take_hit() | synth.take_hit();
                if t == AUDITION {
                    if let Some(sample) = &self.preview
                        && self.preview_voice.is_active()
                    {
                        let p = self.preview_voice.next_frame(sample, false);
                        frame = [frame[0] + p[0], frame[1] + p[1]];
                    }
                    self.audition[i] = frame;
                } else if audible[t] {
                    self.graph.write_track(t, i, frame, hit);
                } else {
                    // Muted: the voices run on, unheard, so unmuting
                    // mid-note picks the note up where it is.
                    self.graph.write_track(t, i, [0.0; 2], false);
                }
            }
        }
    }
}
