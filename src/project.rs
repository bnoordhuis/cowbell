//! Project files.
//!
//! A project is a RON text file (`song.cowbell`) holding the tracks and
//! their clips, the rack and the instrument list. Samples are referenced by
//! path, relative to the project file where possible. Samples that don't
//! come from a file (the demo break, bounces) are written as WAVs into a
//! folder next to the project, `song.samples/`.
//!
//! Cells are stored sparsely, row number to cell, so files stay small and
//! diff well.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::engine::MAX_TRACKS;
use crate::rack::Rack;
use crate::sample::Sample;
use crate::song::{Cell, Clip, Instrument, Song, Target, Tick, Track, nearest_resolution};
use crate::synthvoice::{SynthKind, SynthPatch};
use crate::tuning::Tuning;

pub const EXTENSION: &str = "cowbell";
const FORMAT: u32 = 2;

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    format: u32,
    bpm: f64,
    instruments: Vec<InstrumentFile>,
    tracks: Vec<TrackFile>,
    #[serde(default)]
    loop_range: Option<(Tick, Tick)>,
    rack: Rack,
    #[serde(default)]
    tuning: Tuning,
}

#[derive(Serialize, Deserialize)]
struct InstrumentFile {
    name: String,
    /// Samplers: the WAV file.
    #[serde(default)]
    file: PathBuf,
    #[serde(default)]
    slices: Vec<usize>,
    /// Synths: which kind, and its knobs.
    #[serde(default)]
    synth: Option<SynthFile>,
}

#[derive(Serialize, Deserialize)]
struct SynthFile {
    kind: SynthKind,
    params: Vec<f32>,
}

#[derive(Serialize, Deserialize)]
struct TrackFile {
    name: String,
    instrument: usize,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    solo: bool,
    /// The knob its `K` effects turn.
    #[serde(default)]
    automation: Option<Target>,
    clips: Vec<ClipFile>,
}

#[derive(Serialize, Deserialize)]
struct ClipFile {
    start: Tick,
    rows_per_beat: u32,
    rows: usize,
    cells: BTreeMap<usize, Cell>,
}

/// The folder for a project's own samples: `song.cowbell` → `song.samples`.
pub fn samples_dir(project: &Path) -> PathBuf {
    project.with_extension("samples")
}

/// A path relative to `dir` if it's inside it, otherwise unchanged.
fn relative_to(path: &Path, dir: &Path) -> PathBuf {
    path.strip_prefix(dir)
        .map_or_else(|_| path.to_owned(), Path::to_owned)
}

/// A file name for a generated sample: its index plus a tidied name.
fn sample_file_name(index: usize, name: &str) -> String {
    let tidy: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{index:02}-{}.wav", tidy.trim_matches('-'))
}

