//! Tunings: which frequency each note number plays. Equal temperament by
//! default; anything else comes from a Scala (`.scl`) file.
//!
//! Notes are MIDI-style numbers: 60 is middle C (C4).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tuning {
    pub name: String,
    /// The scale's degrees above the base note, in cents, ending with the
    /// period (1200 for an octave). Degree 0 (the base itself) is implied.
    pub cents: Vec<f64>,
    /// The note number that plays `base_freq`.
    pub base_note: u8,
    pub base_freq: f64,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            name: "12-tone equal".into(),
            cents: (1..=12).map(|i| i as f64 * 100.0).collect(),
            base_note: 60,
            base_freq: 261.625_565_300_598_6,
        }
    }
}

impl Tuning {
    pub fn is_equal_temperament(&self) -> bool {
        *self == Tuning::default()
    }

    /// The frequency of `note`, in Hz.
    pub fn freq(&self, note: u8) -> f64 {
        let n = self.cents.len().max(1) as i32;
        let period = self.cents.last().copied().unwrap_or(1200.0);
        let steps = note as i32 - self.base_note as i32;
        let (periods, degree) = (steps.div_euclid(n), steps.rem_euclid(n));
        let cents = periods as f64 * period
            + if degree == 0 {
                0.0
            } else {
                self.cents[degree as usize - 1]
            };
        self.base_freq * (cents / 1200.0).exp2()
    }

    /// Parse a Scala scale file. Pitches with a `.` are cents; others are
    /// ratios like `3/2` or whole numbers like `2`.
    pub fn from_scala(text: &str) -> Result<Tuning, String> {
        let mut lines = text.lines().map(str::trim).filter(|l| !l.starts_with('!'));
        let name = lines.next().ok_or("empty file")?.to_owned();
        let count: usize = lines
            .next()
            .and_then(|l| l.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .ok_or("missing note count")?;
        if count == 0 || count > 1024 {
            return Err(format!("unsupported note count {count}"));
        }
        let mut cents = Vec::with_capacity(count);
        for line in lines.take(count) {
            let word = line.split_whitespace().next().ok_or("blank pitch line")?;
            let c = if word.contains('.') {
                word.parse::<f64>()
                    .map_err(|_| format!("bad cents value {word:?}"))?
            } else {
                let (num, den) = word.split_once('/').unwrap_or((word, "1"));
                let num: f64 = num.parse().map_err(|_| format!("bad ratio {word:?}"))?;
                let den: f64 = den.parse().map_err(|_| format!("bad ratio {word:?}"))?;
                if num <= 0.0 || den <= 0.0 {
                    return Err(format!("bad ratio {word:?}"));
                }
                1200.0 * (num / den).log2()
            };
            cents.push(c);
        }
        if cents.len() != count {
            return Err(format!("expected {count} pitches, found {}", cents.len()));
        }
        if cents.last().is_none_or(|&p| p <= 0.0) {
            return Err("the last pitch (the period) must be above the base".into());
        }
        let name = if name.is_empty() {
            "unnamed scale".into()
        } else {
            name
        };
        Ok(Tuning {
            name,
            cents,
            ..Tuning::default()
        })
    }
}

/// Tunings that come with the app, as Scala text, by the name you type to
/// load one.
pub const BUILTIN: &[(&str, &str)] = &[
    (
        "just",
        "5-limit just intonation\n12\n16/15\n9/8\n6/5\n5/4\n4/3\n45/32\n3/2\n8/5\n5/3\n9/5\n15/8\n2/1\n",
    ),
    (
        "stretched",
        "12-tone with a slightly wide octave (1210 cents)\n12\n100.833\n201.667\n302.5\n403.333\n504.167\n605.0\n705.833\n806.667\n907.5\n1008.333\n1109.167\n1210.0\n",
    ),
    (
        "slendro",
        "slendro, 5 roughly equal steps\n5\n231.0\n474.0\n717.0\n955.0\n1200.0\n",
    ),
    (
        "pelog",
        "pelog, 7 uneven steps\n7\n120.0\n270.0\n540.0\n670.0\n785.0\n950.0\n1200.0\n",
    ),
    (
        "19-tet",
        "19-tone equal\n19\n63.158\n126.316\n189.474\n252.632\n315.789\n378.947\n442.105\n505.263\n568.421\n631.579\n694.737\n757.895\n821.053\n884.211\n947.368\n1010.526\n1073.684\n1136.842\n1200.0\n",
    ),
];

/// A built-in tuning by name (see [`BUILTIN`]); "12-tet" gives the default.
pub fn builtin(name: &str) -> Option<Tuning> {
    let name = name.trim().to_lowercase();
    if name == "12-tet" || name == "equal" {
        return Some(Tuning::default());
    }
    let (_, text) = BUILTIN.iter().find(|(n, _)| *n == name)?;
    Some(Tuning::from_scala(text).expect("built-in tunings parse"))
}

/// The note range that fits a three-character name: C-0 to B-8.
pub const LOWEST_NOTE: u8 = 12;
pub const HIGHEST_NOTE: u8 = 119;

/// A note's name: `C4`, `C#4`.
pub fn note_name(note: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", NAMES[note as usize % 12], note as i32 / 12 - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_tunings_parse_and_differ_from_equal() {
        for (name, _) in BUILTIN {
            let t = builtin(name).unwrap();
            assert!(!t.is_equal_temperament(), "{name}");
            assert!(t.freq(72) > t.freq(60), "{name}");
        }
        assert!(builtin(" Slendro ").is_some());
        assert!(builtin("12-tet").unwrap().is_equal_temperament());
        assert!(builtin("nope").is_none());
    }

    #[test]
    fn equal_temperament() {
        let t = Tuning::default();
        assert!((t.freq(69) - 440.0).abs() < 1e-9, "A-4");
        assert!((t.freq(72) - 2.0 * t.freq(60)).abs() < 1e-9);
        assert!((t.freq(48) - t.freq(60) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn scala_file() {
        let scl = "! pythagorean.scl\n!\nPythagorean pentatonic\n 5\n!\n 9/8\n 81/64\n 3/2\n 27/16\n 2/1\n";
        let t = Tuning::from_scala(scl).unwrap();
        assert_eq!(t.name, "Pythagorean pentatonic");
        assert_eq!(t.cents.len(), 5);
        let fifth = t.freq(63) / t.freq(60);
        assert!(
            (fifth - 1.5).abs() < 1e-9,
            "degree 3 is a pure fifth: {fifth}"
        );
        assert!(
            (t.freq(65) / t.freq(60) - 2.0).abs() < 1e-9,
            "five steps per octave"
        );
        assert!((t.freq(55) / t.freq(60) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn scala_cents_and_errors() {
        assert!(
            Tuning::from_scala("quarter tones\n24\n").is_err(),
            "missing pitches"
        );
        let text: String = std::iter::once("quarter tones\n24\n".to_owned())
            .chain((1..=24).map(|i| format!("{:.1}\n", i as f64 * 50.0)))
            .collect();
        let t = Tuning::from_scala(&text).unwrap();
        assert!((t.freq(61) / t.freq(60) - (50.0f64 / 1200.0).exp2()).abs() < 1e-12);
        assert!(Tuning::from_scala("x\n1\n0/1\n").is_err());
    }

    #[test]
    fn names() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(61), "C#4");
        assert_eq!(note_name(69), "A4");
        assert_eq!(note_name(LOWEST_NOTE), "C0");
        assert_eq!(note_name(HIGHEST_NOTE), "B8");
    }
}
