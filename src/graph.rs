//! The runnable form of a [`Rack`]: devices in processing order, each with
//! preallocated buffers. Building a graph allocates, so it happens on the UI
//! thread; the audio thread only runs it.
//!
//! Every jack carries a stereo buffer. Control signals use the left channel.

use std::f32::consts::{PI, TAU};

use crate::engine::MAX_TRACKS;
use crate::rack::{DeviceId, Kind, Rack};

/// Frames processed per call to [`Graph::process`].
pub const BLOCK: usize = 256;
/// How long a track's gate output stays high after a hit.
const GATE_MS: f32 = 5.0;
/// Longest delay time, plus headroom.
const DELAY_MAX_S: f32 = 1.6;

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
        for n in &nodes {
            if let Kind::Track(t) = n.kind
                && t < MAX_TRACKS
            {
                tracks[t] = Some((n.outputs[0], n.outputs[1]));
            }
        }
        Graph {
            rate,
            nodes,
            bufs,
            zero: vec![[0.0; 2]; BLOCK],
            tracks,
            gate_left: [0; MAX_TRACKS],
            master,
        }
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
        self.bufs[audio][i] = frame;
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
            ..
        } = self;
        let rate = *rate;
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
            );
            bufs[o] = out;
            master_written |= o == *master;
        }
        if !master_written {
            bufs[*master][..frames].fill([0.0; 2]);
        }
    }
}

fn run<'a>(
    kind: Kind,
    p: &[f32],
    state: &mut State,
    input: &dyn Fn(usize) -> &'a [[f32; 2]],
    out: &mut [[f32; 2]],
    rate: f32,
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
            for (o, i) in out.iter_mut().zip(input(0)) {
                *o = [i[0] * p[0], i[1] * p[0]];
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
            let delay = ((p[0] / 1000.0 * rate) as usize).clamp(1, len - 1);
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
        let out = run_block(&mut g, |i| [i as f32, -(i as f32)]);
        assert!(
            out.iter()
                .enumerate()
                .all(|(i, o)| *o == [i as f32, -(i as f32)])
        );
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
}
