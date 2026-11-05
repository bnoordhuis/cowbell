//! Sample buffers and the break slicer.

use std::path::Path;

const ONSET_HOP: usize = 128;
const ONSET_LOOKBACK: usize = 8;

/// A stereo sample with slice markers.
#[derive(Clone, Debug)]
pub struct Sample {
    pub name: String,
    pub sample_rate: u32,
    pub frames: Vec<[f32; 2]>,
    /// Sorted slice start frames. Always starts with 0.
    pub slices: Vec<usize>,
}

impl Sample {
    pub fn new(name: impl Into<String>, sample_rate: u32, frames: Vec<[f32; 2]>) -> Self {
        Sample {
            name: name.into(),
            sample_rate,
            frames,
            slices: vec![0],
        }
    }

    pub fn load_wav(path: &Path) -> Result<Self, hound::Error> {
        let mut reader = hound::WavReader::open(path)?;
        let spec = reader.spec();
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
            hound::SampleFormat::Int => {
                let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
                reader
                    .samples::<i32>()
                    .map(|s| s.map(|s| s as f32 * scale))
                    .collect::<Result<_, _>>()?
            }
        };
        let channels = spec.channels as usize;
        let frames = samples
            .chunks_exact(channels)
            .map(|c| {
                if channels == 1 {
                    [c[0], c[0]]
                } else {
                    [c[0], c[1]]
                }
            })
            .collect();
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        Ok(Sample::new(name, spec.sample_rate, frames))
    }

    /// Frame range of slice `i`, or `None` if there's no such slice.
    pub fn slice_bounds(&self, i: usize) -> Option<(usize, usize)> {
        let start = *self.slices.get(i)?;
        let end = self.slices.get(i + 1).copied().unwrap_or(self.frames.len());
        Some((start, end))
    }

    /// Replace the slice markers with detected transients.
    ///
    /// `sensitivity` is 0..=1; higher finds more (quieter) hits.
    /// `min_gap_ms` stops one hit from being split into several slices.
    pub fn detect_slices(&mut self, sensitivity: f32, min_gap_ms: f32) {
        // Two bands per hop: raw energy catches kicks and other hits with a
        // soft attack; energy of the first difference emphasizes the
        // broadband attack of hats and snares over whatever is still ringing.
        let mut prev = [0.0f32; 2];
        let bands: Vec<[f32; 2]> = self
            .frames
            .chunks(ONSET_HOP)
            .map(|hop| {
                let mut e = [0.0f32; 2];
                for &[l, r] in hop {
                    let (dl, dr) = (l - prev[0], r - prev[1]);
                    prev = [l, r];
                    e[0] += l * l + r * r;
                    e[1] += dl * dl + dr * dr;
                }
                e.map(|e| e / hop.len() as f32)
            })
            .collect();

        // At sensitivity 1 a hit must rise 1.5x and reach 0.1% of peak energy;
        // at 0 it needs 10x and 10% of peak.
        let s = sensitivity.clamp(0.0, 1.0);
        let ratio = 10.0 - 8.5 * s;
        let floor_scale = 10f32.powf(-1.0 - 2.0 * s);
        let floors: [f32; 2] =
            std::array::from_fn(|b| floor_scale * bands.iter().map(|e| e[b]).fold(0.0, f32::max));
        let min_gap = (min_gap_ms / 1000.0 * self.sample_rate as f32) as usize;

        let mut slices = vec![0];
        for i in 1..bands.len() {
            // Compare against the loudest of the last few hops, so the ripple
            // of a low kick isn't a new hit. Skip the hop right before this
            // one: a hit that starts near the end of it makes it partly loud.
            let window = &bands[i.saturating_sub(ONSET_LOOKBACK + 1)..i - 1];
            let rising = (0..2).any(|b| {
                let recent = window.iter().map(|e| e[b]).fold(1e-12, f32::max);
                bands[i][b] > floors[b] && bands[i][b] > recent * ratio
            });
            let frame = i * ONSET_HOP;
            if rising && frame - slices.last().unwrap() >= min_gap {
                slices.push(frame);
            }
        }
        self.slices = slices;
    }
}
