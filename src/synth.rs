//! A synthetic drum break, so there's something to slice before any sample
//! files are loaded. Deterministic: the same input renders the same audio.

use std::f32::consts::TAU;

use crate::sample::Sample;

struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        // xorshift32
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
    }
}

#[derive(Clone, Copy)]
enum Hit {
    Kick,
    Snare,
    Hat,
}

fn render_hit(hit: Hit, rate: f32, out: &mut [[f32; 2]], noise: &mut Noise) {
    let mut phase = 0.0f32;
    for (i, frame) in out.iter_mut().enumerate() {
        let t = i as f32 / rate;
        let s = match hit {
            Hit::Kick => {
                let freq = 45.0 + 110.0 * (-t * 30.0).exp();
                phase += TAU * freq / rate;
                phase.sin() * (-t * 9.0).exp()
            }
            Hit::Snare => {
                phase += TAU * 190.0 / rate;
                0.5 * phase.sin() * (-t * 25.0).exp() + 0.6 * noise.next() * (-t * 14.0).exp()
            }
            Hit::Hat => 0.25 * noise.next() * (-t * 70.0).exp(),
        };
        frame[0] += s * 0.7;
        frame[1] += s * 0.7;
    }
}

/// Two bars of a funk-style break at `bpm`, with slice markers on every hit
/// so it's ready to sequence.
pub fn demo_break(sample_rate: u32, bpm: f32) -> Sample {
    // 16th-note grid, two bars. K = kick, S = snare, h = hat, . = rest.
    const GRID: &str = "K.h.S.hK.KhhS.h.K.h.S.hK..KhS.hS";
    let rate = sample_rate as f32;
    let step = (rate * 60.0 / bpm / 4.0) as usize;
    let mut frames = vec![[0.0f32; 2]; step * GRID.len()];
    let mut noise = Noise(0x1234_5678);
    let mut slices = Vec::new();
    for (i, c) in GRID.chars().enumerate() {
        let hit = match c {
            'K' => Hit::Kick,
            'S' => Hit::Snare,
            'h' => Hit::Hat,
            _ => continue,
        };
        let start = i * step;
        let end = (start + (rate * 0.4) as usize).min(frames.len());
        render_hit(hit, rate, &mut frames[start..end], &mut noise);
        slices.push(start);
    }
    let mut sample = Sample::new("demo_break", sample_rate, frames);
    sample.slices = slices;
    sample
}
