//! A piano synth: modelled strings, hammers and soundboard.
//!
//! Each note is one to three strings, struck together by a felt hammer.
//! A string is a digital waveguide: a delay line as long as one period,
//! closed through a loss filter (higher partials die away faster), a chain
//! of allpasses that make it stiff (higher partials run sharp, as on a
//! real piano) and a fractional delay that tunes it exactly.
//!
//! The strings of a note are a few cents apart and decay at different
//! rates, which gives the beating and the two-stage decay (a loud first
//! sound, then a long quiet one) of a real piano. The hammer is a smooth
//! pulse that gets shorter, and so brighter, the harder it hits, shaped by
//! where along the string it strikes.
//!
//! What all of a track's notes share is in [`Body`]: a soundboard that
//! colours them, and strings that ring in sympathy while the pedal is down.

use std::f64::consts::{PI, TAU};

/// Indices into a piano patch's params.
pub mod param {
    pub const HAMMER: usize = 0;
    pub const SUSTAIN: usize = 1;
    pub const DAMPER: usize = 2;
    pub const DETUNE: usize = 3;
    pub const STIFFNESS: usize = 4;
    pub const BODY: usize = 5;
    pub const RESONANCE: usize = 6;
    pub const WIDTH: usize = 7;
    pub const VOLUME: usize = 8;
}

/// Most strings a note has.
const MAX_STRINGS: usize = 3;
/// Most allpasses that make a string stiff.
const MAX_STAGES: usize = 8;
/// Keys (MIDI note numbers) from here up have no dampers, as on a real
/// grand: they ring on after they're let go.
const FIRST_UNDAMPED: f64 = 89.0;
/// The lowest note a string's delay line has room for. Lower ones play an
/// octave (or more) up.
const LOWEST_FREQ: f64 = 26.0;
/// A voice is done once it's this quiet.
const SILENT: f32 = 3e-5;

/// Linear interpolation in a table of (x, y), sorted by x, held flat past
/// its ends.
fn interp(table: &[(f64, f64)], x: f64) -> f64 {
    let (first, last) = (table[0], table[table.len() - 1]);
    if x <= first.0 {
        return first.1;
    }
    if x >= last.0 {
        return last.1;
    }
    let i = table.windows(2).position(|w| x < w[1].0).unwrap_or(0);
    let ((x0, y0), (x1, y1)) = (table[i], table[i + 1]);
    y0 + (y1 - y0) * (x - x0) / (x1 - x0)
}

/// Phase delay in samples, at `w` radians per sample, of the first-order
/// allpass `(a + z⁻¹) / (1 + a z⁻¹)`.
fn allpass_delay(a: f64, w: f64) -> f64 {
    1.0 - 2.0 * (a * w.sin()).atan2(1.0 + a * w.cos()) / w
}

/// The first-order allpass coefficient with a phase delay of `d` samples
/// at `w`.
fn allpass_for_delay(d: f64, w: f64) -> f64 {
    let t = ((1.0 - d) * w / 2.0).tan();
    t / (w.sin() - t * w.cos())
}

/// Seconds for the fundamental to die away by 60 dB, by key: long in the
/// bass, short in the treble.
fn sustain_time(key: f64) -> f64 {
    const T: &[(f64, f64)] = &[
        (21.0, 22.0),
        (40.0, 15.0),
        (60.0, 10.0),
        (72.0, 6.5),
        (84.0, 3.5),
        (96.0, 1.8),
        (108.0, 0.9),
    ];
    interp(T, key)
}

/// The same for the partials around [`high_freq`]: they fade much faster.
fn sustain_time_high(key: f64) -> f64 {
    const T: &[(f64, f64)] = &[
        (21.0, 2.0),
        (48.0, 2.5),
        (72.0, 1.8),
        (96.0, 0.9),
        (108.0, 0.5),
    ];
    interp(T, key)
}

/// Where [`sustain_time_high`] applies.
fn high_freq(f0: f64, rate: f64) -> f64 {
    (4.0 * f0).max(4000.0).min(0.45 * rate)
}

