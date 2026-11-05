//! Synth instruments: a 303-style acid voice and a two-operator FM voice.
//!
//! Like sample voices, synth voices are monophonic, one per track. Notes
//! hold until the next note on the track or a cut. A note that glides
//! (`Fx::Glide`) into a held note slides its pitch without restarting the
//! envelopes, which is the acid "slide".

use std::f64::consts::TAU;

use serde::{Deserialize, Serialize};

use crate::rack::{Curve, ParamSpec};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SynthKind {
    Acid,
    Fm,
}

/// Indices into `SynthPatch::params`.
mod acid {
    pub const WAVE: usize = 0;
    pub const CUTOFF: usize = 1;
    pub const RESO: usize = 2;
    pub const ENV_MOD: usize = 3;
    pub const DECAY: usize = 4;
    pub const ACCENT: usize = 5;
    pub const DRIVE: usize = 6;
    pub const VOLUME: usize = 7;
}

mod fm {
    pub const RATIO: usize = 0;
    pub const INDEX: usize = 1;
    pub const INDEX_DECAY: usize = 2;
    pub const FEEDBACK: usize = 3;
    pub const ATTACK: usize = 4;
    pub const DECAY: usize = 5;
    pub const SUSTAIN: usize = 6;
    pub const RELEASE: usize = 7;
    pub const DETUNE: usize = 8;
    pub const VOLUME: usize = 9;
}

impl SynthKind {
    pub const ALL: [SynthKind; 2] = [SynthKind::Acid, SynthKind::Fm];

