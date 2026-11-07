//! The rack: devices and the cables between them. Plain data like the
//! patterns; [`crate::graph`] turns it into something the engine can run.
//!
//! Cables go from an output jack to an input jack. An output can feed any
//! number of inputs; an input takes one cable. Cables that would make a loop
//! are refused, so the rack always has a processing order.

use serde::{Deserialize, Serialize};

use crate::engine::MAX_TRACKS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Audio,
    /// Control voltage: a modulation signal, carried in the left channel.
    Cv,
}

pub struct PortSpec {
    pub name: &'static str,
    pub signal: Signal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    Linear,
    /// For frequencies and times: equal knob travel per octave.
    Exp,
    /// Whole numbers only.
    Steps,
}

pub struct ParamSpec {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub curve: Curve,
    pub unit: &'static str,
    /// Names for `Steps` params, e.g. filter modes.
    pub labels: &'static [&'static str],
}

impl ParamSpec {
    pub const fn new(
        name: &'static str,
        min: f32,
        max: f32,
        default: f32,
        curve: Curve,
        unit: &'static str,
    ) -> Self {
        ParamSpec {
            name,
            min,
            max,
            default,
            curve,
            unit,
            labels: &[],
        }
    }

    pub const fn labelled(
        name: &'static str,
        default: f32,
        labels: &'static [&'static str],
    ) -> Self {
        let max = (labels.len() - 1) as f32;
        ParamSpec {
            name,
            min: 0.0,
            max,
            default,
            curve: Curve::Steps,
            unit: "",
            labels,
        }
    }

    pub fn clamp(&self, v: f32) -> f32 {
        let v = v.clamp(self.min, self.max);
        if self.curve == Curve::Steps {
            v.round()
        } else {
            v
        }
    }

    /// Knob position (0..=1) for a value.
    pub fn to_knob(&self, v: f32) -> f32 {
        let v = self.clamp(v);
        match self.curve {
            Curve::Exp => (v / self.min).ln() / (self.max / self.min).ln(),
            _ => (v - self.min) / (self.max - self.min),
        }
    }

    /// Value for a knob position (0..=1).
    pub fn from_knob(&self, k: f32) -> f32 {
        let k = k.clamp(0.0, 1.0);
        self.clamp(match self.curve {
            Curve::Exp => self.min * (self.max / self.min).powf(k),
            _ => self.min + k * (self.max - self.min),
        })
    }

    /// For switches (params with labels): the choice after `v`, wrapping
    /// around. Clicking such a knob steps through its choices.
    pub fn next_choice(&self, v: f32) -> Option<f32> {
        if self.labels.is_empty() {
            return None;
        }
        Some((v.round() + 1.0) % self.labels.len() as f32)
    }

    pub fn format(&self, v: f32) -> String {
        if let Some(label) = self.labels.get(v.round() as usize) {
            return (*label).to_owned();
        }
        let digits = match self.curve {
            Curve::Steps => 0,
            _ if v.abs() >= 100.0 => 0,
            _ if v.abs() >= 10.0 => 1,
            _ => 2,
        };
        format!("{v:.digits$}{}", self.unit)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// A pattern track's voice. Only created and removed with the track.
    Track(usize),
    Crusher,
    Filter,
    Delay,
    Lfo,
    Mixer,
    Master,
}

const fn port(name: &'static str, signal: Signal) -> PortSpec {
    PortSpec { name, signal }
}

const AUDIO_IN: &[PortSpec] = &[port("in", Signal::Audio)];
const AUDIO_OUT: &[PortSpec] = &[port("out", Signal::Audio)];
pub const MIXER_CHANNELS: usize = 6;

impl Kind {
    /// Devices the user can add from the rack view.
    pub const ADDABLE: [Kind; 5] = [
        Kind::Crusher,
        Kind::Filter,
        Kind::Delay,
        Kind::Lfo,
        Kind::Mixer,
    ];