/// Seconds for a damped string to die away by 60 dB, at a damper time of
/// one second: bass strings take longest to quieten.
fn damped_time(key: f64) -> f64 {
    interp(&[(21.0, 2.5), (60.0, 1.0), (88.0, 0.6)], key)
}

/// Inharmonicity: how sharp a string's partials run, `fₙ = n f₀ √(1 + B n²)`.
/// Wound bass strings are a bit stiff, short treble strings very.
fn inharmonicity(key: f64) -> f64 {
    const LOG_B: &[(f64, f64)] = &[
        (21.0, -3.5),
        (33.0, -3.9),
        (45.0, -4.0),
        (60.0, -3.7),
        (72.0, -3.3),
        (84.0, -2.8),
        (96.0, -2.4),
        (108.0, -2.0),
    ];
    10f64.powf(interp(LOG_B, key))
}

/// A loss filter `b / (1 − p z⁻¹)` and its phase delay at `w0`, for a
/// string at `w0` whose loop loses `g0` per period at `w0` and `gh` at
/// `wh`.
fn loss_filter(g0: f64, gh: f64, w0: f64, wh: f64) -> (f64, f64, f64) {
    // The most a one-pole can fall from w0 to wh.
    let floor = (w0 / 2.0).sin() / (wh / 2.0).sin();
    let r = (gh / g0).clamp(floor + 1e-4, 1.0);
    let p = if r >= 1.0 - 1e-9 {
        0.0
    } else {
        // |1 − p e^−jw0| / |1 − p e^−jwh| = r, a quadratic in p with one
        // root inside the unit circle.
        let a = 1.0 - r * r;
        let b = 2.0 * (w0.cos() - r * r * wh.cos());
        let disc = (b * b - 4.0 * a * a).max(0.0);
        ((b - disc.sqrt()) / (2.0 * a)).clamp(0.0, 0.999)
    };
    let mag = (1.0 - 2.0 * p * w0.cos() + p * p).sqrt();
    let b = (g0 * mag).min(0.99999 * (1.0 - p));
    (b, p, loss_delay(p, w0))
}

#[derive(Clone, Debug, Default)]
struct Str {
    buf: Box<[f32]>,
    w: usize,
    len: usize,
    /// Loss filter: `y = b x + p y₋₁`.
    loss_b: f32,
    loss_p: f32,
    loss_y: f32,
    /// Stiffness: `stages` allpasses with coefficient `disp_a`.
    disp_a: f32,
    stages: usize,
    disp: [[f32; 2]; MAX_STAGES],
    /// Fine tuning: one more allpass.
    frac_c: f32,
    frac: [f32; 2],
    gain: f32,
    freq: f64,
    loss_delay: f64,
}

impl Str {
    fn silence(&mut self) {
        self.buf.fill(0.0);
        self.loss_y = 0.0;
        self.disp = [[0.0; 2]; MAX_STAGES];
        self.frac = [0.0; 2];
    }

    /// Tune to `freq` with inharmonicity `b`, its fundamental dying away
    /// in `t0` seconds and its high partials in `th`. False if the delay
    /// line is too short.
    fn tune(&mut self, freq: f64, b: f64, t0: f64, th: f64, rate: f64) -> bool {
        let period = rate / freq;
        if period + 4.0 > self.buf.len() as f64 {
            return false;
        }
        self.freq = freq;
        self.set_loss(t0, th, rate);
        let w0 = TAU * freq / rate;
        let budget = period - 2.5 - self.loss_delay;
        let stages = ((period / 20.0) as usize).min(MAX_STAGES);
        let stages = if (stages as f64) < budget && b > 0.0 {
            stages
        } else {
            0
        };
        let p = self.loss_p as f64;
        let a = dispersion(b, period, w0, stages, budget, p);
        let (len, c) = split(period, w0, stages, a, p);
        self.len = len;
        self.stages = stages;
        self.disp_a = a as f32;
        self.frac_c = c as f32;
        true
    }

