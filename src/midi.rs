//! MIDI keyboards: their messages as [`Key`]s for the engine, and (with
//! live audio) listening to one with midir.
//!
//! Every channel counts. Notes, the sustain pedal (controller 64) and the
//! "all notes off" kind of controllers are all that's read; the rest, and
//! system messages, are ignored.

use crate::engine::Key;

/// The keyboard event in one MIDI message, if it's one we play.
pub fn parse(msg: &[u8]) -> Option<Key> {
    let (&status, data) = msg.split_first()?;
    let at = |i: usize| data.get(i).copied().filter(|b| *b < 0x80);
    match status & 0xf0 {
        0x80 => Some(Key::Off { note: at(0)? }),
        0x90 => {
            let (note, velocity) = (at(0)?, at(1)?);
            // A note-on at velocity 0 is a note-off, by convention.
            Some(if velocity == 0 {
                Key::Off { note }
            } else {
                Key::On { note, velocity }
            })
        }
        0xb0 => match at(0)? {
            // Half way and up is down, as most pianos' pedals send it.
            64 => Some(Key::Pedal(at(1)? >= 64)),
            // All sound off, reset controllers, all notes off.
            120 | 121 | 123 => Some(Key::AllOff),
            _ => None,
        },
        _ => None,
    }
}

/// Which keyboard to listen to.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Choice {
    /// The first one there is.
    #[default]
    Any,
    /// None.
    Off,
    /// The one with this name (its [`base_name`]).
    Port(String),
}

/// A port's name without what ALSA adds at the end, its client and port
/// numbers ("Keys:Keys MIDI 1 20:0"), which can change when the keyboard
/// is plugged in again. Choices are remembered by this.
pub fn base_name(name: &str) -> &str {
    match name.rsplit_once(' ') {
        Some((base, nums))
            if nums
                .split_once(':')
                .is_some_and(|(a, b)| is_number(a) && is_number(b)) =>
        {
            base
        }
        _ => name,
    }
}

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Whether a port is ALSA's loopback, which isn't a keyboard: "any"
/// skips it.
pub fn is_through(name: &str) -> bool {
    name.contains("Midi Through")
}

#[cfg(feature = "live")]
pub use input::Input;

#[cfg(feature = "live")]
mod input {
    use midir::{Ignore, MidiInput, MidiInputConnection};

    use super::{Choice, base_name, is_through, parse};
    use crate::engine::Keys;

    const CLIENT: &str = "cowbell";

    /// The keyboard being listened to, if any.
    pub struct Input {
        keys: Keys,
        connection: Option<MidiInputConnection<()>>,
        port: Option<String>,
    }

    impl Input {
        /// Not listening yet; [`Input::listen`] starts.
        pub fn new(keys: Keys) -> Self {
            Input {
                keys,
                connection: None,
                port: None,
            }
        }

        /// The names of the MIDI inputs there are now.
        pub fn ports() -> Result<Vec<String>, String> {
            let input = MidiInput::new(CLIENT).map_err(|e| e.to_string())?;
            Ok(input
                .ports()
                .iter()
                .filter_map(|p| input.port_name(p).ok())
                .collect())
        }

        /// The port listened to, if any.
        pub fn port(&self) -> Option<&str> {
            self.port.as_deref()
        }

        /// Listen to the keyboard `choice` says, instead of the one before.
        /// Returns the port's name, `None` for none (off, or no keyboard
        /// for "any").
        pub fn listen(&mut self, choice: &Choice) -> Result<Option<String>, String> {
            if let Some(c) = self.connection.take() {
                c.close();
                // Whatever was held on it won't be let go now.
                push(&self.keys, crate::engine::Key::AllOff);
            }
            self.port = None;
            if *choice == Choice::Off {
                return Ok(None);
            }
            let mut input = MidiInput::new(CLIENT).map_err(|e| e.to_string())?;
            input.ignore(Ignore::All);
            let ports = input.ports();
            let named = |p: &&midir::MidiInputPort| input.port_name(p).unwrap_or_default();
            let port = match choice {
                Choice::Port(name) => ports.iter().find(|p| base_name(&named(p)) == name),
                _ => ports.iter().find(|p| !is_through(&named(p))),
            };
            let Some(port) = port.cloned() else {
                return match choice {
                    Choice::Port(name) => Err(format!("no MIDI input called {name}")),
                    _ => Ok(None),
                };
            };
            let name = input.port_name(&port).map_err(|e| e.to_string())?;
            let keys = self.keys.clone();
            let connection = input
                .connect(
                    &port,
                    "cowbell-in",
                    move |_stamp, msg, _| {
                        if let Some(key) = parse(msg) {
                            push(&keys, key);
                        }
                    },
                    (),
                )
                .map_err(|e| format!("{name}: {e}"))?;
            self.connection = Some(connection);
            self.port = Some(name.clone());
            Ok(Some(name))
        }
    }

    /// Hand a key to the engine. If its queue is full, the engine has
    /// stopped reading, and the key may as well go.
    fn push(keys: &Keys, key: crate::engine::Key) {
        if let Ok(mut q) = keys.lock() {
            let _ = q.push(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_and_the_pedal_on_any_channel() {
        assert_eq!(
            parse(&[0x90, 60, 100]),
            Some(Key::On {
                note: 60,
                velocity: 100
            })
        );
        assert_eq!(parse(&[0x93, 60, 0]), Some(Key::Off { note: 60 }), "vel 0");
        assert_eq!(parse(&[0x8f, 61, 40]), Some(Key::Off { note: 61 }));
        assert_eq!(parse(&[0xb0, 64, 127]), Some(Key::Pedal(true)));
        assert_eq!(parse(&[0xb5, 64, 63]), Some(Key::Pedal(false)));
        assert_eq!(parse(&[0xb0, 123, 0]), Some(Key::AllOff));
    }

    #[test]
    fn ports_are_known_by_name_without_their_numbers() {
        assert_eq!(base_name("Keys:Keys MIDI 1 20:0"), "Keys:Keys MIDI 1");
        assert_eq!(base_name("Keys MIDI 1"), "Keys MIDI 1");
        assert_eq!(base_name("Odd 1:x"), "Odd 1:x");
        assert!(is_through("Midi Through:Midi Through Port-0 14:0"));
    }

    #[test]
    fn other_and_broken_messages_are_ignored() {
        assert_eq!(parse(&[0xb0, 1, 64]), None, "mod wheel");
        assert_eq!(parse(&[0xe0, 0, 64]), None, "pitch bend");
        assert_eq!(parse(&[0xf8]), None, "clock");
        assert_eq!(parse(&[0x90, 60]), None, "cut short");
        assert_eq!(parse(&[0x90, 0x90, 100]), None, "a status byte for data");
        assert_eq!(parse(&[]), None);
    }
}