pub fn save(song: &Song, path: &Path) -> Result<(), String> {
    let err = |what: &str, e: &dyn std::fmt::Display| format!("{what}: {e}");
    let path = std::path::absolute(path).map_err(|e| err(&path.display().to_string(), &e))?;
    let dir = path.parent().ok_or("project path has no folder")?;

    let mut instruments = Vec::new();
    for (i, inst) in song.instruments.iter().enumerate() {
        let sample = match inst {
            Instrument::Sampler(s) => s,
            Instrument::Synth(p) => {
                let synth = Some(SynthFile {
                    kind: p.kind,
                    params: p.params.clone(),
                });
                instruments.push(InstrumentFile {
                    name: p.name.clone(),
                    file: PathBuf::new(),
                    slices: vec![],
                    synth,
                });
                continue;
            }
        };
        let file = match &sample.path {
            Some(p) if p.exists() => relative_to(p, dir),
            _ => {
                let samples = samples_dir(&path);
                std::fs::create_dir_all(&samples)
                    .map_err(|e| err(&samples.display().to_string(), &e))?;
                let file = samples.join(sample_file_name(i, &sample.name));
                sample
                    .save_wav(&file)
                    .map_err(|e| err(&file.display().to_string(), &e))?;
                relative_to(&file, dir)
            }
        };
        instruments.push(InstrumentFile {
            name: sample.name.clone(),
            file,
            slices: sample.slices.clone(),
            synth: None,
        });
    }

    let tracks = song
        .tracks
        .iter()
        .map(|t| TrackFile {
            name: t.name.clone(),
            instrument: t.instrument,
            mute: t.mute,
            solo: t.solo,
            automation: t.automation,
            clips: t
                .clips
                .iter()
                .map(|c| ClipFile {
                    start: c.start,
                    rows_per_beat: c.rows_per_beat,
                    rows: c.cells.len(),
                    cells: c
                        .cells
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| **c != Cell::default())
                        .map(|(row, c)| (row, *c))
                        .collect(),
                })
                .collect(),
        })
        .collect();

    let file = ProjectFile {
        format: FORMAT,
        bpm: song.bpm,
        instruments,
        tracks,
        loop_range: song.loop_range.as_ref().map(|r| (r.start, r.end)),
        rack: song.rack.clone(),
        tuning: song.tuning.clone(),
    };
    // One cell per line, arrays on one line: compact, and diffs show which
    // cells changed.
    let config = ron::ser::PrettyConfig::default()
        .struct_names(false)
        .compact_arrays(true)
        .depth_limit(4);
    let text =
        ron::ser::to_string_pretty(&file, config).map_err(|e| err("encoding project", &e))?;
    // Write to a temporary file and rename it over the old one, so a crash
    // mid-save can't leave a half-written project.
    let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
    std::fs::write(&tmp, text + "\n").map_err(|e| err(&tmp.display().to_string(), &e))?;
    std::fs::rename(&tmp, &path).map_err(|e| err(&path.display().to_string(), &e))?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Song, String> {
    let name = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|e| format!("{name}: {e}"))?;
    let file: ProjectFile = ron::from_str(&text).map_err(|e| format!("{name}: {e}"))?;
    if file.format != FORMAT {
        return Err(format!(
            "{name}: unsupported project format {}",
            file.format
        ));
    }
    let dir = path.parent().unwrap_or(Path::new("."));

    let mut instruments = Vec::new();
    for inst in &file.instruments {
        if let Some(synth) = &inst.synth {
            let mut patch = SynthPatch {
                name: inst.name.clone(),
                kind: synth.kind,
                params: synth.params.clone(),
            };
            patch.repair();
            instruments.push(Instrument::Synth(Arc::new(patch)));
            continue;
        }
        let wav = dir.join(&inst.file);
        let mut sample = Sample::load_wav(&wav).map_err(|e| format!("{}: {e}", wav.display()))?;
        sample.name = inst.name.clone();
        let mut slices: Vec<usize> = inst
            .slices
            .iter()
            .copied()
            .filter(|&s| s < sample.frames.len())
            .collect();
        slices.push(0);
        slices.sort_unstable();
        slices.dedup();
        sample.slices = slices;
        instruments.push(Instrument::Sampler(Arc::new(sample)));
    }
    if instruments.is_empty() {
        return Err(format!("{name}: no instruments"));
    }

    let tracks: Vec<Track> = file
        .tracks
        .into_iter()
        .take(MAX_TRACKS)
        .map(|t| {
            let mut track = Track::new(&t.name, t.instrument.min(instruments.len() - 1));
            track.mute = t.mute;
            track.solo = t.solo;
            track.automation = t.automation;
            for c in t.clips {
                let mut clip = Clip::new(c.start, nearest_resolution(c.rows_per_beat), c.rows);
                for (row, cell) in c.cells {
                    if let Some(slot) = clip.cells.get_mut(row) {
                        *slot = cell;
                    }
                }
                // An overlapping or empty clip can't play; drop it.
                let _ = track.insert(clip);
            }
            track
        })
        .collect();

    let mut rack = file.rack;
    rack.repair();
    rack.sync_tracks(tracks.len());
    Ok(Song {
        bpm: file.bpm.clamp(20.0, 400.0),
        instruments,
        tracks,
        loop_range: file.loop_range.filter(|(a, b)| a < b).map(|(a, b)| a..b),
        rack,
        tuning: file.tuning,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{demo, synth};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cowbell-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn frames(song: &Song, i: usize) -> &[[f32; 2]] {
        let Instrument::Sampler(s) = &song.instruments[i] else {
            panic!("not a sample")
        };
        &s.frames
    }

    #[test]
    fn round_trip() {
        let dir = temp_dir("round-trip");
        let path = dir.join("song.cowbell");
        let song = demo::song(synth::demo_break(48_000, 172.0));
        save(&song, &path).unwrap();
        assert!(dir.join("song.samples").join("00-demo_break.wav").exists());

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.bpm, song.bpm);
        assert!(loaded.rack.same_patch(&song.rack));
        assert_eq!(loaded.tracks.len(), song.tracks.len());
        for (a, b) in loaded.tracks.iter().zip(&song.tracks) {
            assert_eq!(
                (&a.name, a.instrument, a.mute, a.solo, a.automation),
                (&b.name, b.instrument, b.mute, b.solo, b.automation)
            );
            assert_eq!(a.clips, b.clips);
        }
        assert_eq!(
            frames(&loaded, 0),
            frames(&song, 0),
            "32-bit float WAV is lossless"
        );
        let Instrument::Sampler(s) = &loaded.instruments[0] else {
            panic!("not a sample")
        };
        assert_eq!(s.slices, synth::demo_break(48_000, 172.0).slices);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn samples_from_files_stay_where_they_are() {
        let dir = temp_dir("external");
        let wav = dir.join("break.wav");
        synth::demo_break(48_000, 172.0).save_wav(&wav).unwrap();
        let mut song = demo::song(Sample::load_wav(&wav).unwrap());
        song.instruments.truncate(1);
        let path = dir.join("song.cowbell");
        save(&song, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("file: \"break.wav\""),
            "relative path:\n{text}"
        );
        assert!(!samples_dir(&path).exists(), "nothing copied");
        load(&path).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn empty_cells_are_not_written() {
        let dir = temp_dir("sparse");
        let path = dir.join("song.cowbell");
        let song = demo::song(synth::demo_break(48_000, 172.0));
        save(&song, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let used = song
            .tracks
            .iter()
            .flat_map(|t| &t.clips)
            .flat_map(|c| &c.cells)
            .filter(|c| **c != Cell::default());
        assert_eq!(text.matches("trigger:").count(), used.count());
        // One cell per line.
        assert!(
            text.lines()
                .any(|l| l.trim() == "29: (trigger: None, fx: (Some(Cut(4)), None)),"),
            "{text}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mixer_state_and_loop_survive() {
        let dir = temp_dir("mixer");
        let path = dir.join("song.cowbell");
        let mut song = demo::song(synth::demo_break(48_000, 172.0));
        song.tracks[1].mute = true;
        song.tracks[0].solo = true;
        song.tracks[0].automation = Some(Target::Synth { param: 2 });
        song.loop_range = Some(768..1536);
        save(&song, &path).unwrap();
        let loaded = load(&path).unwrap();
        assert!(loaded.tracks[1].mute && loaded.tracks[0].solo);
        assert_eq!(
            loaded.tracks[0].automation,
            Some(Target::Synth { param: 2 })
        );
        assert_eq!(loaded.loop_range, Some(768..1536));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn overlapping_clips_are_dropped_on_load() {
        let dir = temp_dir("overlap");
        let path = dir.join("song.cowbell");
        let song = demo::song(synth::demo_break(48_000, 172.0));
        save(&song, &path).unwrap();
        // Move the second clip of the first track onto the first.
        let text = std::fs::read_to_string(&path).unwrap();
        let second = song.tracks[0].clips[1].start;
        let text = text.replacen(&format!("start: {second},"), "start: 0,", 1);
        std::fs::write(&path, text).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.tracks[0].clips.len(), song.tracks[0].clips.len() - 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn broken_cables_are_dropped_on_load() {
        let dir = temp_dir("repair");
        let path = dir.join("song.cowbell");
        let mut song = demo::song(synth::demo_break(48_000, 172.0));
        let n = song.rack.cables.len();
        let mut bogus = song.rack.cables[0];
        bogus.from.device = 9999;
        song.rack.cables.push(bogus);
        save(&song, &path).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.rack.cables.len(), n);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