    fn set_loss(&mut self, t0: f64, th: f64, rate: f64) {
        let f0 = self.freq;
        let w0 = TAU * f0 / rate;
        let fh = high_freq(f0, rate).max(1.5 * f0).min(0.48 * rate);
        let wh = TAU * fh / rate;
        let g0 = 10f64.powf(-3.0 / (f0 * t0.max(1e-3)));
        let gh = 10f64.powf(-3.0 / (f0 * th.min(0.95 * t0).max(1e-3)));
        let (b, p, delay) = loss_filter(g0, gh, w0, wh);
        self.loss_b = b as f32;
        self.loss_p = p as f32;
        self.loss_delay = delay;
    }

    #[inline]
    fn next(&mut self, input: f32) -> f32 {
        let mask = self.buf.len() - 1;
        let s = self.buf[(self.w + self.buf.len() - self.len) & mask];
        let mut v = self.loss_b * s + self.loss_p * self.loss_y;
        self.loss_y = v;
        let a = self.disp_a;
        for st in &mut self.disp[..self.stages] {
            let y = a * v + st[0] - a * st[1];
            *st = [v, y];
            v = y;
        }
        let c = self.frac_c;
        let y = c * v + self.frac[0] - c * self.frac[1];
        self.frac = [v, y];
        self.buf[self.w] = y + input;
        self.w = (self.w + 1) & mask;
        s
    }
}

/// Phase delay in samples, at `w`, of the loss filter with pole `p`.
fn loss_delay(p: f64, w: f64) -> f64 {
    (p * w.sin()).atan2(1.0 - p * w.cos()) / w
}

/// The delay line's length and the tuning allpass's coefficient that
/// make a string's loop exactly `period` samples at `w0`, given `stages`
/// stiffness allpasses `a` and loss filter pole `p`.
fn split(period: f64, w0: f64, stages: usize, a: f64, p: f64) -> (usize, f64) {
    let rest = period - stages as f64 * allpass_delay(a, w0) - loss_delay(p, w0);
    let len = ((rest - 0.5).floor() as usize).max(1);
    let c = allpass_for_delay(rest - len as f64, w0).clamp(-0.95, 0.95);
    (len, c)
}

/// The coefficient for `stages` allpasses that make a string of `period`
/// samples (`w0` radians per sample) as stiff as `b` says, within a delay
/// of `budget` samples at `w0`: the best fit, by least squares, to its
/// first partials. The loss filter (pole `p`) and the tuning allpass
/// spread the partials too, which short strings notice, so the fit
/// counts them.
fn dispersion(b: f64, period: f64, w0: f64, stages: usize, budget: f64, p: f64) -> f64 {
    if stages == 0 || b <= 0.0 {
        return 0.0;
    }
    let partial = |n: f64| n * w0 * ((1.0 + b * n * n) / (1.0 + b)).sqrt();
    // The partials that matter most: up to the 10th, below 10 kHz-ish
    // (0.8π at 24 kHz).
    let mut k = 1.0;
    while k < 10.0 && partial(k + 1.0) < 0.8 * PI {
        k += 1.0;
    }
    if k < 2.0 {
        return 0.0;
    }
    let m = stages as f64;
    // Each partial's tuning error, as a share of its period, the lower
    // (and louder) ones counting most.
    let cost = |a: f64| {
        let (len, c) = split(period, w0, stages, a, p);
        let mut sum = 0.0;
        let mut n = 2.0;
        while n <= k {
            let w = partial(n);
            let want = period * ((1.0 + b) / (1.0 + b * n * n)).sqrt();
            let got = len as f64 + m * allpass_delay(a, w) + loss_delay(p, w) + allpass_delay(c, w);
            sum += ((got - want) / period).powi(2) / (n * n);
            n += 1.0;
        }
        sum
    };
    // The most negative coefficient (the stiffest) that fits the budget.
    let (mut lo, mut hi) = (-0.97, 0.0);
    if m * allpass_delay(lo, w0) > budget {
        for _ in 0..30 {
            let mid = (lo + hi) / 2.0;
            if m * allpass_delay(mid, w0) > budget {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo = hi;
    }
    // Golden-section search between there and none.
    let (mut a, mut c) = (lo, 0.0);
    let g = 0.618_033_988_75;
    let mut x1 = c - g * (c - a);
    let mut x2 = a + g * (c - a);
    let (mut f1, mut f2) = (cost(x1), cost(x2));
    for _ in 0..32 {
        if f1 < f2 {
            c = x2;
            x2 = x1;
            f2 = f1;
            x1 = c - g * (c - a);
            f1 = cost(x1);
        } else {
            a = x1;
            x1 = x2;
            f1 = f2;
            x2 = a + g * (c - a);
            f2 = cost(x2);
        }
    }
    (a + c) / 2.0
}

/// xorshift32 noise, −1..1.
#[derive(Clone, Debug)]
struct Noise(u32);

impl Default for Noise {
    fn default() -> Self {
        Noise(0x9e37_79b9)
    }
}

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
    }
}

