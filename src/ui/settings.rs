//! Settings that outlast a run: how big the UI is drawn, and which MIDI
//! keyboard to listen to.
//!
//! They live in `$XDG_CONFIG_HOME/cowbell/settings.ron`, or
//! `~/.config/cowbell/settings.ron` without it. A missing or unreadable
//! file just means the defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// egui's zoom factor: Ctrl+= and Ctrl+- change it.
    pub zoom: f32,
    pub midi: crate::midi::Choice,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            zoom: 1.3,
            midi: crate::midi::Choice::Any,
        }
    }
}

/// The smallest and largest zoom egui goes to.
const ZOOM_RANGE: std::ops::RangeInclusive<f32> = 0.2..=5.0;

/// Where the settings are kept, if there's a home to keep them in.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(base.join("cowbell").join("settings.ron"))
}

/// The settings in `path`, or the defaults if there are none (or they
/// can't be read). Values out of range are brought back into it.
pub fn load(path: Option<&Path>) -> Settings {
    let mut s: Settings = path
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| ron::from_str(&text).ok())
        .unwrap_or_default();
    if !s.zoom.is_finite() {
        s.zoom = Settings::default().zoom;
    }
    s.zoom = s.zoom.clamp(*ZOOM_RANGE.start(), *ZOOM_RANGE.end());
    s
}

/// Write the settings to `path`, making its folder if need be.
pub fn save(path: &Path, settings: &Settings) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| err(&e))?;
    }
    let text = ron::ser::to_string_pretty(settings, ron::ser::PrettyConfig::default())
        .map_err(|e| err(&e))?;
    std::fs::write(path, text + "\n").map_err(|e| err(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_come_back_as_saved_and_default_when_missing() {
        let dir = std::env::temp_dir().join(format!("cowbell-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("cowbell").join("settings.ron");
        assert_eq!(load(Some(&file)), Settings::default(), "no file yet");
        assert_eq!(load(None), Settings::default(), "nowhere to look");
        let saved = Settings {
            zoom: 1.7,
            midi: crate::midi::Choice::Port("Keys 1".into()),
        };
        save(&file, &saved).unwrap();
        assert_eq!(load(Some(&file)), saved);
        std::fs::write(&file, "(zoom: 40.0)").unwrap();
        assert_eq!(load(Some(&file)).zoom, 5.0, "kept in range");
        std::fs::write(&file, "()").unwrap();
        assert_eq!(load(Some(&file)), Settings::default(), "missing fields");
        std::fs::write(&file, "nonsense").unwrap();
        assert_eq!(load(Some(&file)), Settings::default(), "unreadable");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