    pub fn name(self) -> &'static str {
        match self {
            SynthKind::Acid => "acid",
            SynthKind::Fm => "fm",
        }
    }

    pub fn params(self) -> &'static [ParamSpec] {
        use Curve::*;
        const ACID: &[ParamSpec] = &[
            ParamSpec::labelled("wave", 0.0, &["saw", "square"]),
            ParamSpec::new("cutoff", 40.0, 8000.0, 400.0, Exp, "Hz"),
            ParamSpec::new("reso", 0.0, 0.97, 0.75, Linear, ""),
            ParamSpec::new("env mod", 0.0, 1.0, 0.6, Linear, ""),
            ParamSpec::new("decay", 30.0, 2000.0, 300.0, Exp, "ms"),
            ParamSpec::new("accent", 0.0, 1.0, 0.5, Linear, ""),
            ParamSpec::new("drive", 0.0, 1.0, 0.3, Linear, ""),
            ParamSpec::new("volume", 0.0, 1.0, 0.5, Linear, ""),
        ];
        const FM: &[ParamSpec] = &[
            ParamSpec::new("ratio", 0.5, 16.0, 3.5, Linear, ""),
            ParamSpec::new("index", 0.0, 10.0, 2.5, Linear, ""),
            ParamSpec::new("idx decay", 10.0, 4000.0, 500.0, Exp, "ms"),
            ParamSpec::new("feedback", 0.0, 1.0, 0.0, Linear, ""),
            ParamSpec::new("attack", 1.0, 2000.0, 2.0, Exp, "ms"),
            ParamSpec::new("decay", 10.0, 4000.0, 900.0, Exp, "ms"),
            ParamSpec::new("sustain", 0.0, 1.0, 0.2, Linear, ""),
            ParamSpec::new("release", 5.0, 4000.0, 600.0, Exp, "ms"),
            ParamSpec::new("detune", -50.0, 50.0, 0.0, Linear, "ct"),
            ParamSpec::new("volume", 0.0, 1.0, 0.5, Linear, ""),
        ];
        match self {
            SynthKind::Acid => ACID,
            SynthKind::Fm => FM,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SynthPatch {
    pub name: String,
    pub kind: SynthKind,
    pub params: Vec<f32>,
}

impl SynthPatch {
    pub fn new(kind: SynthKind, name: impl Into<String>) -> Self {
        SynthPatch {
            name: name.into(),
            kind,
            params: kind.params().iter().map(|p| p.default).collect(),
        }
    }

    /// Fix up a patch read from a file: right number of params, in range.
    pub fn repair(&mut self) {
        let specs = self.kind.params();
        let have = self.params.len().min(specs.len());
        self.params.truncate(have);
        self.params.extend(specs[have..].iter().map(|p| p.default));
        for (v, spec) in self.params.iter_mut().zip(specs) {
            *v = spec.clamp(*v);
        }
    }

    fn p(&self, i: usize) -> f32 {
        self.params.get(i).copied().unwrap_or(0.0)
    }
}

/// Per-frame multiplier for an envelope that falls by 60 dB (to
/// practically silent) over `ms` milliseconds.
fn decay_coef(ms: f32, rate: f64) -> f32 {
    let frames = (ms as f64 / 1000.0 * rate).max(1.0);
    (-6.9 / frames).exp() as f32
}

/// PolyBLEP: smooths an oscillator's jump at phase `t`, `dt` per frame, to
/// cut aliasing.
fn poly_blep(t: f64, dt: f64) -> f64 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Stage {
    #[default]
    Off,
    Attack,
    Decay,
    Release,
}

#[derive(Clone, Debug, Default)]
pub struct SynthVoice {
    active: bool,
    instrument: usize,
    freq: f64,
    target: f64,
    /// Per-frame pitch multiplier while gliding.
    glide: f64,
    phase: f64,
    mod_phase: f64,
    feedback: f32,
    /// Acid filter state, one per pole.
    poles: [f32; 4],
    amp: f32,
    stage: Stage,
    /// Filter envelope (acid) or modulation index envelope (FM), 1 down to 0.
    env: f32,
    velocity: f32,
    accent: bool,
    /// Output frames until the note is released.
    release_in: Option<f64>,
    hit: bool,
}

impl SynthVoice {
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn instrument(&self) -> usize {
        self.instrument
    }

    pub fn stop(&mut self) {
        self.active = false;
    }

    pub fn take_hit(&mut self) -> bool {
        std::mem::take(&mut self.hit)
    }

    /// Start a note at `freq` Hz. With `glide_ms` and a note already held,
    /// slide there instead of starting over.
    pub fn note_on(
        &mut self,
        instrument: usize,
        freq: f64,
        velocity: u8,
        glide_ms: f32,
        rate: f64,
    ) {
        let v = velocity.min(127) as f32 / 127.0;
        let held = self.active && self.stage != Stage::Release && self.instrument == instrument;
        self.release_in = None;
        if glide_ms > 0.0 && held {
            self.target = freq;
            let frames = (glide_ms as f64 / 1000.0 * rate).max(1.0);
            self.glide = (freq / self.freq).powf(1.0 / frames);
            return;
        }
        // Keep the phase and the current level, so a new note on a sounding
        // voice doesn't click.
        self.active = true;
        self.instrument = instrument;
        self.freq = freq;
        self.target = freq;
        self.glide = 1.0;
        self.stage = Stage::Attack;
        self.env = 1.0;
        self.velocity = v;
        self.accent = velocity > 100;
        self.hit = true;
    }

    /// Release the note after `frames` output frames.
    pub fn note_off_in(&mut self, frames: f64) {
        if self.active {
            self.release_in = Some(frames.max(0.0));
        }
    }

    pub fn next_frame(&mut self, patch: &SynthPatch, rate: f64) -> [f32; 2] {
        if !self.active {
            return [0.0; 2];
        }
        if let Some(r) = &mut self.release_in {
            if *r <= 0.0 {
                self.release_in = None;
                self.stage = Stage::Release;
            } else {
                *r -= 1.0;
            }
        }
        if self.glide != 1.0 {
            self.freq *= self.glide;
            let arrived = if self.glide > 1.0 {
                self.freq >= self.target
            } else {
                self.freq <= self.target
            };
            if arrived {
                self.freq = self.target;
                self.glide = 1.0;
            }
        }
        let s = match patch.kind {
            SynthKind::Acid => self.acid(patch, rate),
            SynthKind::Fm => self.fm(patch, rate),
        };
        if self.stage == Stage::Release && self.amp < 1e-4 {
            self.active = false;
            self.stage = Stage::Off;
        }
        [s; 2]
    }

    fn acid(&mut self, patch: &SynthPatch, rate: f64) -> f32 {
        use acid::*;
        let dt = (self.freq / rate).min(0.5);
        let t = self.phase;
        let osc = if patch.p(WAVE) < 0.5 {
            2.0 * t - 1.0 - poly_blep(t, dt)
        } else {
            let sq = if t < 0.5 { 1.0 } else { -1.0 };
            sq + poly_blep(t, dt) - poly_blep((t + 0.5) % 1.0, dt)
        } as f32;
        self.phase = (self.phase + dt) % 1.0;

        // Amp: 1 ms attack, hold while the note is on, 8 ms release.
        match self.stage {
            Stage::Attack | Stage::Decay => {
                self.amp = (self.amp + 1.0 / (0.001 * rate as f32)).min(1.0);
                self.stage = Stage::Decay;
            }
            Stage::Release => self.amp *= decay_coef(8.0, rate),
            Stage::Off => self.amp = 0.0,
        }
        let accent = if self.accent { patch.p(ACCENT) } else { 0.0 };
        // Accented notes have a shorter, stronger filter sweep.
        let decay = patch.p(DECAY) * (1.0 - 0.6 * accent);
        self.env *= decay_coef(decay, rate);

        let octaves = patch.p(ENV_MOD) * self.env * (4.0 + 3.0 * accent);
        let fc = (patch.p(CUTOFF) * octaves.exp2()).clamp(20.0, rate as f32 * 0.45);
        let g = 1.0 - (-std::f32::consts::TAU * fc / rate as f32).exp();
        let k = patch.p(RESO) * 3.9;
        // Four one-pole lowpasses with resonance fed back from the last,
        // saturated at the input. Bass is lost as resonance goes up, so
        // part of the input is added back.
        let drive = 1.0 + 4.0 * patch.p(DRIVE);
        let x = (drive * osc - k * self.poles[3]).tanh();
        self.poles[0] += g * (x - self.poles[0]);
        self.poles[1] += g * (self.poles[0] - self.poles[1]);
        self.poles[2] += g * (self.poles[1] - self.poles[2]);
        self.poles[3] += g * (self.poles[2] - self.poles[3]);
        let out = self.poles[3] * (1.0 + 0.5 * k);
        let level = patch.p(VOLUME) * (0.6 + 0.4 * self.velocity) * (1.0 + 0.4 * accent);
        out * self.amp * level
    }

    fn fm(&mut self, patch: &SynthPatch, rate: f64) -> f32 {
        use fm::*;
        let ratio = (patch.p(RATIO) * 2.0).round() as f64 / 2.0;
        let carrier = self.freq * ((patch.p(DETUNE) as f64) / 1200.0).exp2();
        let m = ((self.mod_phase * TAU) as f32 + patch.p(FEEDBACK) * 2.0 * self.feedback).sin();
        self.feedback = m;
        self.env *= decay_coef(patch.p(INDEX_DECAY), rate);
        let index = patch.p(INDEX) * (0.3 + 0.7 * self.env) * (0.5 + 0.5 * self.velocity);
        let out = ((self.phase * TAU) as f32 + index * m).sin();
        self.phase = (self.phase + carrier / rate) % 1.0;
        self.mod_phase = (self.mod_phase + carrier * ratio / rate) % 1.0;

        let sustain = patch.p(SUSTAIN);
        match self.stage {
            Stage::Attack => {
                self.amp += 1.0 / (patch.p(ATTACK) / 1000.0 * rate as f32).max(1.0);
                if self.amp >= 1.0 {
                    self.amp = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                let c = decay_coef(patch.p(DECAY), rate);
                self.amp = sustain + (self.amp - sustain) * c;
            }
            Stage::Release => self.amp *= decay_coef(patch.p(RELEASE), rate),
            Stage::Off => self.amp = 0.0,
        }
        out * self.amp * patch.p(VOLUME) * self.velocity * self.velocity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    fn render(v: &mut SynthVoice, patch: &SynthPatch, frames: usize) -> Vec<f32> {
        (0..frames).map(|_| v.next_frame(patch, RATE)[0]).collect()
    }

    /// Frequency from upward zero crossings, skipping the start.
    fn pitch(out: &[f32]) -> f64 {
        let out = &out[out.len() / 4..];
        let ups = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        ups as f64 / (out.len() as f64 / RATE)
    }

    #[test]
    fn both_kinds_play_in_tune() {
        for kind in SynthKind::ALL {
            let mut patch = SynthPatch::new(kind, "t");
            if kind == SynthKind::Acid {
                // Open the filter so the fundamental dominates.
                patch.params[acid::CUTOFF] = 8000.0;
                patch.params[acid::RESO] = 0.0;
            } else {
                patch.params[fm::INDEX] = 0.0;
            }
            let mut v = SynthVoice::default();
            v.note_on(0, 220.0, 100, 0.0, RATE);
            let f = pitch(&render(&mut v, &patch, 48_000));
            assert!((f - 220.0).abs() < 3.0, "{kind:?} at {f} Hz");
        }
    }

    #[test]
    fn note_off_releases_to_silence() {
        for kind in SynthKind::ALL {
            let patch = SynthPatch::new(kind, "t");
            let mut v = SynthVoice::default();
            v.note_on(0, 110.0, 127, 0.0, RATE);
            render(&mut v, &patch, 4800);
            v.note_off_in(0.0);
            render(&mut v, &patch, 48_000 * 4);
            assert!(!v.is_active(), "{kind:?} still sounding");
        }
    }

    #[test]
    fn glide_slides_without_restarting() {
        let patch = SynthPatch::new(SynthKind::Acid, "t");
        let mut v = SynthVoice::default();
        v.note_on(0, 110.0, 80, 0.0, RATE);
        assert!(v.take_hit());
        render(&mut v, &patch, 1000);
        v.note_on(0, 220.0, 80, 50.0, RATE);
        assert!(!v.take_hit(), "a slide is not a new hit");
        render(&mut v, &patch, 1000);
        assert!(v.freq > 110.0 && v.freq < 220.0, "halfway: {}", v.freq);
        render(&mut v, &patch, 2000);
        assert_eq!(v.freq, 220.0);
    }

    #[test]
    fn output_stays_bounded() {
        for kind in SynthKind::ALL {
            let mut patch = SynthPatch::new(kind, "t");
            for (p, spec) in patch.params.iter_mut().zip(kind.params()) {
                *p = spec.max;
            }
            let mut v = SynthVoice::default();
            v.note_on(0, 55.0, 127, 0.0, RATE);
            let out = render(&mut v, &patch, 48_000);
            let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(
                out.iter().all(|s| s.is_finite()) && peak < 4.0,
                "{kind:?} peak {peak}"
            );
        }
    }

    #[test]
    fn repair_fills_and_clamps() {
        let mut p = SynthPatch {
            name: "x".into(),
            kind: SynthKind::Fm,
            params: vec![99.0],
        };
        p.repair();
        assert_eq!(p.params.len(), SynthKind::Fm.params().len());
        assert_eq!(p.params[0], 16.0);
    }
}