/// One note.
#[derive(Clone, Debug, Default)]
pub struct Voice {
    strings: [Str; MAX_STRINGS],
    count: usize,
    key: f64,
    /// Frames since the hammer hit.
    n: f64,
    /// The hammer: a pulse `n e^(−n/τ)`, minus itself `comb` frames later
    /// (where along the string it hit), `amp` loud, over `hammer_len`
    /// frames.
    amp: f64,
    tau: f64,
    comb: f64,
    fall: f64,
    exp: f64,
    hammer_len: f64,
    /// The thump of the hammer and key: noise through a resonance.
    noise: Noise,
    knock_amp: f32,
    knock_fall: f32,
    knock_bp: [f32; 2],
    knock_coef: [f32; 2],
    pan: [f32; 2],
    env: f32,
    env_fall: f32,
    damped: bool,
}

impl Voice {
    /// A voice with room for notes down to [`LOWEST_FREQ`] at `rate`.
    pub fn new(rate: f64) -> Self {
        let len = ((rate / LOWEST_FREQ) as usize + 16).next_power_of_two();
        let mut v = Voice::default();
        for s in &mut v.strings {
            s.buf = vec![0.0; len].into_boxed_slice();
        }
        v
    }

    fn has_room(&self) -> bool {
        !self.strings[0].buf.is_empty()
    }

