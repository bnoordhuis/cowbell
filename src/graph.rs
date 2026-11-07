//! The runnable form of a [`Rack`]: devices in processing order, each with
//! preallocated buffers. Building a graph allocates, so it happens on the UI
//! thread; the audio thread only runs it.
//!
//! Every jack carries a stereo buffer. Control signals use the left channel.

use std::f32::consts::{PI, TAU};

use crate::engine::MAX_TRACKS;
use crate::rack::{DeviceId, Kind, Rack, SYNC_BEATS, TRACK_PAN};

/// Frames processed per call to [`Graph::process`].
pub const BLOCK: usize = 256;
/// How long a track's gate output stays high after a hit.
const GATE_MS: f32 = 5.0;
/// Longest delay time, plus headroom. A synced half note at 80 bpm fits.
const DELAY_MAX_S: f32 = 1.6;
/// The master's limiter leaves everything under this alone, and rounds
/// off what's over so it never passes [`LIMIT_CEILING`], just under full
/// scale.
const LIMIT_KNEE: f32 = 0.8;
const LIMIT_CEILING: f32 = 0.99;
/// The reverb's longest pre-delay, plus headroom.
const PRE_DELAY_MAX_S: f32 = 0.11;
/// Where the EQ's shelves turn.
const LOW_SHELF_HZ: f32 = 150.0;
const HIGH_SHELF_HZ: f32 = 6000.0;
/// The level, -12 dB, that the drive keeps as loud as it was.
const DRIVE_REFERENCE: f32 = 0.25;
/// The compressor's soft knee, in dB.
const KNEE_DB: f32 = 6.0;

type Buf = Vec<[f32; 2]>;

#[derive(Debug)]
enum State {
    None,
    Crusher {
        held: [f32; 2],
        count: u32,
    },
    Filter {
        ic1: [f32; 2],
        ic2: [f32; 2],
    },
    Delay {
        buf: Vec<[f32; 2]>,
        pos: usize,
    },
    Lfo {
        phase: f32,
        held: f32,
        rng: u32,
        last_clock: f32,
    },
    Drive {
        /// The tone filter, per channel.
        lp: [f32; 2],
    },
    Eq {
        /// Per band (low, mid, high), per channel: the biquad's two state
        /// values.
        z: [[[f32; 2]; 2]; 3],
    },
    Compressor {
        /// The level being followed, linear.
        env: f32,
        /// Whether the sidechain input is plugged in, so it does the
        /// ducking instead of the signal itself. Set before each block.
        side: bool,
    },
    Reverb(Box<Reverb>),
}

/// Freeverb (Jezar's): eight damped combs into four allpasses, per channel,
/// the right channel's a little longer for width. After a pre-delay.
#[derive(Debug)]
struct Reverb {
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<Allpass>; 2],
    pre: Vec<[f32; 2]>,
    pre_pos: usize,
}

#[derive(Debug)]
struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
}

#[derive(Debug)]
struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

/// Freeverb's tunings, in frames at 44.1 kHz.
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const STEREO_SPREAD: usize = 23;

impl Reverb {
    fn new(rate: f32) -> Reverb {
        let scale = |n: usize| ((n as f32 * rate / 44_100.0) as usize).max(1);
        let combs = |spread| {
            COMBS
                .iter()
                .map(|&n| Comb {
                    buf: vec![0.0; scale(n + spread)],
                    pos: 0,
                    store: 0.0,
                })
                .collect()
        };
        let allpasses = |spread| {
            ALLPASSES
                .iter()
                .map(|&n| Allpass {
                    buf: vec![0.0; scale(n + spread)],
                    pos: 0,
                })
                .collect()
        };
        Reverb {
            combs: [combs(0), combs(STEREO_SPREAD)],
            allpasses: [allpasses(0), allpasses(STEREO_SPREAD)],
            pre: vec![[0.0; 2]; (rate * PRE_DELAY_MAX_S) as usize],
            pre_pos: 0,
        }
    }
}

#[derive(Debug)]
struct Node {
    id: DeviceId,
    kind: Kind,
    params: Vec<f32>,
    /// Buffer index per input jack, or `None` if nothing is plugged in.
    inputs: Vec<Option<usize>>,
    /// Buffer index per output jack.
    outputs: Vec<usize>,
    state: State,
}

#[derive(Debug)]
pub struct Graph {
    rate: f32,
    nodes: Vec<Node>,
    bufs: Vec<Buf>,
    zero: Buf,
    /// Per track: (audio buffer, gate buffer).
    tracks: [Option<(usize, usize)>; MAX_TRACKS],
    gate_left: [u32; MAX_TRACKS],
    /// Per track: whether its voice fades instead of clicking.
    declick: [bool; MAX_TRACKS],
    /// Per track: the track device's volume and pan, as left and right
    /// gains.
    gain: [[f32; 2]; MAX_TRACKS],
    /// The tempo, for delays synced to it.
    bpm: f32,
    /// Where the master device writes its output.
    master: usize,
}

