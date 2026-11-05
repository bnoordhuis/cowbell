//! A sample voice: plays one slice, with the per-cell effects applied.
//!
//! Voices hold no references to sample data; the engine passes the sample in
//! on every call. That way swapping the song never makes the audio thread
//! free sample memory.

use crate::pattern::{Fx, Trigger};
use crate::sample::Sample;

#[derive(Clone, Debug, Default)]
pub struct Voice {
    active: bool,
    instrument: usize,
    start: usize,
    len: usize,
    /// Playhead in source frames, relative to the (offset) slice start.
    pos: f64,
    offset: f64,
    rate: f64,
    reverse: bool,
    gain: f32,
    loop_len: Option<f64>,
    crush_levels: Option<f32>,
    /// Output frames between retriggers, and how many are still to come.
    retrig_period: f64,
    retrigs_left: u32,
    retrig_clock: f64,
    /// Output frames until the note is cut.
    cut_in: Option<f64>,
    /// Set when the note starts or retriggers; drives the track's gate.
    hit: bool,
}

impl Voice {
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn instrument(&self) -> usize {
        self.instrument
    }

    pub fn stop(&mut self) {
        self.active = false;
    }

    /// Start a note. `frames_per_row` is in output frames.
    pub fn trigger(
        &mut self,
        instrument: usize,
        sample: &Sample,
        trigger: Trigger,
        fx: impl Iterator<Item = Fx>,
        out_rate: f64,
        frames_per_row: f64,
    ) {
        let Some((start, end)) = sample.slice_bounds(trigger.slice as usize) else {
            self.active = false;
            return;
        };
        let v = trigger.velocity.min(127) as f32 / 127.0;
        *self = Voice {
            active: true,
            instrument,
            start,
            len: end - start,
            rate: sample.sample_rate as f64 / out_rate,
            gain: v * v,
            hit: true,
            ..Voice::default()
        };
        for fx in fx {
            match fx {
                Fx::Offset(n) => self.offset = self.len as f64 * n as f64 / 256.0,
                Fx::Retrig(n) if n > 1 => {
                    self.retrig_period = frames_per_row / n as f64;
                    self.retrigs_left = n as u32 - 1;
                    self.retrig_clock = self.retrig_period;
                }
                Fx::Retrig(_) => {}
                Fx::Reverse => self.reverse = true,
                Fx::Pitch(st) => self.rate *= 2f64.powf(st as f64 / 12.0),
                Fx::Crush(bits) => {
                    self.crush_levels = Some((1u32 << (bits.clamp(1, 16) - 1)) as f32)
                }
                Fx::Stutter(ms) => {
                    self.loop_len =
                        Some((ms.max(1) as f64 / 1000.0 * sample.sample_rate as f64).max(1.0))
                }
                Fx::Cut(n) => self.cut_in = Some(frames_per_row * n as f64 / 16.0),
            }
        }
    }

    /// Whether the note started or retriggered since the last call.
    pub fn take_hit(&mut self) -> bool {
        std::mem::take(&mut self.hit)
    }

    /// Cut a ringing note partway through the row (a `Cut` with no trigger).
    pub fn schedule_cut(&mut self, frames: f64) {
        self.cut_in = Some(frames);
    }

    /// Render one output frame. Returns silence once the note has ended.
    #[inline]
    pub fn next_frame(&mut self, sample: &Sample) -> [f32; 2] {
        if !self.active {
            return [0.0; 2];
        }
        if let Some(cut) = &mut self.cut_in {
            if *cut <= 0.0 {
                self.active = false;
                return [0.0; 2];
            }
            *cut -= 1.0;
        }
        if self.retrigs_left > 0 {
            if self.retrig_clock <= 0.0 {
                self.pos = 0.0;
                self.hit = true;
                self.retrigs_left -= 1;
                self.retrig_clock += self.retrig_period;
            }
            self.retrig_clock -= 1.0;
        }

        let avail = self.len as f64 - self.offset;
        let mut p = self.pos;
        if let Some(l) = self.loop_len {
            p %= l.min(avail);
        }
        if p >= avail - 1.0 {
            // Past the end. Keep the voice alive if a retrigger is pending.
            if self.retrigs_left == 0 {
                self.active = false;
            }
            return [0.0; 2];
        }
        let p = self.offset + p;
        let p = if self.reverse {
            self.len as f64 - 1.0 - p
        } else {
            p
        };
        let i = p as usize;
        let frac = (p - i as f64) as f32;
        let a = sample.frames[self.start + i];
        let b = sample.frames[self.start + (i + 1).min(self.len - 1)];
        self.pos += self.rate;

        let mut out = [0.0; 2];
        for ch in 0..2 {
            let mut s = a[ch] + (b[ch] - a[ch]) * frac;
            if let Some(levels) = self.crush_levels {
                s = (s * levels).round() / levels;
            }
            out[ch] = s * self.gain;
        }
        out
    }
}