    /// Audio in, audio out: what goes in a track's effects chain.
    pub fn is_effect(self) -> bool {
        matches!(self, Kind::Crusher | Kind::Filter | Kind::Delay)
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Track(_) => "track",
            Kind::Crusher => "crusher",
            Kind::Filter => "filter",
            Kind::Delay => "delay",
            Kind::Lfo => "lfo",
            Kind::Mixer => "mixer",
            Kind::Master => "master",
        }
    }

    pub fn removable(self) -> bool {
        !matches!(self, Kind::Track(_) | Kind::Master)
    }

    pub fn inputs(self) -> &'static [PortSpec] {
        const FILTER: &[PortSpec] = &[port("in", Signal::Audio), port("cutoff", Signal::Cv)];
        const LFO: &[PortSpec] = &[port("clock", Signal::Cv)];
        const MIXER: &[PortSpec] = &[
            port("1", Signal::Audio),
            port("2", Signal::Audio),
            port("3", Signal::Audio),
            port("4", Signal::Audio),
            port("5", Signal::Audio),
            port("6", Signal::Audio),
        ];
        match self {
            Kind::Track(_) => &[],
            Kind::Crusher | Kind::Delay | Kind::Master => AUDIO_IN,
            Kind::Filter => FILTER,
            Kind::Lfo => LFO,
            Kind::Mixer => MIXER,
        }
    }

    pub fn outputs(self) -> &'static [PortSpec] {
        const TRACK: &[PortSpec] = &[port("out", Signal::Audio), port("gate", Signal::Cv)];
        const LFO: &[PortSpec] = &[port("out", Signal::Cv)];
        match self {
            Kind::Track(_) => TRACK,
            Kind::Crusher | Kind::Filter | Kind::Delay | Kind::Mixer => AUDIO_OUT,
            Kind::Lfo => LFO,
            Kind::Master => &[],
        }
    }

    pub fn params(self) -> &'static [ParamSpec] {
        use Curve::*;
        const LEVEL: ParamSpec = ParamSpec::new("", 0.0, 1.5, 1.0, Linear, "");
        const MIXER: &[ParamSpec] = &[
            ParamSpec { name: "1", ..LEVEL },
            ParamSpec { name: "2", ..LEVEL },
            ParamSpec { name: "3", ..LEVEL },
            ParamSpec { name: "4", ..LEVEL },
            ParamSpec { name: "5", ..LEVEL },
            ParamSpec { name: "6", ..LEVEL },
        ];
        const CRUSHER: &[ParamSpec] = &[
            ParamSpec::new("bits", 1.0, 16.0, 6.0, Steps, ""),
            ParamSpec::new("rate", 1.0, 32.0, 1.0, Steps, "x"),
            ParamSpec::new("mix", 0.0, 1.0, 1.0, Linear, ""),
        ];
        const FILTER: &[ParamSpec] = &[
            ParamSpec::new("cutoff", 30.0, 16000.0, 2000.0, Exp, "Hz"),
            ParamSpec::new("reso", 0.0, 0.95, 0.3, Linear, ""),
            ParamSpec::labelled("mode", 0.0, &["low", "band", "high"]),
            ParamSpec::new("cv amt", -1.0, 1.0, 0.5, Linear, ""),
        ];
        const DELAY: &[ParamSpec] = &[
            ParamSpec::new("time", 5.0, 1500.0, 260.0, Exp, "ms"),
            ParamSpec::new("feedback", 0.0, 0.95, 0.4, Linear, ""),
            ParamSpec::new("mix", 0.0, 1.0, 0.35, Linear, ""),
        ];
        const LFO: &[ParamSpec] = &[
            ParamSpec::new("rate", 0.05, 30.0, 2.0, Exp, "Hz"),
            ParamSpec::labelled("shape", 0.0, &["sine", "square", "s&h"]),
            ParamSpec::new("depth", 0.0, 1.0, 1.0, Linear, ""),
        ];
        const MASTER: &[ParamSpec] = &[ParamSpec::new("volume", 0.0, 1.5, 1.0, Linear, "")];
        // Short fades on cuts, steals and retriggers. Off gives hard edges,
        // clicks and all.
        const TRACK: &[ParamSpec] = &[
            ParamSpec::labelled("declick", 1.0, &["off", "on"]),
            ParamSpec::new("volume", 0.0, 1.5, 1.0, Linear, ""),
        ];
        match self {
            Kind::Track(_) => TRACK,
            Kind::Crusher => CRUSHER,
            Kind::Filter => FILTER,
            Kind::Delay => DELAY,
            Kind::Lfo => LFO,
            Kind::Mixer => MIXER,
            Kind::Master => MASTER,
        }
    }
}