    /// Strike `freq` at `velocity` (0..=1). False if there's no room for
    /// the strings (a voice made without [`Voice::new`]).
    pub fn strike(&mut self, freq: f64, velocity: f32, params: &[f32], rate: f64) -> bool {
        if !self.has_room() {
            return false;
        }
        let p = |i: usize| params.get(i).copied().unwrap_or(0.0) as f64;
        let v = (velocity as f64).clamp(0.0, 1.0);
        let max_period = self.strings[0].buf.len() as f64 - 8.0;
        let mut freq = freq.max(1.0);
        while rate / freq > max_period {
            freq *= 2.0;
        }
        let key = 69.0 + 12.0 * (freq / 440.0).log2();
        self.key = key;
        let sustain = p(param::SUSTAIN).max(0.05);
        let t0 = sustain_time(key) * sustain;
        let th = sustain_time_high(key) * sustain.sqrt();
        let b = inharmonicity(key) * p(param::STIFFNESS);
        let detune = p(param::DETUNE);
        // Unisons: the first string is the loud one that fades quickly, the
        // others ring on. A little different from key to key, as tuned by
        // hand.
        let wobble = ((key * 7.31).sin() * 0.3 + 1.0) * detune;
        let (count, cents, gains, fades): (usize, [f64; 3], [f32; 3], [f64; 3]) = if key < 32.0 {
            (1, [0.0; 3], [1.0, 0.0, 0.0], [1.0; 3])
        } else if key < 46.0 {
            (2, [-0.5, 0.5, 0.0], [1.0, 0.8, 0.0], [0.4, 1.0, 1.0])
        } else {
            (3, [0.0, 0.9, -0.7], [1.0, 0.75, 0.7], [0.4, 1.0, 1.0])
        };
        self.count = count;
        for (k, s) in self.strings[..count].iter_mut().enumerate() {
            s.silence();
            let f = freq * (cents[k] * wobble / 1200.0).exp2();
            s.tune(f, b, t0 * fades[k], th * fades[k].sqrt(), rate);
            s.gain = gains[k];
        }

        // The hammer: shorter, so brighter, when it's harder or hits harder.
        let tau_ms = interp(
            &[(21.0, 0.32), (60.0, 0.13), (84.0, 0.07), (108.0, 0.035)],
            key,
        ) * (2.2 - 1.6 * v)
            * (1.6 - 1.2 * p(param::HAMMER));
        let period = rate / freq;
        self.tau = (tau_ms / 1000.0 * rate).max(0.3);
        let at = interp(
            &[(21.0, 8.0), (60.0, 8.5), (96.0, 11.0), (108.0, 14.0)],
            key,
        );
        self.comb = period / at;
        self.fall = (-1.0 / self.tau).exp();
        self.exp = 1.0;
        self.hammer_len = 14.0 * self.tau + self.comb + 2.0;
        // A unit-area pulse each period puts 2/period into the fundamental:
        // scale by the period so low notes aren't drowned out.
        let loudness = 10f64.powf(-(1.0 - v) * 1.5);
        self.amp = 1.2 * loudness * period.powf(0.8) / (self.tau * self.tau) * key_level(key);
        self.n = 0.0;

        let body = p(param::BODY) as f32;
        let thump = interp(&[(21.0, 90.0), (60.0, 160.0), (108.0, 420.0)], key);
        let r = (-PI * thump / 1.2 / rate).exp();
        self.knock_coef = [
            (2.0 * r * (TAU * thump / rate).cos()) as f32,
            -(r * r) as f32,
        ];
        self.knock_bp = [0.0; 2];
        self.knock_amp = 0.03 * body * (loudness as f32).powf(1.2);
        self.knock_fall = (-1.0 / (0.03 * rate)).exp() as f32;
        self.noise = Noise(0x9e37_79b9 ^ (key * 1000.0) as u32);

        let spread = (((key - 64.0) / 44.0).clamp(-1.0, 1.0) * p(param::WIDTH) * 0.7) as f32;
        let angle = (spread + 1.0) * std::f32::consts::FRAC_PI_4;
        self.pan = [
            angle.cos() * std::f32::consts::SQRT_2,
            angle.sin() * std::f32::consts::SQRT_2,
        ];
        self.env = 1.0;
        self.env_fall = (-1.0 / (0.03 * rate)).exp() as f32;
        self.damped = false;
        true
    }

    /// Let go of the key: the damper falls on the strings, except at the
    /// top, where there are none.
    pub fn damp(&mut self, params: &[f32], rate: f64) {
        if self.damped || self.key >= FIRST_UNDAMPED {
            return;
        }
        self.damped = true;
        let ms = params.get(param::DAMPER).copied().unwrap_or(200.0) as f64;
        let t = damped_time(self.key) * ms / 1000.0;
        for s in &mut self.strings[..self.count] {
            s.set_loss(t, 0.4 * t, rate);
        }
    }

    /// How loud it is now, roughly.
    pub fn level(&self) -> f32 {
        self.env
    }

    /// Whether it has died away.
    pub fn is_done(&self) -> bool {
        self.n > self.hammer_len && self.env < SILENT
    }

    pub fn next_frame(&mut self) -> [f32; 2] {
        let mut hammer = 0.0;
        if self.n < self.hammer_len {
            let n = self.n;
            let mut x = n * self.exp;
            if n >= self.comb {
                x -= (n - self.comb) * self.exp * (self.comb / self.tau).exp();
            }
            hammer = (self.amp * x) as f32;
            self.exp *= self.fall;
        }
        self.n += 1.0;
        let mut out = 0.0;
        for s in &mut self.strings[..self.count] {
            out += s.gain * s.next(hammer);
        }
        if self.knock_amp > 1e-6 {
            let x = self.noise.next() * self.knock_amp;
            let [c1, c2] = self.knock_coef;
            let y = x + c1 * self.knock_bp[0] + c2 * self.knock_bp[1];
            self.knock_bp = [y, self.knock_bp[0]];
            out += y * 0.15;
            self.knock_amp *= self.knock_fall;
        }
        self.env = (out.abs()).max(self.env * self.env_fall);
        [out * self.pan[0], out * self.pan[1]]
    }
}

