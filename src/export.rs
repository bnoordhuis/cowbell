//! Export a song to a WAV file: once through the order list, then until
//! the last notes and echoes have died out.

use std::path::Path;

use crate::engine::Engine;
use crate::pattern::Song;

pub const RATE: u32 = 48_000;
/// Longest tail after the song ends, for releases and delay echoes.
const MAX_TAIL_S: f64 = 10.0;
/// The tail ends after this long below [`SILENCE`].
const QUIET_S: f64 = 0.5;
const SILENCE: f32 = 1e-4;

/// Length of one pass through the order list, in seconds.
pub fn song_seconds(song: &Song) -> f64 {
    let beats: f64 = song
        .order
        .iter()
        .filter_map(|&p| song.patterns.get(p))
        .map(|p| p.rows as f64 / p.rows_per_beat.max(1) as f64)
        .sum();
    beats * 60.0 / song.bpm.max(1.0)
}

/// Render the song once, with its tail, as stereo frames at [`RATE`].
pub fn render(song: &Song) -> Vec<[f32; 2]> {
    let (mut engine, _controller) = Engine::new(RATE, song.clone());
    engine.set_looping(false);
    engine.play();
    let mut out: Vec<[f32; 2]> = Vec::new();
    let mut buf = vec![0.0f32; 2 * 1024];
    let max_tail = (MAX_TAIL_S * RATE as f64) as usize;
    let quiet_frames = (QUIET_S * RATE as f64) as usize;
    let (mut tail, mut quiet) = (0usize, 0usize);
    loop {
        let sequencing = engine.is_playing();
        engine.process(&mut buf);
        for f in buf.as_chunks::<2>().0 {
            out.push(*f);
            if !sequencing {
                tail += 1;
                quiet = if f[0].abs().max(f[1].abs()) < SILENCE {
                    quiet + 1
                } else {
                    0
                };
            }
        }
        if !sequencing && (quiet >= quiet_frames || tail >= max_tail) {
            break;
        }
    }
    // Trim the silence the tail detection waited through.
    let end = out
        .iter()
        .rposition(|f| f[0].abs().max(f[1].abs()) >= SILENCE)
        .map_or(0, |i| i + 1);
    let song_end = (song_seconds(song) * RATE as f64).ceil() as usize;
    out.truncate(end.max(song_end).min(out.len()));
    out
}

/// Render the song to a 32-bit float stereo WAV.
pub fn export_wav(song: &Song, path: &Path) -> Result<f64, String> {
    let frames = render(song);
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let err = |e: hound::Error| format!("{}: {e}", path.display());
    let mut w = hound::WavWriter::create(path, spec).map_err(err)?;
    for [l, r] in &frames {
        w.write_sample(*l).map_err(err)?;
        w.write_sample(*r).map_err(err)?;
    }
    w.finalize().map_err(err)?;
    Ok(frames.len() as f64 / RATE as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{demo, synth};

    #[test]
    fn plays_once_then_rings_out() {
        let song = demo::song(synth::demo_break(RATE, demo::BPM as f32));
        let seconds = song_seconds(&song);
        let out = render(&song);
        let len = out.len() as f64 / RATE as f64;
        assert!(len >= seconds, "{len} < {seconds}");
        assert!(len < seconds + MAX_TAIL_S, "the tail ends");
        // The delay on the snare keeps echoing after the last row.
        let last_row = (seconds * RATE as f64) as usize;
        assert!(
            out[last_row..].iter().any(|f| f[0].abs() > 0.001),
            "has a tail"
        );
    }

    #[test]
    fn writes_a_wav() {
        let dir = std::env::temp_dir().join(format!("cowbell-test-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.wav");
        let song = demo::song(synth::demo_break(RATE, demo::BPM as f32));
        let seconds = export_wav(&song, &path).unwrap();
        let r = hound::WavReader::open(&path).unwrap();
        assert_eq!(r.spec().channels, 2);
        assert!((r.duration() as f64 / RATE as f64 - seconds).abs() < 1e-6);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