impl Graph {
    pub fn build(rack: &Rack, sample_rate: u32) -> Graph {
        let rate = sample_rate as f32;
        let mut bufs: Vec<Buf> = Vec::new();
        let new_buf = |bufs: &mut Vec<Buf>| {
            bufs.push(vec![[0.0; 2]; BLOCK]);
            bufs.len() - 1
        };
        // The master's own output, which the engine reads.
        let master = new_buf(&mut bufs);

        let order = rack.order();
        let mut nodes: Vec<Node> = order
            .iter()
            .map(|&id| {
                let d = rack.device(id).unwrap();
                let outputs = match d.kind {
                    Kind::Master => vec![master],
                    k => k.outputs().iter().map(|_| new_buf(&mut bufs)).collect(),
                };
                let state = match d.kind {
                    Kind::Crusher => State::Crusher {
                        held: [0.0; 2],
                        count: 0,
                    },
                    Kind::Filter => State::Filter {
                        ic1: [0.0; 2],
                        ic2: [0.0; 2],
                    },
                    Kind::Delay => State::Delay {
                        buf: vec![[0.0; 2]; (rate * DELAY_MAX_S) as usize],
                        pos: 0,
                    },
                    Kind::Lfo => State::Lfo {
                        phase: 0.0,
                        held: 0.0,
                        rng: 0x9e37_79b9 ^ id,
                        last_clock: 0.0,
                    },
                    Kind::Drive => State::Drive { lp: [0.0; 2] },
                    Kind::Eq => State::Eq {
                        z: [[[0.0; 2]; 2]; 3],
                    },
                    Kind::Compressor => State::Compressor {
                        env: 0.0,
                        side: false,
                    },
                    Kind::Reverb => State::Reverb(Box::new(Reverb::new(rate))),
                    _ => State::None,
                };
                Node {
                    id,
                    kind: d.kind,
                    params: d.params.clone(),
                    inputs: Vec::new(),
                    outputs,
                    state,
                }
            })
            .collect();

        let out_buf = |nodes: &[Node], device: DeviceId, port: usize| {
            nodes
                .iter()
                .find(|n| n.id == device)
                .and_then(|n| n.outputs.get(port).copied())
        };
        for i in 0..nodes.len() {
            let n = &nodes[i];
            let inputs = (0..n.kind.inputs().len())
                .map(|port| {
                    let c = rack.cable_into(crate::rack::Jack { device: n.id, port })?;
                    out_buf(&nodes, c.from.device, c.from.port)
                })
                .collect();
            nodes[i].inputs = inputs;
        }

        let mut tracks = [None; MAX_TRACKS];
        let mut declick = [true; MAX_TRACKS];
        let mut gain = [[1.0; 2]; MAX_TRACKS];
        for n in &nodes {
            if let Kind::Track(t) = n.kind
                && t < MAX_TRACKS
            {
                tracks[t] = Some((n.outputs[0], n.outputs[1]));
                declick[t] = declick_param(&n.params);
                gain[t] = track_gains(&n.params);
            }
        }
        Graph {
            rate,
            nodes,
            bufs,
            zero: vec![[0.0; 2]; BLOCK],
            tracks,
            gate_left: [0; MAX_TRACKS],
            declick,
            gain,
            bpm: 120.0,
            master,
        }
    }

    /// Set the tempo synced delays follow.
    pub fn set_tempo(&mut self, bpm: f64) {
        self.bpm = bpm.max(1.0) as f32;
    }

    /// Carry over filter, delay and LFO state from the graph this one
    /// replaces, so re-patching doesn't cut off delay tails. Doesn't allocate.
    pub fn adopt_state(&mut self, old: &mut Graph) {
        for n in &mut self.nodes {
            if let Some(o) = old
                .nodes
                .iter_mut()
                .find(|o| o.id == n.id && o.kind == n.kind)
            {
                std::mem::swap(&mut n.state, &mut o.state);
            }
        }
        self.gate_left = old.gate_left;
    }

    pub fn set_param(&mut self, device: DeviceId, param: usize, value: f32) {
        if let Some(p) = self
            .nodes
            .iter_mut()
            .find(|n| n.id == device)
            .and_then(|n| n.params.get_mut(param))
        {
            *p = value;
        }
        if let Some(n) = self.nodes.iter().find(|n| n.id == device)
            && let Kind::Track(t) = n.kind
            && t < MAX_TRACKS
        {
            self.declick[t] = declick_param(&n.params);
            self.gain[t] = track_gains(&n.params);
        }
    }

    /// Whether the track's voice should fade out on cuts, steals and
    /// retriggers, rather than stop dead.
    pub fn track_declick(&self, track: usize) -> bool {
        self.declick.get(track).copied().unwrap_or(true)
    }

    /// Clear the track inputs before the engine writes a new block.
    pub fn begin(&mut self, frames: usize) {
        for &(audio, gate) in self.tracks.iter().flatten() {
            self.bufs[audio][..frames].fill([0.0; 2]);
            self.bufs[gate][..frames].fill([0.0; 2]);
        }
    }