pub type DeviceId = u32;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,
    pub kind: Kind,
    pub params: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Jack {
    pub device: DeviceId,
    pub port: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cable {
    pub from: Jack,
    pub to: Jack,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rack {
    pub devices: Vec<Device>,
    pub cables: Vec<Cable>,
    next_id: DeviceId,
    /// Bumped on every change to devices or cables (not to param values), so
    /// the UI knows when the engine needs a new graph. Not saved.
    #[serde(skip)]
    pub version: u64,
}

impl Rack {
    /// One track device per track, all into a mixer, into the master.
    pub fn simple(tracks: usize) -> Rack {
        let mut rack = Rack::default();
        let master = rack.add(Kind::Master);
        let mixer = rack.add(Kind::Mixer);
        rack.connect(
            Jack {
                device: mixer,
                port: 0,
            },
            Jack {
                device: master,
                port: 0,
            },
        )
        .unwrap();
        for t in 0..tracks {
            rack.add_track(t);
        }
        rack
    }

    pub fn add(&mut self, kind: Kind) -> DeviceId {
        let id = self.next_id;
        self.next_id += 1;
        let params = kind.params().iter().map(|p| p.default).collect();
        self.devices.push(Device { id, kind, params });
        self.version += 1;
        id
    }

    /// Add the device for a new track and plug it into a free mixer
    /// channel, adding a mixer if they're all taken.
    pub fn add_track(&mut self, track: usize) -> DeviceId {
        let id = self.add(Kind::Track(track));
        if let Some(free) = self.free_mixer_input().or_else(|| self.add_mixer()) {
            let _ = self.connect(
                Jack {
                    device: id,
                    port: 0,
                },
                free,
            );
        }
        id
    }

    /// A new mixer in front of the master, taking what went into the
    /// master as its first channel. Returns its first free channel.
    fn add_mixer(&mut self) -> Option<Jack> {
        let master = self.devices.iter().find(|d| d.kind == Kind::Master)?.id;
        let into = Jack {
            device: master,
            port: 0,
        };
        let old = self.cable_into(into).map(|c| c.from);
        let mixer = self.add(Kind::Mixer);
        let channel = |port| Jack {
            device: mixer,
            port,
        };
        self.connect(channel(0), into).ok()?;
        match old {
            Some(old) => {
                self.connect(old, channel(0)).ok()?;
                Some(channel(1))
            }
            None => Some(channel(0)),
        }
    }

    fn free_mixer_input(&self) -> Option<Jack> {
        self.devices
            .iter()
            .filter(|d| d.kind == Kind::Mixer)
            .find_map(|d| {
                (0..MIXER_CHANNELS)
                    .map(|port| Jack { device: d.id, port })
                    .find(|&j| self.cable_into(j).is_none())
            })
    }

    pub fn remove(&mut self, id: DeviceId) -> bool {
        let Some(i) = self
            .devices
            .iter()
            .position(|d| d.id == id && d.kind.removable())
        else {
            return false;
        };
        self.devices.remove(i);
        self.cables
            .retain(|c| c.from.device != id && c.to.device != id);
        self.version += 1;
        true
    }

    pub fn device(&self, id: DeviceId) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn track_device(&self, track: usize) -> Option<&Device> {
        self.devices.iter().find(|d| d.kind == Kind::Track(track))
    }

    /// Set a param value, clamped to its range. Returns the value set.
    pub fn set_param(&mut self, id: DeviceId, param: usize, value: f32) -> Option<f32> {
        let d = self.devices.iter_mut().find(|d| d.id == id)?;
        let v = d.kind.params().get(param)?.clamp(value);
        d.params[param] = v;
        Some(v)
    }

    /// Same devices, params and cables, ignoring `version` and cable order.
    pub fn same_patch(&self, other: &Rack) -> bool {
        let sorted = |r: &Rack| {
            let mut c: Vec<_> = r
                .cables
                .iter()
                .map(|c| (c.to.device, c.to.port, c.from.device, c.from.port))
                .collect();
            c.sort();
            c
        };
        self.devices == other.devices && sorted(self) == sorted(other)
    }

    pub fn cable_into(&self, to: Jack) -> Option<&Cable> {
        self.cables.iter().find(|c| c.to == to)
    }

    /// Plug `from` (an output) into `to` (an input), replacing whatever was in
    /// `to`. Refuses cables that would make a loop.
    pub fn connect(&mut self, from: Jack, to: Jack) -> Result<(), &'static str> {
        let out = self.device(from.device).ok_or("no such device")?;
        let inp = self.device(to.device).ok_or("no such device")?;
        if from.port >= out.kind.outputs().len() || to.port >= inp.kind.inputs().len() {
            return Err("no such jack");
        }
        if from.device == to.device || self.reaches(to.device, from.device) {
            return Err("that would make a loop");
        }
        self.cables.retain(|c| c.to != to);
        self.cables.push(Cable { from, to });
        self.version += 1;
        Ok(())
    }

    pub fn disconnect(&mut self, to: Jack) -> Option<Cable> {
        let i = self.cables.iter().position(|c| c.to == to)?;
        self.version += 1;
        Some(self.cables.remove(i))
    }

    /// Does signal flow from device `a` to device `b`?
    fn reaches(&self, a: DeviceId, b: DeviceId) -> bool {
        let mut stack = vec![a];
        let mut seen = vec![a];
        while let Some(d) = stack.pop() {
            if d == b {
                return true;
            }
            for c in self.cables.iter().filter(|c| c.from.device == d) {
                if !seen.contains(&c.to.device) {
                    seen.push(c.to.device);
                    stack.push(c.to.device);
                }
            }
        }
        false
    }

    /// Devices in processing order: every device after the ones feeding it.
    pub fn order(&self) -> Vec<DeviceId> {
        let mut pending: Vec<DeviceId> = self.devices.iter().map(|d| d.id).collect();
        let mut done = Vec::with_capacity(pending.len());
        while !pending.is_empty() {
            let before = pending.len();
            pending.retain(|&id| {
                let ready = self
                    .cables
                    .iter()
                    .filter(|c| c.to.device == id)
                    .all(|c| done.contains(&c.from.device));
                if ready {
                    done.push(id);
                }
                !ready
            });
            if pending.len() == before {
                // A loop, which `connect` prevents; leave those devices out.
                break;
            }
        }
        done
    }

    /// Fix up a rack read from a file: drop cables to devices or jacks that
    /// don't exist, keep inputs to one cable, and make sure new device ids
    /// can't collide with existing ones. Params missing from older files get
    /// their defaults.
    pub fn repair(&mut self) {
        let valid = |r: &Rack, c: &Cable| {
            let (Some(a), Some(b)) = (r.device(c.from.device), r.device(c.to.device)) else {
                return false;
            };
            c.from.port < a.kind.outputs().len() && c.to.port < b.kind.inputs().len()
        };
        let cables = std::mem::take(&mut self.cables);
        for c in cables {
            if valid(self, &c) && self.cable_into(c.to).is_none() {
                self.cables.push(c);
            }
        }
        for d in &mut self.devices {
            let specs = d.kind.params();
            let have = d.params.len().min(specs.len());
            d.params.truncate(have);
            d.params.extend(specs[have..].iter().map(|p| p.default));
            for (v, spec) in d.params.iter_mut().zip(specs) {
                *v = spec.clamp(*v);
            }
        }
        self.next_id = self
            .devices
            .iter()
            .map(|d| d.id + 1)
            .max()
            .unwrap_or(0)
            .max(self.next_id);
        self.version += 1;
    }

    /// Make room for a new track `track`, shifting the ones from there on
    /// down, and add its device.
    pub fn insert_track(&mut self, track: usize) -> DeviceId {
        for d in &mut self.devices {
            if let Kind::Track(t) = &mut d.kind
                && *t >= track
            {
                *t += 1;
            }
        }
        self.add_track(track)
    }

    /// Remove track `track`'s device and its cables, shifting the tracks
    /// after it up.
    pub fn remove_track(&mut self, track: usize) {
        if let Some(id) = self.track_device(track).map(|d| d.id) {
            self.devices.retain(|d| d.id != id);
            self.cables
                .retain(|c| c.from.device != id && c.to.device != id);
        }
        for d in &mut self.devices {
            if let Kind::Track(t) = &mut d.kind
                && *t > track
            {
                *t -= 1;
            }
        }
        self.version += 1;
    }

    /// A track's effects chain: the audio effects its sound goes through
    /// on the way to a mixer or the master, in order, following the first
    /// cable out of each audio output.
    pub fn chain(&self, track: usize) -> Vec<DeviceId> {
        let mut chain = Vec::new();
        let Some(mut at) = self.track_device(track).map(|d| d.id) else {
            return chain;
        };
        while let Some(next) = self
            .cable_from(Jack {
                device: at,
                port: 0,
            })
            .map(|c| c.to)
        {
            let Some(d) = self.device(next.device) else {
                break;
            };
            if next.port != 0 || !d.kind.is_effect() || chain.contains(&d.id) {
                break;
            }
            chain.push(d.id);
            at = d.id;
        }
        chain
    }

    /// The first cable out of an output.
    pub fn cable_from(&self, from: Jack) -> Option<&Cable> {
        self.cables.iter().find(|c| c.from == from)
    }

    /// Add an effect at the end of a track's chain: between its last
    /// device and wherever that went.
    pub fn append_effect(&mut self, track: usize, kind: Kind) -> Option<DeviceId> {
        let last = match self.chain(track).last() {
            Some(&id) => id,
            None => self.track_device(track)?.id,
        };
        let out = Jack {
            device: last,
            port: 0,
        };
        let next = self.cable_from(out).map(|c| c.to);
        let id = self.add(kind);
        let jack = |device| Jack { device, port: 0 };
        self.connect(out, jack(id)).ok()?;
        if let Some(next) = next {
            self.connect(jack(id), next).ok()?;
        }
        Some(id)
    }

    /// Remove an effect, joining what went into it to where it went.
    pub fn splice_out(&mut self, id: DeviceId) -> bool {
        let jack = |port| Jack { device: id, port };
        let from = self.cable_into(jack(0)).map(|c| c.from);
        let to = self.cable_from(jack(0)).map(|c| c.to);
        if !self.remove(id) {
            return false;
        }
        if let (Some(from), Some(to)) = (from, to) {
            let _ = self.connect(from, to);
        }
        true
    }

    /// Track devices whose track no longer exists are removed.
    pub fn sync_tracks(&mut self, tracks: usize) {
        for t in 0..tracks.min(MAX_TRACKS) {
            if self.track_device(t).is_none() {
                self.add_track(t);
            }
        }
        let stale: Vec<_> = self
            .devices
            .iter()
            .filter(|d| matches!(d.kind, Kind::Track(t) if t >= tracks))
            .map(|d| d.id)
            .collect();
        for id in stale {
            self.devices.retain(|d| d.id != id);
            self.cables
                .retain(|c| c.from.device != id && c.to.device != id);
            self.version += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jack(device: DeviceId, port: usize) -> Jack {
        Jack { device, port }
    }

    #[test]
    fn simple_rack_routes_tracks_to_master() {
        let rack = Rack::simple(2);
        let mixer = rack
            .devices
            .iter()
            .find(|d| d.kind == Kind::Mixer)
            .unwrap()
            .id;
        for t in 0..2 {
            let track = rack.track_device(t).unwrap().id;
            assert_eq!(
                rack.cable_into(jack(mixer, t)).unwrap().from,
                jack(track, 0)
            );
        }
        let order = rack.order();
        let pos = |id| order.iter().position(|&d| d == id).unwrap();
        let master = rack
            .devices
            .iter()
            .find(|d| d.kind == Kind::Master)
            .unwrap()
            .id;
        assert!(pos(rack.track_device(1).unwrap().id) < pos(mixer));
        assert!(pos(mixer) < pos(master));
    }

    #[test]
    fn loops_are_refused() {
        let mut rack = Rack::default();
        let a = rack.add(Kind::Filter);
        let b = rack.add(Kind::Delay);
        rack.connect(jack(a, 0), jack(b, 0)).unwrap();
        assert!(rack.connect(jack(b, 0), jack(a, 0)).is_err());
        assert!(rack.connect(jack(a, 0), jack(a, 1)).is_err());
        assert_eq!(rack.cables.len(), 1);
    }

    #[test]
    fn input_takes_one_cable() {
        let mut rack = Rack::default();
        let a = rack.add(Kind::Lfo);
        let b = rack.add(Kind::Lfo);
        let f = rack.add(Kind::Filter);
        rack.connect(jack(a, 0), jack(f, 1)).unwrap();
        rack.connect(jack(b, 0), jack(f, 1)).unwrap();
        assert_eq!(
            rack.cables,
            vec![Cable {
                from: jack(b, 0),
                to: jack(f, 1)
            }]
        );
    }

    #[test]
    fn removing_a_device_removes_its_cables() {
        let mut rack = Rack::simple(1);
        let mixer = rack
            .devices
            .iter()
            .find(|d| d.kind == Kind::Mixer)
            .unwrap()
            .id;
        assert!(rack.remove(mixer));
        assert!(rack.cables.is_empty());
        let master = rack
            .devices
            .iter()
            .find(|d| d.kind == Kind::Master)
            .unwrap()
            .id;
        assert!(!rack.remove(master), "master stays");
    }

    #[test]
    fn exp_knob_round_trips() {
        let p = &Kind::Filter.params()[0];
        for v in [30.0, 440.0, 2000.0, 16000.0] {
            assert!((p.from_knob(p.to_knob(v)) - v).abs() / v < 1e-4);
        }
        assert_eq!(Kind::Filter.params()[2].format(1.0), "band");
    }

    #[test]
    fn repair_fills_missing_params_with_defaults() {
        let mut rack = Rack::simple(1);
        let t = rack.track_device(0).unwrap().id;
        let f = rack.add(Kind::Filter);
        // As saved before tracks had params, and with a param dropped.
        rack.devices.iter_mut().find(|d| d.id == t).unwrap().params = vec![];
        rack.devices.iter_mut().find(|d| d.id == f).unwrap().params = vec![500.0];
        rack.repair();
        assert_eq!(
            rack.device(t).unwrap().params,
            vec![1.0, 1.0],
            "declick on, full volume"
        );
        let defaults: Vec<f32> = Kind::Filter.params().iter().map(|p| p.default).collect();
        assert_eq!(rack.device(f).unwrap().params[0], 500.0);
        assert_eq!(rack.device(f).unwrap().params[1..], defaults[1..]);
    }

    #[test]
    fn tracks_can_be_inserted_and_removed_in_the_middle() {
        let mut rack = Rack::simple(3);
        let ids: Vec<DeviceId> = (0..3).map(|t| rack.track_device(t).unwrap().id).collect();
        let new = rack.insert_track(1);
        assert_eq!(rack.track_device(1).unwrap().id, new);
        assert_eq!(rack.track_device(2).unwrap().id, ids[1], "shifted down");
        assert_eq!(rack.track_device(3).unwrap().id, ids[2]);
        assert!(
            rack.cables.iter().any(|c| c.from.device == new),
            "plugged into the mixer"
        );
        rack.remove_track(0);
        assert!(rack.device(ids[0]).is_none());
        assert!(rack.cables.iter().all(|c| c.from.device != ids[0]));
        assert_eq!(rack.track_device(0).unwrap().id, new, "shifted up");
        assert_eq!(rack.track_device(2).unwrap().id, ids[2]);
        assert!(rack.track_device(3).is_none());
    }

    #[test]
    fn effects_chain_follows_the_cables() {
        let mut rack = Rack::simple(2);
        assert_eq!(rack.chain(0), vec![], "straight into the mixer");
        let f = rack.append_effect(0, Kind::Filter).unwrap();
        let d = rack.append_effect(0, Kind::Delay).unwrap();
        assert_eq!(rack.chain(0), vec![f, d]);
        assert_eq!(rack.chain(1), vec![], "the other track is untouched");
        let mixer = rack
            .devices
            .iter()
            .find(|x| x.kind == Kind::Mixer)
            .unwrap()
            .id;
        assert_eq!(
            rack.cable_from(jack(d, 0)).unwrap().to,
            jack(mixer, 0),
            "still reaches the mixer"
        );
        assert!(rack.splice_out(f));
        assert_eq!(rack.chain(0), vec![d]);
        let t = rack.track_device(0).unwrap().id;
        assert_eq!(
            rack.cable_from(jack(t, 0)).unwrap().to,
            jack(d, 0),
            "rejoined"
        );
        // A cable into a CV input isn't part of the chain.
        let lfo = rack.add(Kind::Lfo);
        rack.connect(jack(lfo, 0), jack(d, 0)).ok();
        assert!(rack.chain(0).len() <= 1);
    }

    #[test]
    fn a_full_mixer_gets_another_in_front_of_it() {
        let mut rack = Rack::simple(MIXER_CHANNELS);
        let master = rack
            .devices
            .iter()
            .find(|d| d.kind == Kind::Master)
            .unwrap()
            .id;
        for t in MIXER_CHANNELS..MIXER_CHANNELS + 3 {
            rack.add_track(t);
        }
        let mixers = rack
            .devices
            .iter()
            .filter(|d| d.kind == Kind::Mixer)
            .count();
        assert_eq!(mixers, 2);
        for t in 0..MIXER_CHANNELS + 3 {
            let id = rack.track_device(t).unwrap().id;
            assert!(rack.reaches(id, master), "track {t} is heard");
        }
    }

    #[test]
    fn sync_tracks_adds_and_removes() {
        let mut rack = Rack::simple(1);
        rack.sync_tracks(3);
        assert!(rack.track_device(2).is_some());
        rack.sync_tracks(1);
        assert!(rack.track_device(1).is_none());
    }
}