/// Evens out loudness across the keyboard: the bass's many partials make
/// it louder than its fundamental alone suggests.
fn key_level(key: f64) -> f64 {
    // dB, measured: a hard hit's first quarter second comes out around
    // −15 dB in the bass to −18 dB at the top.
    const DB: &[(f64, f64)] = &[
        (21.0, -23.5),
        (27.0, -22.9),
        (33.0, -26.7),
        (39.0, -26.1),
        (45.0, -25.5),
        (51.0, -27.2),
        (57.0, -26.6),
        (63.0, -25.3),
        (69.0, -23.5),
        (75.0, -21.0),
        (81.0, -18.5),
        (87.0, -16.5),
        (93.0, -15.0),
        (99.0, -11.1),
        (105.0, -8.9),
        (108.0, -6.8),
    ];
    10f64.powf(interp(DB, key) / 20.0)
}

/// A Schroeder allpass with a delay line.
#[derive(Clone, Debug, Default)]
struct Diffuser {
    buf: Box<[f32]>,
    i: usize,
}

impl Diffuser {
    fn new(ms: f64, rate: f64) -> Self {
        let len = ((ms / 1000.0 * rate) as usize).max(1);
        Diffuser {
            buf: vec![0.0; len].into_boxed_slice(),
            i: 0,
        }
    }

    #[inline]
    fn next(&mut self, x: f32, g: f32) -> f32 {
        let d = self.buf[self.i];
        let w = x + g * d;
        self.buf[self.i] = w;
        self.i = (self.i + 1) % self.buf.len();
        d - g * w
    }
}

/// A string tuned to one pitch class, ringing along with the notes while
/// the dampers are up.
#[derive(Clone, Debug, Default)]
struct Sympathetic {
    buf: Box<[f32]>,
    i: usize,
    lp: f32,
}

/// The soundboard delays, left and right, in ms.
const DIFFUSE_MS: [[f64; 4]; 2] = [[1.13, 2.71, 4.33, 6.97], [1.31, 2.39, 4.87, 6.43]];
/// The lowest octave of sympathetic strings starts at this MIDI note (C2).
const SYMPATHETIC_FROM: f64 = 36.0;

/// What all of a piano track's notes go through: a soundboard, and the
/// sympathetic strings.
#[derive(Clone, Debug, Default)]
pub struct Body {
    diffusers: [[Diffuser; 4]; 2],
    lp: [f32; 2],
    strings: [Sympathetic; 12],
    feedback_up: f32,
    feedback_down: f32,
    feedback: f32,
    env: f32,
    env_fall: f32,
}

impl Body {
    pub fn new(rate: f64) -> Self {
        let mut body = Body::default();
        for (side, ms) in body.diffusers.iter_mut().zip(DIFFUSE_MS) {
            for (d, ms) in side.iter_mut().zip(ms) {
                *d = Diffuser::new(ms, rate);
            }
        }
        for (k, s) in body.strings.iter_mut().enumerate() {
            let freq = 440.0 * ((SYMPATHETIC_FROM + k as f64 - 69.0) / 12.0).exp2();
            let len = (rate / freq).round() as usize;
            s.buf = vec![0.0; len.max(1)].into_boxed_slice();
        }
        // Per period of the lowest string; near enough for all of them.
        let period = 1.0 / (440.0 * ((SYMPATHETIC_FROM - 69.0) / 12.0).exp2());
        body.feedback_down = 10f64.powf(-3.0 * period / 6.0) as f32;
        body.feedback_up = 10f64.powf(-3.0 * period / 0.08) as f32;
        body.env_fall = (-1.0 / (0.05 * rate)).exp() as f32;
        body
    }

