//! MIDI keyboards: every input port is opened as it appears, and note
//! messages come out of [`Inputs::poll`]. Plug a keyboard in while the app
//! runs and it's picked up within a couple of seconds.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;
use midir::{Ignore, MidiInput, MidiInputConnection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    NoteOn { note: u8, velocity: u8 },
    NoteOff { note: u8 },
}

/// Read a note message, on any channel. A note-on with velocity 0 is a
/// note-off, as many keyboards send it.
pub fn parse(bytes: &[u8]) -> Option<Msg> {
    let (&status, data) = bytes.split_first()?;
    let (note, velocity) = (*data.first()? & 0x7f, *data.get(1)? & 0x7f);
    match status & 0xf0 {
        0x90 if velocity > 0 => Some(Msg::NoteOn { note, velocity }),
        0x90 | 0x80 => Some(Msg::NoteOff { note }),
        _ => None,
    }
}

const CLIENT: &str = "cowbell";
const RESCAN: Duration = Duration::from_secs(2);

pub struct Inputs {
    connections: Vec<(String, MidiInputConnection<()>)>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    ctx: egui::Context,
    next_scan: Instant,
}

impl Inputs {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        Inputs {
            connections: Vec::new(),
            tx,
            rx,
            ctx,
            next_scan: Instant::now(),
        }
    }

    /// Names of the connected ports.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.connections.iter().map(|(n, _)| n.as_str())
    }

    /// Messages since the last call. Every couple of seconds this also
    /// looks for new ports and drops gone ones; `connected` gets the names
    /// of new ones.
    pub fn poll(&mut self, connected: &mut Vec<String>) -> Vec<Msg> {
        if Instant::now() >= self.next_scan {
            self.next_scan = Instant::now() + RESCAN;
            self.scan(connected);
            self.ctx.request_repaint_after(RESCAN);
        }
        self.rx.try_iter().collect()
    }

    fn scan(&mut self, connected: &mut Vec<String>) {
        let Ok(probe) = MidiInput::new(CLIENT) else {
            return;
        };
        let ports: Vec<(String, midir::MidiInputPort)> = probe
            .ports()
            .into_iter()
            .filter_map(|p| Some((probe.port_name(&p).ok()?, p)))
            // Our own ports, and the loopback every ALSA system has.
            .filter(|(name, _)| !name.starts_with(CLIENT) && !name.contains("Midi Through"))
            .collect();
        self.connections
            .retain(|(name, _)| ports.iter().any(|(n, _)| n == name));
        for (name, port) in ports {
            if self.connections.iter().any(|(n, _)| *n == name) {
                continue;
            }
            let Ok(mut input) = MidiInput::new(CLIENT) else {
                return;
            };
            input.ignore(Ignore::All);
            let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
            let callback = move |_: u64, bytes: &[u8], _: &mut ()| {
                if let Some(msg) = parse(bytes) {
                    let _ = tx.send(msg);
                    ctx.request_repaint();
                }
            };
            if let Ok(conn) = input.connect(&port, "cowbell-in", callback, ()) {
                connected.push(name.clone());
                self.connections.push((name, conn));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_on_any_channel() {
        assert_eq!(
            parse(&[0x90, 60, 100]),
            Some(Msg::NoteOn {
                note: 60,
                velocity: 100
            })
        );
        assert_eq!(
            parse(&[0x9f, 36, 1]),
            Some(Msg::NoteOn {
                note: 36,
                velocity: 1
            })
        );
        assert_eq!(parse(&[0x83, 60, 64]), Some(Msg::NoteOff { note: 60 }));
        // Running-status style note-on with velocity 0 is an off.
        assert_eq!(parse(&[0x90, 60, 0]), Some(Msg::NoteOff { note: 60 }));
    }

    #[test]
    fn ignores_everything_else() {
        assert_eq!(parse(&[0xb0, 1, 64]), None); // mod wheel
        assert_eq!(parse(&[0xf8]), None); // clock
        assert_eq!(parse(&[0x90, 60]), None); // truncated
        assert_eq!(parse(&[]), None);
    }
}