    /// Write one frame of track output. `hit` is true when the track's voice
    /// started or retriggered on this frame; it raises the gate.
    pub fn write_track(&mut self, track: usize, i: usize, frame: [f32; 2], hit: bool) {
        let Some(&Some((audio, gate))) = self.tracks.get(track) else {
            return;
        };
        let [l, r] = self.gain[track];
        self.bufs[audio][i] = [frame[0] * l, frame[1] * r];
        if hit {
            self.gate_left[track] = (self.rate * GATE_MS / 1000.0) as u32;
        }
        let left = &mut self.gate_left[track];
        if *left > 0 {
            *left -= 1;
            self.bufs[gate][i] = [1.0; 2];
        }
    }

    /// The master output of the last block.
    pub fn master(&self) -> &[[f32; 2]] {
        &self.bufs[self.master]
    }

    pub fn process(&mut self, frames: usize) {
        let Graph {
            rate,
            nodes,
            bufs,
            zero,
            master,
            bpm,
            ..
        } = self;
        let (rate, bpm) = (*rate, *bpm);
        let mut master_written = false;
        for n in nodes.iter_mut() {
            if matches!(n.kind, Kind::Track(_)) {
                continue;
            }
            // Take the output buffer out so inputs can be read while it's
            // written. Inputs never alias it: there are no loops. Moving a
            // Vec out with `take` doesn't allocate.
            let Some(&o) = n.outputs.first() else {
                continue;
            };
            let mut out = std::mem::take(&mut bufs[o]);
            if let State::Compressor { side, .. } = &mut n.state {
                *side = n.inputs.get(1).copied().flatten().is_some();
            }
            let input = |port: usize| -> &[[f32; 2]] {
                match n.inputs.get(port).copied().flatten() {
                    Some(b) => &bufs[b][..frames],
                    None => &zero[..frames],
                }
            };
            run(
                n.kind,
                &n.params,
                &mut n.state,
                &input,
                &mut out[..frames],
                rate,
                bpm,
            );
            bufs[o] = out;
            master_written |= o == *master;
        }
        if !master_written {
            bufs[*master][..frames].fill([0.0; 2]);
        }
    }
}

/// The track device's `declick` param. On if missing, like its default.
fn declick_param(params: &[f32]) -> bool {
    params.first().is_none_or(|&v| v >= 0.5)
}

/// The track device's volume and pan as left and right gains. In the
/// middle both are the volume exactly; panning turns the other side down.
fn track_gains(params: &[f32]) -> [f32; 2] {
    let volume = params.get(1).copied().unwrap_or(1.0);
    let pan = params
        .get(TRACK_PAN)
        .copied()
        .unwrap_or(0.0)
        .clamp(-1.0, 1.0);
    let side = |away: f32| (away.max(0.0) * PI / 2.0).cos();
    [volume * side(pan), volume * side(-pan)]
}

/// The master's limiter: the identity up to [`LIMIT_KNEE`], then a smooth
/// curve up to [`LIMIT_CEILING`].
fn limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= LIMIT_KNEE {
        return x;
    }
    let room = LIMIT_CEILING - LIMIT_KNEE;
    (LIMIT_KNEE + room * ((a - LIMIT_KNEE) / room).tanh()).copysign(x)
}

/// A biquad's coefficients, normalised: (b0, b1, b2, a1, a2).
type Biquad = [f32; 5];

