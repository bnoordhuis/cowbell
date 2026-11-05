//! A sample voice: plays one slice, with the per-cell effects applied.
//!
//! Voices hold no references to sample data; the engine passes the sample in
//! on every call. That way swapping the song never makes the audio thread
//! free sample memory.
//!
//! With declick on, a voice never stops dead while it still has sound left:
//! a cut fades out over [`FADE_MS`], ending on the frame the cut is due, and
//! a voice that is stolen or retriggered hands its sound to a tail voice
//! (see [`Voice::steal_into`]) that fades it while the new note starts at
//! full level. Attacks are never faded.

use crate::pattern::{Fx, Trigger};
use crate::sample::Sample;

/// Length of a declick fade.
pub const FADE_MS: f64 = 1.5;

/// Declick fade length in output frames.
pub fn fade_frames(out_rate: f64) -> u32 {
    (out_rate * FADE_MS / 1000.0).round().max(1.0) as u32
}

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
    /// How long the fade before the cut is; shorter than `fade_len` if the
    /// note is cut sooner than that after it starts.
    cut_fade: u32,
    /// Full declick fade length, in output frames.
    fade_len: u32,
    /// While fading out: the frames left, and the fade's total length.
    fade_left: u32,
    fade_total: u32,
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

    /// Whether the voice still has sound to make, now or after a pending
    /// retrigger. A voice that has played to the end of its slice has none,
    /// so stealing it leaves nothing to fade.
    pub fn is_sounding(&self) -> bool {
        self.active && (self.retrigs_left > 0 || self.source_pos().is_some())
    }

    /// Whether the next frame restarts the note for a retrigger.
    pub fn retrig_due(&self) -> bool {
        self.active && self.retrigs_left > 0 && self.retrig_clock <= 0.0
    }

    /// Hand the current sound to `tail`, which fades it out while this voice
    /// goes on to start a new note (or retrigger). Leaves `tail` alone if
    /// there is nothing audible to fade.
    pub fn steal_into(&self, tail: &mut Voice) {
        if !self.active || self.source_pos().is_none() {
            return;
        }
        *tail = self.clone();
        tail.hit = false;
        tail.retrigs_left = 0;
        tail.cut_in = None;
        // A voice that was already fading keeps its place in the fade.
        if tail.fade_left == 0 {
            tail.start_fade(self.fade_len);
        }
    }

    fn start_fade(&mut self, frames: u32) {
        self.cut_in = None;
        if frames == 0 {
            self.active = false;
        } else {
            self.fade_left = frames;
            self.fade_total = frames;
        }
    }

    /// Where the playhead is in the slice, or `None` past its end.
    fn source_pos(&self) -> Option<f64> {
        let avail = self.len as f64 - self.offset;
        let mut p = self.pos;
        if let Some(l) = self.loop_len {
            p %= l.min(avail);
        }
        // The last frame of the slice is at `avail - 1`; it plays too, so
        // slices played back to back don't drop a frame at each join.
        (p <= avail - 1.0).then_some(p)
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
            fade_len: fade_frames(out_rate),
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
        if let Some(cut) = self.cut_in {
            // Frames the note sounds before the cut.
            self.cut_fade = self.fade_len.min(cut.max(0.0).ceil() as u32);
        }
    }

    /// Whether the note started or retriggered since the last call.
    pub fn take_hit(&mut self) -> bool {
        std::mem::take(&mut self.hit)
    }

    /// Cut a ringing note partway through the row (a `Cut` with no trigger).
    pub fn schedule_cut(&mut self, frames: f64) {
        self.cut_in = Some(frames);
        // The note is already sounding, so it gets a full fade even if that
        // runs a little past the cut.
        self.cut_fade = self.fade_len;
    }

    /// Render one output frame. Returns silence once the note has ended.
    /// With `declick`, a cut fades out instead of stopping dead.
    #[inline]
    pub fn next_frame(&mut self, sample: &Sample, declick: bool) -> [f32; 2] {
        if !self.active {
            return [0.0; 2];
        }
        if let Some(cut) = self.cut_in {
            if declick && self.fade_left > 0 {
                // Already fading out; that ends the note soon enough.
                self.cut_in = None;
            } else if declick && cut <= self.cut_fade as f64 {
                // Exactly `cut_fade` frames are left before the cut (or the
                // cut is overdue and the note gets a full fade from now).
                self.start_fade(self.cut_fade);
                if !self.active {
                    return [0.0; 2];
                }
            } else if cut <= 0.0 {
                self.active = false;
                return [0.0; 2];
            } else {
                self.cut_in = Some(cut - 1.0);
            }
        }
        // Gain for this frame of a fade-out: (n / (n + 1)) down to
        // (1 / (n + 1)) over n frames, then silence.
        let fade = (self.fade_left > 0).then(|| {
            let g = self.fade_left as f32 / (self.fade_total + 1) as f32;
            self.fade_left -= 1;
            if self.fade_left == 0 {
                // This is the fade's last frame.
                self.active = false;
                self.retrigs_left = 0;
            }
            g
        });
        if self.retrigs_left > 0 {
            if self.retrig_clock <= 0.0 {
                self.pos = 0.0;
                self.hit = true;
                self.retrigs_left -= 1;
                self.retrig_clock += self.retrig_period;
            }
            self.retrig_clock -= 1.0;
        }

        let Some(p) = self.source_pos() else {
            // Past the end. Keep the voice alive if a retrigger is pending.
            if self.retrigs_left == 0 {
                self.active = false;
            }
            return [0.0; 2];
        };
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
            if let Some(g) = fade {
                out[ch] *= g;
            }
        }
        out
    }
}