    pub fn is_active(&self) -> bool {
        self.env > SILENT
    }

    pub fn silence(&mut self) {
        for d in self.diffusers.iter_mut().flatten() {
            d.buf.fill(0.0);
        }
        for s in &mut self.strings {
            s.buf.fill(0.0);
            s.lp = 0.0;
        }
        self.lp = [0.0; 2];
        self.env = 0.0;
    }

    /// The notes `dry` through the body, with knob values `params` and the
    /// pedal up or down.
    pub fn next_frame(&mut self, dry: [f32; 2], params: &[f32], pedal: bool) -> [f32; 2] {
        if self.diffusers[0][0].buf.is_empty() {
            return dry;
        }
        let p = |i: usize| params.get(i).copied().unwrap_or(0.0);
        let body = p(param::BODY);
        let resonance = p(param::RESONANCE);

        let target = if pedal {
            self.feedback_down
        } else {
            self.feedback_up
        };
        self.feedback += 0.001 * (target - self.feedback);
        let mono = 0.5 * (dry[0] + dry[1]);
        let mut ring = 0.0;
        if resonance > 0.0 {
            let fb = self.feedback;
            for s in &mut self.strings {
                let d = s.buf[s.i];
                s.lp += 0.6 * (d - s.lp);
                s.buf[s.i] = 0.002 * mono + fb * s.lp;
                s.i = (s.i + 1) % s.buf.len();
                ring += d;
            }
            ring *= resonance;
        }

        let mut out = [0.0; 2];
        for (c, side) in self.diffusers.iter_mut().enumerate() {
            let mut x = dry[c] + ring;
            for d in side.iter_mut() {
                x = d.next(x, 0.6);
            }
            self.lp[c] += 0.45 * (x - self.lp[c]);
            out[c] = dry[c] * (1.0 - 0.35 * body) + ring + body * 0.8 * self.lp[c];
        }
        self.env = (out[0].abs() + out[1].abs()).max(self.env * self.env_fall);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthvoice::{SynthKind, SynthPatch};

    const RATE: f64 = 48_000.0;

    fn patch() -> SynthPatch {
        SynthPatch::new(SynthKind::Piano, "piano")
    }

    fn render(v: &mut Voice, frames: usize) -> Vec<f32> {
        (0..frames).map(|_| v.next_frame()[0]).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// The strongest frequency near `around`, by a slow, Hann-windowed
    /// DFT scan.
    fn peak_near(x: &[f32], around: f64) -> f64 {
        let mut best = (0.0, 0.0);
        let mut f = around * 0.97;
        let len = x.len() as f64;
        while f < around * 1.03 {
            let w = TAU * f / RATE;
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, &s) in x.iter().enumerate() {
                let s = s as f64 * (0.5 - 0.5 * (TAU * n as f64 / len).cos());
                re += s * (w * n as f64).cos();
                im += s * (w * n as f64).sin();
            }
            let m = re * re + im * im;
            if m > best.1 {
                best = (f, m);
            }
            f += around * 0.0002;
        }
        best.0
    }

    #[test]
    fn notes_play_in_tune_across_the_keyboard() {
        let p = patch();
        for key in [21.0, 33.0, 45.0, 57.0, 69.0, 81.0, 93.0, 105.0] {
            let f = 440.0 * ((key - 69.0) / 12.0f64).exp2();
            let mut v = Voice::new(RATE);
            assert!(v.strike(f, 0.7, &p.params, RATE));
            // Long enough to tell 27.5 Hz to within a cent or so.
            let out = render(&mut v, 96_000);
            let got = peak_near(&out[4800..], f);
            let cents = 1200.0 * (got / f).log2();
            assert!(
                cents.abs() < 3.0,
                "key {key}: {got} Hz for {f} ({cents} ct)"
            );
        }
    }

    #[test]
    fn stiff_strings_have_sharp_partials() {
        let mut p = patch();
        let f = 440.0 * ((84.0 - 69.0) / 12.0f64).exp2();
        let fifth = |p: &SynthPatch| {
            let mut v = Voice::new(RATE);
            v.strike(f, 1.0, &p.params, RATE);
            let out = render(&mut v, 48_000);
            peak_near(&out[2400..], 5.0 * f) / (5.0 * f)
        };
        p.params[param::STIFFNESS] = 0.0;
        let soft = fifth(&p);
        p.params[param::STIFFNESS] = 1.0;
        let stiff = fifth(&p);
        let b = inharmonicity(84.0);
        let want = ((1.0 + 25.0 * b) / (1.0 + b)).sqrt();
        assert!((soft - 1.0).abs() < 0.003, "no stiffness: {soft}");
        // The allpasses only approximate it: near enough is fine.
        let stretch = (stiff - 1.0) / (want - 1.0);
        assert!((0.8..1.5).contains(&stretch), "{stiff} for {want}");
    }

    #[test]
    fn harder_hits_are_louder_and_brighter() {
        let p = patch();
        let measure = |velocity: f32| {
            let mut v = Voice::new(RATE);
            v.strike(220.0, velocity, &p.params, RATE);
            let out = render(&mut v, 9600);
            // Brightness: the slope's share of the level.
            let diff: Vec<f32> = out.windows(2).map(|w| w[1] - w[0]).collect();
            (rms(&out), rms(&diff) / rms(&out))
        };
        let (soft, soft_bright) = measure(0.3);
        let (hard, hard_bright) = measure(1.0);
        assert!(hard > soft * 4.0, "{hard} vs {soft}");
        assert!(
            hard_bright > soft_bright * 1.3,
            "{hard_bright} vs {soft_bright}"
        );
    }

    #[test]
    fn the_damper_stops_a_note_but_not_the_top_ones() {
        let p = patch();
        let tail = |key: f64, damp: bool| {
            let mut v = Voice::new(RATE);
            v.strike(
                440.0 * ((key - 69.0) / 12.0f64).exp2(),
                0.8,
                &p.params,
                RATE,
            );
            render(&mut v, 4800);
            if damp {
                v.damp(&p.params, RATE);
            }
            render(&mut v, 24_000);
            rms(&render(&mut v, 4800))
        };
        assert!(tail(60.0, true) < tail(60.0, false) * 0.01);
        assert_eq!(tail(96.0, true), tail(96.0, false), "no damper up there");
    }

    #[test]
    fn notes_die_away_and_end() {
        let p = patch();
        let mut v = Voice::new(RATE);
        v.strike(1000.0, 1.0, &p.params, RATE);
        let mut frames = 0;
        while !v.is_done() {
            let s = v.next_frame();
            assert!(s.iter().all(|s| s.is_finite() && s.abs() < 4.0));
            frames += 1;
            assert!(frames < RATE as usize * 30, "still ringing");
        }
        let mut v = Voice::new(RATE);
        v.strike(1000.0, 1.0, &p.params, RATE);
        v.damp(&p.params, RATE);
        let mut damped = 0;
        while !v.is_done() {
            v.next_frame();
            damped += 1;
        }
        assert!(damped < frames / 4, "{damped} vs {frames}");
    }

    #[test]
    fn a_voice_without_room_stays_silent() {
        let mut v = Voice::default();
        assert!(!v.strike(440.0, 1.0, &patch().params, RATE));
    }

    #[test]
    fn the_pedal_lets_the_body_ring_on() {
        let p = patch();
        let ring = |pedal: bool| {
            let mut body = Body::new(RATE);
            let mut v = Voice::new(RATE);
            v.strike(130.8, 1.0, &p.params, RATE);
            for _ in 0..4800 {
                body.next_frame(v.next_frame(), &p.params, pedal);
            }
            let tail: Vec<f32> = (0..9600)
                .map(|_| body.next_frame([0.0; 2], &p.params, pedal)[0])
                .collect();
            rms(&tail[4800..])
        };
        assert!(
            ring(true) > ring(false) * 5.0,
            "{} vs {}",
            ring(true),
            ring(false)
        );
    }
}