/// RBJ cookbook shelves and peak. `kind`: 0 low shelf, 1 peak, 2 high shelf.
fn biquad(kind: u8, freq: f32, db: f32, rate: f32) -> Biquad {
    let a = 10f32.powf(db / 40.0);
    let w = TAU * freq.min(rate * 0.45) / rate;
    let (cos, sin) = (w.cos(), w.sin());
    let [b0, b1, b2, a0, a1, a2] = match kind {
        1 => {
            let alpha = sin / (2.0 * 0.9);
            [
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ]
        }
        _ => {
            // Shelf slope 1.
            let alpha = sin / 2.0 * 2f32.sqrt();
            let k = 2.0 * a.sqrt() * alpha;
            let s = if kind == 0 { 1.0 } else { -1.0 };
            [
                a * ((a + 1.0) - s * (a - 1.0) * cos + k),
                s * 2.0 * a * ((a - 1.0) - s * (a + 1.0) * cos),
                a * ((a + 1.0) - s * (a - 1.0) * cos - k),
                (a + 1.0) + s * (a - 1.0) * cos + k,
                -s * 2.0 * ((a - 1.0) + s * (a + 1.0) * cos),
                (a + 1.0) + s * (a - 1.0) * cos - k,
            ]
        }
    };
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// One sample through a biquad (transposed direct form II).
fn biquad_step(c: &Biquad, z: &mut [f32; 2], x: f32) -> f32 {
    let y = c[0] * x + z[0];
    z[0] = c[1] * x - c[3] * y + z[1];
    z[1] = c[2] * x - c[4] * y;
    y
}

/// One-pole smoothing coefficient for a time constant of `ms`.
fn coefficient(ms: f32, rate: f32) -> f32 {
    1.0 - (-1000.0 / (ms.max(0.01) * rate)).exp()
}

fn run<'a>(
    kind: Kind,
    p: &[f32],
    state: &mut State,
    input: &dyn Fn(usize) -> &'a [[f32; 2]],
    out: &mut [[f32; 2]],
    rate: f32,
    bpm: f32,
) {
    match (kind, state) {
        (Kind::Mixer, _) => {
            out.fill([0.0; 2]);
            for (ch, &level) in p.iter().enumerate() {
                for (o, i) in out.iter_mut().zip(input(ch)) {
                    o[0] += i[0] * level;
                    o[1] += i[1] * level;
                }
            }
        }
        (Kind::Master, _) => {
            let limiting = p.get(1).is_none_or(|&v| v >= 0.5);
            for (o, i) in out.iter_mut().zip(input(0)) {
                *o = [i[0] * p[0], i[1] * p[0]];
                if limiting {
                    *o = [limit(o[0]), limit(o[1])];
                }
            }
        }
        (Kind::Drive, State::Drive { lp }) => {
            let g = 10f32.powf(p[0] / 20.0);
            // Harder driving is louder: turn it back down so a signal at
            // -12 dB comes out as loud as it went in, whatever the drive.
            let level = DRIVE_REFERENCE / (DRIVE_REFERENCE * g).tanh();
            let a = 1.0 - (-TAU * p[1] / rate).exp();
            let (shape, mix) = (p[2] as u32, p[3]);
            for (o, i) in out.iter_mut().zip(input(0)) {
                for ch in 0..2 {
                    let x = i[ch] * g;
                    let y = match shape {
                        0 => x.tanh(),
                        1 => x.clamp(-1.0, 1.0),
                        _ => x.sin(),
                    } * level;
                    lp[ch] += (y - lp[ch]) * a;
                    o[ch] = i[ch] + (lp[ch] - i[ch]) * mix;
                }
            }
        }
        (Kind::Eq, State::Eq { z }) => {
            let bands = [
                (0, LOW_SHELF_HZ, p[0]),
                (1, p[2], p[1]),
                (2, HIGH_SHELF_HZ, p[3]),
            ];
            out.copy_from_slice(input(0));
            for (band, &(kind, freq, db)) in bands.iter().enumerate() {
                // A flat band leaves the sound exactly as it is.
                if db == 0.0 {
                    continue;
                }
                let c = biquad(kind, freq, db, rate);
                for o in out.iter_mut() {
                    for ch in 0..2 {
                        o[ch] = biquad_step(&c, &mut z[band][ch], o[ch]);
                    }
                }
            }
        }
        (Kind::Compressor, State::Compressor { env, side }) => {
            let (threshold, ratio) = (p[0], p[1].max(1.0));
            let (attack, release) = (coefficient(p[2], rate), coefficient(p[3], rate));
            let makeup = p[4];
            let slope = 1.0 / ratio - 1.0;
            let key = if *side { input(1) } else { input(0) };
            for ((o, i), k) in out.iter_mut().zip(input(0)).zip(key) {
                let level = k[0].abs().max(k[1].abs());
                let a = if level > *env { attack } else { release };
                *env += (level - *env) * a;
                let over = 20.0 * env.max(1e-6).log10() - threshold;
                let reduce = if over <= -KNEE_DB / 2.0 {
                    0.0
                } else if over < KNEE_DB / 2.0 {
                    slope * (over + KNEE_DB / 2.0).powi(2) / (2.0 * KNEE_DB)
                } else {
                    slope * over
                };
                let g = 10f32.powf((reduce + makeup) / 20.0);
                *o = [i[0] * g, i[1] * g];
            }
        }
        (Kind::Reverb, State::Reverb(r)) => {
            let feedback = p[0] * 0.28 + 0.7;
            let damp = p[1] * 0.4;
            let pre_len = r.pre.len();
            let pre = ((p[2] / 1000.0 * rate) as usize).min(pre_len - 1);
            let mix = p[3];
            for (o, i) in out.iter_mut().zip(input(0)) {
                r.pre[r.pre_pos] = *i;
                let d = r.pre[(r.pre_pos + pre_len - pre) % pre_len];
                r.pre_pos = (r.pre_pos + 1) % pre_len;
                let x = (d[0] + d[1]) * 0.015;
                for ch in 0..2 {
                    let mut wet = 0.0;
                    for c in &mut r.combs[ch] {
                        let y = c.buf[c.pos];
                        c.store = y * (1.0 - damp) + c.store * damp;
                        c.buf[c.pos] = x + c.store * feedback;
                        c.pos = (c.pos + 1) % c.buf.len();
                        wet += y;
                    }
                    for a in &mut r.allpasses[ch] {
                        let b = a.buf[a.pos];
                        a.buf[a.pos] = wet + b * 0.5;
                        a.pos = (a.pos + 1) % a.buf.len();
                        wet = b - wet;
                    }
                    o[ch] = i[ch] * (1.0 - mix) + wet * mix;
                }
            }
        }
        (Kind::Crusher, State::Crusher { held, count }) => {
            let levels = (1u32 << ((p[0] as u32).clamp(1, 16) - 1)) as f32;
            let every = (p[1] as u32).max(1);
            let mix = p[2];
            for (o, i) in out.iter_mut().zip(input(0)) {
                if *count == 0 {
                    *held = [
                        (i[0] * levels).round() / levels,
                        (i[1] * levels).round() / levels,
                    ];
                }
                *count = (*count + 1) % every;
                *o = [i[0] + (held[0] - i[0]) * mix, i[1] + (held[1] - i[1]) * mix];
            }
        }
        (Kind::Filter, State::Filter { ic1, ic2 }) => {
            // Trapezoidal state-variable filter (Simper/Cytomic).
            let (cutoff, reso, mode, amount) = (p[0], p[1], p[2] as u32, p[3]);
            let k = 2.0 - 2.0 * reso;
            let (sig, cv) = (input(0), input(1));
            for ((o, i), c) in out.iter_mut().zip(sig).zip(cv) {
                let fc = (cutoff * (c[0] * amount * 4.0).exp2()).clamp(20.0, rate * 0.45);
                let g = (PI * fc / rate).tan();
                let a1 = 1.0 / (1.0 + g * (g + k));
                let (a2, a3) = (g * a1, g * g * a1);
                for ch in 0..2 {
                    let v3 = i[ch] - ic2[ch];
                    let v1 = a1 * ic1[ch] + a2 * v3;
                    let v2 = ic2[ch] + a2 * ic1[ch] + a3 * v3;
                    ic1[ch] = 2.0 * v1 - ic1[ch];
                    ic2[ch] = 2.0 * v2 - ic2[ch];
                    o[ch] = match mode {
                        0 => v2,
                        1 => v1,
                        _ => i[ch] - k * v1 - v2,
                    };
                }
            }
        }
        (Kind::Delay, State::Delay { buf, pos }) => {
            let len = buf.len();
            let beats = SYNC_BEATS
                .get(p.get(3).copied().unwrap_or(0.0) as usize)
                .copied()
                .unwrap_or(0.0);
            let seconds = if beats > 0.0 {
                beats * 60.0 / bpm
            } else {
                p[0] / 1000.0
            };
            let delay = ((seconds * rate) as usize).clamp(1, len - 1);
            let (feedback, mix) = (p[1], p[2]);
            for (o, i) in out.iter_mut().zip(input(0)) {
                let d = buf[(*pos + len - delay) % len];
                buf[*pos] = [i[0] + d[0] * feedback, i[1] + d[1] * feedback];
                *pos = (*pos + 1) % len;
                *o = [i[0] + (d[0] - i[0]) * mix, i[1] + (d[1] - i[1]) * mix];
            }
        }
        (
            Kind::Lfo,
            State::Lfo {
                phase,
                held,
                rng,
                last_clock,
            },
        ) => {
            let (step, shape, depth) = (p[0] / rate, p[1] as u32, p[2]);
            let clock = input(0);
            let mut random = || {
                *rng ^= *rng << 13;
                *rng ^= *rng >> 17;
                *rng ^= *rng << 5;
                *rng as f32 / u32::MAX as f32 * 2.0 - 1.0
            };
            for (o, c) in out.iter_mut().zip(clock) {
                // A rising clock edge restarts the cycle; for sample & hold
                // it also takes a new value. That's how a track's gate makes
                // the modulation follow the beat.
                let edge = c[0] >= 0.5 && *last_clock < 0.5;
                *last_clock = c[0];
                *phase += step;
                let wrapped = *phase >= 1.0;
                *phase -= phase.floor();
                if edge {
                    *phase = 0.0;
                }
                if edge || (wrapped && shape == 2) {
                    *held = random();
                }
                let v = match shape {
                    0 => (*phase * TAU).sin(),
                    1 => {
                        if *phase < 0.5 {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                    _ => *held,
                };
                *o = [v * depth; 2];
            }
        }
        _ => out.fill([0.0; 2]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rack::Jack;

    const RATE: u32 = 48_000;

    fn run_block(g: &mut Graph, f: impl Fn(usize) -> [f32; 2]) -> Vec<[f32; 2]> {
        g.begin(BLOCK);
        for i in 0..BLOCK {
            g.write_track(0, i, f(i), i == 0);
        }
        g.process(BLOCK);
        g.master().to_vec()
    }

    #[test]
    fn simple_rack_passes_audio_through() {
        let mut g = Graph::build(&Rack::simple(1), RATE);
        // Under the limiter's knee, so it's left alone too.
        let f = |i: usize| [i as f32 / 512.0, -(i as f32) / 512.0];
        let out = run_block(&mut g, f);
        assert!(out.iter().enumerate().all(|(i, o)| *o == f(i)));
    }

    #[test]
    fn unpatched_track_is_silent() {
        let mut rack = Rack::simple(1);
        let t = rack.track_device(0).unwrap().id;
        let to = rack.cables.iter().find(|c| c.from.device == t).unwrap().to;
        rack.disconnect(to);
        let mut g = Graph::build(&rack, RATE);
        assert!(
            run_block(&mut g, |_| [1.0; 2])
                .iter()
                .all(|o| *o == [0.0; 2])
        );
    }

    fn insert(rack: &mut Rack, kind: Kind) -> DeviceId {
        // track 0 -> new device -> mixer channel 1
        let t = rack.track_device(0).unwrap().id;
        let to = rack.cables.iter().find(|c| c.from.device == t).unwrap().to;
        let d = rack.add(kind);
        rack.connect(Jack { device: t, port: 0 }, Jack { device: d, port: 0 })
            .unwrap();
        rack.connect(Jack { device: d, port: 0 }, to).unwrap();
        d
    }

    #[test]
    fn lowpass_removes_highs() {
        let mut rack = Rack::simple(1);
        let f = insert(&mut rack, Kind::Filter);
        rack.set_param(f, 0, 200.0);
        rack.set_param(f, 1, 0.0);
        let mut g = Graph::build(&rack, RATE);
        // Nyquist-rate square wave: all high frequencies.
        let mut peak: f32 = 0.0;
        for _ in 0..4 {
            let out = run_block(&mut g, |i| [if i % 2 == 0 { 1.0 } else { -1.0 }; 2]);
            peak = out.iter().fold(0.0, |m, o| m.max(o[0].abs()));
        }
        assert!(peak < 0.01, "peak {peak}");
    }

    #[test]
    fn delay_echoes_after_its_time() {
        let mut rack = Rack::simple(1);
        let d = insert(&mut rack, Kind::Delay);
        rack.set_param(d, 0, 2.0 * BLOCK as f32 / RATE as f32 * 1000.0); // two blocks
        rack.set_param(d, 1, 0.0);
        rack.set_param(d, 2, 1.0); // wet only
        let mut g = Graph::build(&rack, RATE);
        let first = run_block(&mut g, |i| [(i == 0) as u8 as f32; 2]);
        assert!(first.iter().all(|o| o[0] == 0.0));
        run_block(&mut g, |_| [0.0; 2]);
        let third = run_block(&mut g, |_| [0.0; 2]);
        let echo: Vec<_> = third
            .iter()
            .enumerate()
            .filter(|(_, o)| o[0] != 0.0)
            .map(|(i, _)| i)
            .collect();
        // The delay time rounds down to a whole frame, so allow one frame.
        assert!(echo.len() == 1 && echo[0] <= 1, "echo at {echo:?}");
    }

    #[test]
    fn gate_clocks_sample_and_hold() {
        let mut rack = Rack::simple(1);
        let lfo = rack.add(Kind::Lfo);
        rack.set_param(lfo, 0, 0.05); // so slow it never wraps by itself
        rack.set_param(lfo, 1, 2.0); // s&h
        let t = rack.track_device(0).unwrap().id;
        rack.connect(
            Jack { device: t, port: 1 },
            Jack {
                device: lfo,
                port: 0,
            },
        )
        .unwrap();
        // Listen to the lfo through the mixer.
        let mixer = rack
            .devices
            .iter()
            .find(|d| d.kind == Kind::Mixer)
            .unwrap()
            .id;
        rack.connect(
            Jack {
                device: lfo,
                port: 0,
            },
            Jack {
                device: mixer,
                port: 1,
            },
        )
        .unwrap();
        rack.set_param(mixer, 0, 0.0);
        let mut g = Graph::build(&rack, RATE);
        let a = run_block(&mut g, |_| [0.0; 2]);
        let b = run_block(&mut g, |_| [0.0; 2]);
        // Each block starts with a hit, so the held value changes per block
        // and stays put within it.
        assert!(a.iter().all(|o| *o == a[0]) && b.iter().all(|o| *o == b[0]));
        assert_ne!(a[0], b[0]);
    }

    #[test]
    fn track_declick_follows_param() {
        let mut rack = Rack::simple(2);
        let t1 = rack.track_device(1).unwrap().id;
        rack.set_param(t1, 0, 0.0);
        let mut g = Graph::build(&rack, RATE);
        assert!(g.track_declick(0));
        assert!(!g.track_declick(1));
        g.set_param(t1, 0, 1.0);
        assert!(g.track_declick(1));
    }

    #[test]
    fn track_volume_scales_its_output() {
        let mut rack = Rack::simple(2);
        let t1 = rack.track_device(1).unwrap().id;
        rack.set_param(t1, 1, 0.5);
        let mut g = Graph::build(&rack, RATE);
        g.begin(1);
        g.write_track(0, 0, [0.8, 0.8], false);
        g.write_track(1, 0, [0.8, -0.4], false);
        let (a, b) = (g.tracks[0].unwrap().0, g.tracks[1].unwrap().0);
        assert_eq!(g.bufs[a][0], [0.8, 0.8], "full volume is exact");
        assert_eq!(g.bufs[b][0], [0.4, -0.2]);
        g.set_param(t1, 1, 0.0);
        g.write_track(1, 0, [0.8, 0.8], false);
        assert_eq!(g.bufs[b][0], [0.0, 0.0]);
    }

    #[test]
    fn rebuild_keeps_delay_tail() {
        let mut rack = Rack::simple(1);
        let d = insert(&mut rack, Kind::Delay);
        rack.set_param(d, 1, 0.0);
        let mut g = Graph::build(&rack, RATE);
        run_block(&mut g, |_| [1.0; 2]);
        // Re-patch something unrelated, then rebuild.
        rack.add(Kind::Lfo);
        let mut g2 = Graph::build(&rack, RATE);
        g2.adopt_state(&mut g);
        let State::Delay { buf, .. } = &g2.nodes.iter().find(|n| n.id == d).unwrap().state else {
            panic!()
        };
        assert!(buf.iter().any(|f| f[0] != 0.0));
    }

    /// Run `blocks` blocks of track 0 playing `f(frame)`, returning the
    /// master's output.
    fn run_frames(g: &mut Graph, blocks: usize, f: impl Fn(usize) -> [f32; 2]) -> Vec<[f32; 2]> {
        let mut out = Vec::new();
        for b in 0..blocks {
            g.begin(BLOCK);
            for i in 0..BLOCK {
                g.write_track(0, i, f(b * BLOCK + i), false);
            }
            g.process(BLOCK);
            out.extend_from_slice(g.master());
        }
        out
    }

    fn peak(frames: &[[f32; 2]]) -> f32 {
        frames
            .iter()
            .fold(0.0, |m, o| m.max(o[0].abs()).max(o[1].abs()))
    }

    fn sine(freq: f32, amp: f32) -> impl Fn(usize) -> [f32; 2] {
        move |i| [(TAU * freq * i as f32 / RATE as f32).sin() * amp; 2]
    }

    #[test]
    fn the_limiter_leaves_quiet_alone_and_rounds_off_loud() {
        let mut rack = Rack::simple(1);
        let mut g = Graph::build(&rack, RATE);
        let quiet = run_frames(&mut g, 1, |_| [0.7, -0.7]);
        assert!(quiet.iter().all(|o| *o == [0.7, -0.7]));
        let loud = run_frames(&mut g, 1, |_| [5.0, -40.0]);
        assert!(
            loud.iter()
                .all(|o| o[0] < 1.0 && o[0] > 0.95 && o[1] > -1.0)
        );
        // Smooth at the knee: just over it is just over it.
        assert!((limit(LIMIT_KNEE + 0.01) - (LIMIT_KNEE + 0.01)).abs() < 1e-3);
        let master = rack
            .devices
            .iter()
            .find(|d| d.kind == Kind::Master)
            .unwrap()
            .id;
        rack.set_param(master, crate::rack::MASTER_LIMIT, 0.0);
        let mut g = Graph::build(&rack, RATE);
        assert!(
            run_frames(&mut g, 1, |_| [5.0; 2])
                .iter()
                .all(|o| *o == [5.0; 2])
        );
    }

    #[test]
    fn a_synced_delay_follows_the_tempo() {
        let mut rack = Rack::simple(1);
        let d = insert(&mut rack, Kind::Delay);
        rack.set_param(d, 1, 0.0);
        rack.set_param(d, 2, 1.0); // wet only
        rack.set_param(d, 3, 3.0); // 1/16
        let echo_at = |bpm: f64| {
            let mut g = Graph::build(&rack, RATE);
            g.set_tempo(bpm);
            let out = run_frames(&mut g, 40, |i| [(i == 0) as u8 as f32; 2]);
            out.iter().position(|o| o[0] != 0.0)
        };
        // A sixteenth at 120 bpm is 125 ms; at 150, 100 ms.
        assert_eq!(echo_at(120.0), Some(6000));
        assert_eq!(echo_at(150.0), Some(4800));
    }

    #[test]
    fn pan_turns_the_other_side_down() {
        let mut rack = Rack::simple(1);
        let t = rack.track_device(0).unwrap().id;
        rack.set_param(t, TRACK_PAN, -1.0);
        let mut g = Graph::build(&rack, RATE);
        let out = run_frames(&mut g, 1, |_| [0.5; 2]);
        assert!(out[0][0] == 0.5 && out[0][1].abs() < 1e-6, "{:?}", out[0]);
        g.set_param(t, TRACK_PAN, 0.5);
        let out = run_frames(&mut g, 1, |_| [0.5; 2]);
        assert_eq!(out[0][1], 0.5, "the near side keeps its level");
        assert!((out[0][0] - 0.5 * (PI / 4.0).cos()).abs() < 1e-6);
    }

    #[test]
    fn drive_saturates_and_its_mix_blends_it_in() {
        let mut rack = Rack::simple(1);
        let d = insert(&mut rack, Kind::Drive);
        rack.set_param(d, 0, 36.0);
        rack.set_param(d, 1, 16000.0);
        let mut g = Graph::build(&rack, RATE);
        let out = run_frames(&mut g, 8, sine(100.0, 0.5));
        let p = peak(&out[1024..]);
        assert!(p < 0.5, "turned back down to {p}");
        // Squared off: most of the wave sits near its peak.
        let near = out[1024..].iter().filter(|o| o[0].abs() > 0.8 * p).count();
        assert!(near > out[1024..].len() / 2, "{near} near the peak");
        rack.set_param(d, 3, 0.0);
        let mut g = Graph::build(&rack, RATE);
        let dry = run_frames(&mut g, 2, sine(100.0, 0.5));
        assert!(
            dry.iter()
                .enumerate()
                .all(|(i, o)| *o == sine(100.0, 0.5)(i))
        );
    }

    #[test]
    fn eq_lifts_and_cuts_where_its_told() {
        let mut rack = Rack::simple(1);
        let e = insert(&mut rack, Kind::Eq);
        let mut g = Graph::build(&rack, RATE);
        let flat = run_frames(&mut g, 2, sine(1000.0, 0.25));
        assert!(
            flat.iter()
                .enumerate()
                .all(|(i, o)| *o == sine(1000.0, 0.25)(i))
        );
        let gain = |rack: &Rack, freq: f32| {
            let mut g = Graph::build(rack, RATE);
            let out = run_frames(&mut g, 16, sine(freq, 0.1));
            peak(&out[2048..]) / 0.1
        };
        rack.set_param(e, 1, 12.0);
        let mid = gain(&rack, 1000.0);
        assert!((mid - 3.98).abs() < 0.1, "+12 dB at 1 kHz is {mid}");
        assert!(gain(&rack, 50.0) < 1.1, "and not down low");
        rack.set_param(e, 1, 0.0);
        rack.set_param(e, 0, -12.0);
        assert!(gain(&rack, 40.0) < 0.35, "a low cut");
        assert!(
            (gain(&rack, 3000.0) - 1.0).abs() < 0.05,
            "leaving the highs"
        );
    }

    #[test]
    fn the_compressor_turns_loud_down_and_its_side_jack_ducks() {
        let mut rack = Rack::simple(2);
        let c = insert(&mut rack, Kind::Compressor);
        rack.set_param(c, 0, -20.0);
        rack.set_param(c, 1, 10.0);
        let mut g = Graph::build(&rack, RATE);
        // 0.5 is 14 dB over: ten to one leaves about 1.4 of it.
        let out = run_frames(&mut g, 40, |_| [0.5; 2]);
        let level = 20.0 * out.last().unwrap()[0].log10();
        assert!((level - -18.6).abs() < 0.5, "{level} dB");
        // After a second to let go, a quiet signal goes through as it is.
        let quiet = run_frames(&mut g, 200, |_| [0.01; 2]);
        assert!(
            (quiet.last().unwrap()[0] - 0.01).abs() < 1e-4,
            "under it, untouched"
        );

        // Track 1 into the side jack: while it's loud, track 0 ducks.
        let t1 = rack.track_device(1).unwrap().id;
        rack.connect(
            Jack {
                device: t1,
                port: 0,
            },
            Jack { device: c, port: 1 },
        )
        .unwrap();
        let mut g = Graph::build(&rack, RATE);
        let mut last = [0.0; 2];
        for _ in 0..40 {
            g.begin(BLOCK);
            for i in 0..BLOCK {
                g.write_track(0, i, [0.1; 2], false);
                g.write_track(1, i, [0.9; 2], false);
            }
            g.process(BLOCK);
            last = g.master()[BLOCK - 1];
        }
        // The master hears both: track 1 through the mixer, and track 0
        // ducked well under its 0.1.
        let ducked = last[0] - 0.9;
        assert!(ducked < 0.03, "ducked to {ducked}");
    }

    #[test]
    fn reverb_leaves_a_tail_that_dies_away() {
        let mut rack = Rack::simple(1);
        let r = insert(&mut rack, Kind::Reverb);
        rack.set_param(r, 2, 0.0);
        rack.set_param(r, 3, 1.0); // wet only
        let mut g = Graph::build(&rack, RATE);
        let out = run_frames(&mut g, 400, |i| [(i == 0) as u8 as f32; 2]);
        let second = |s: usize| peak(&out[s * RATE as usize / 4..(s + 1) * RATE as usize / 4]);
        let (early, late) = (second(0), second(7));
        assert!(early > 0.01, "a tail: {early}");
        assert!(late < early / 10.0, "that dies away: {early} then {late}");
        assert!(out.iter().all(|o| o[0].is_finite() && o[0].abs() < 1.0));
        assert_ne!(out[3000][0], out[3000][1], "wider than mono");
        rack.set_param(r, 3, 0.0);
        let mut g = Graph::build(&rack, RATE);
        assert!(
            run_frames(&mut g, 1, |_| [0.3; 2])
                .iter()
                .all(|o| *o == [0.3; 2])
        );
    }
}
