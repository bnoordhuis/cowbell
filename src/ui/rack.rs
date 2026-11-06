//! The rack view: one panel per device, with knobs on the left and jacks on
//! the right, and cables hanging between the jacks.
//!
//! Drag from a jack to another jack to patch. Drag a cable's plug out of an
//! input to move it, or drop it anywhere else to unplug it. Drag knobs up and
//! down (hold shift for fine control); double-click a knob to reset it.

use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2,
};
use eframe::epaint::CubicBezierShape;

use super::color;
use crate::rack::{DeviceId, Jack, Kind, Rack, Signal};

const PANEL_HEIGHT: f32 = 76.0;
/// Jack centers, from the top of the panel. Labels go above them, because
/// cables hang down.
const JACK_Y: f32 = 44.0;
const PANEL_GAP: f32 = 6.0;
const MARGIN: f32 = 8.0;
const ADD_BAR: f32 = 30.0;
const KNOB_CELL: f32 = 66.0;
const JACK_RADIUS: f32 = 7.0;
const JACK_CELL: f32 = 46.0;
/// How close the pointer must be to a jack to grab or drop on it.
const JACK_HIT: f32 = 13.0;

const DEVICE_BG: Color32 = Color32::from_rgb(0x26, 0x27, 0x2b);
const DEVICE_EDGE: Color32 = Color32::from_rgb(0x3a, 0x3b, 0x40);
const JACK_HOLE: Color32 = Color32::from_rgb(0x0e, 0x0e, 0x10);
const AUDIO_CABLES: [Color32; 5] = [
    Color32::from_rgb(0xd8, 0x5a, 0x30),
    Color32::from_rgb(0x1d, 0x9e, 0x75),
    Color32::from_rgb(0x7f, 0x77, 0xdd),
    Color32::from_rgb(0x37, 0x8a, 0xdd),
    Color32::from_rgb(0xd4, 0x53, 0x7e),
];
const CV_CABLE: Color32 = Color32::from_rgb(0xef, 0x9f, 0x27);

enum Drag {
    /// A cable end in hand. `anchor` is the plugged-in end.
    Cable { anchor: Jack, anchor_is_input: bool },
    Knob {
        device: DeviceId,
        param: usize,
        start_y: f32,
        start_knob: f32,
    },
}

#[derive(Default)]
pub struct RackView {
    drag: Option<Drag>,
    /// The rack as it was when the current mouse gesture started.
    before: Option<Rack>,
    scroll: f32,
    /// Shown in the status bar: why the last patch didn't work.
    pub message: Option<String>,
}

/// Where everything is, worked out before drawing so the same geometry is
/// used for hit testing.
struct Layout {
    panels: Vec<(DeviceId, Rect)>,
    jacks: Vec<(Jack, bool, Signal, Pos2)>,
    knobs: Vec<(DeviceId, usize, Pos2)>,
    removes: Vec<(DeviceId, Rect)>,
    adds: Vec<(Kind, Rect)>,
}

impl Layout {
    fn jack_at(&self, p: Pos2) -> Option<(Jack, bool)> {
        self.jacks
            .iter()
            .find(|(.., c)| c.distance(p) <= JACK_HIT)
            .map(|&(j, input, ..)| (j, input))
    }

    fn jack_pos(&self, jack: Jack, input: bool) -> Option<Pos2> {
        self.jacks
            .iter()
            .find(|(j, i, ..)| *j == jack && *i == input)
            .map(|&(.., p)| p)
    }

    fn knob_at(&self, p: Pos2) -> Option<(DeviceId, usize)> {
        self.knobs
            .iter()
            .find(|(.., c)| c.distance(p) <= super::knob::HIT)
            .map(|&(d, i, _)| (d, i))
    }
}

/// Tracks first, then effects in the order they were added, master last.
fn display_order(rack: &Rack) -> Vec<DeviceId> {
    let mut tracks: Vec<_> = rack
        .devices
        .iter()
        .filter_map(|d| match d.kind {
            Kind::Track(t) => Some((t, d.id)),
            _ => None,
        })
        .collect();
    tracks.sort();
    let mut order: Vec<_> = tracks.into_iter().map(|(_, id)| id).collect();
    order.extend(
        rack.devices
            .iter()
            .filter(|d| d.kind.removable())
            .map(|d| d.id),
    );
    order.extend(
        rack.devices
            .iter()
            .filter(|d| d.kind == Kind::Master)
            .map(|d| d.id),
    );
    order
}

fn layout(rack: &Rack, area: Rect, scroll: f32, font: &FontId, ui: &egui::Ui) -> Layout {
    let mut adds = Vec::new();
    let mut x = area.left() + MARGIN;
    for kind in Kind::ADDABLE {
        let w = ui.fonts_mut(|f| f.glyph_width(font, '0')) * (kind.name().len() + 2) as f32 + 16.0;
        adds.push((
            kind,
            Rect::from_min_size(pos2(x, area.top() + 4.0), vec2(w, ADD_BAR - 8.0)),
        ));
        x += w + 6.0;
    }

    let mut l = Layout {
        panels: Vec::new(),
        jacks: Vec::new(),
        knobs: Vec::new(),
        removes: Vec::new(),
        adds,
    };
    let top = area.top() + ADD_BAR - scroll;
    for (i, id) in display_order(rack).into_iter().enumerate() {
        let d = rack.device(id).unwrap();
        let y = top + i as f32 * (PANEL_HEIGHT + PANEL_GAP);
        let panel = Rect::from_min_size(
            pos2(area.left() + MARGIN, y),
            vec2(area.width() - 2.0 * MARGIN, PANEL_HEIGHT),
        );
        l.panels.push((id, panel));
        if d.kind.removable() {
            l.removes.push((
                id,
                Rect::from_min_size(panel.left_bottom() + vec2(10.0, -22.0), vec2(56.0, 16.0)),
            ));
        }
        for p in 0..d.kind.params().len() {
            let c = pos2(
                panel.left() + 150.0 + (p as f32 + 0.5) * KNOB_CELL,
                panel.top() + 28.0,
            );
            l.knobs.push((id, p, c));
        }
        // Jacks start after the knobs, inputs then outputs. Different
        // devices end up with jacks at different x, so cables between them
        // don't all run down the same line.
        let (ins, outs) = (d.kind.inputs(), d.kind.outputs());
        let n = d.kind.params().len();
        let knobs_end = if n == 0 {
            190.0
        } else {
            150.0 + n as f32 * KNOB_CELL + 50.0
        };
        let x0 = panel.left() + knobs_end;
        let y = panel.top() + JACK_Y;
        for (port, spec) in ins.iter().enumerate() {
            let x = x0 + (port as f32 + 0.5) * JACK_CELL;
            l.jacks
                .push((Jack { device: id, port }, true, spec.signal, pos2(x, y)));
        }
        let outs_x0 = x0 + ins.len() as f32 * JACK_CELL + if ins.is_empty() { 0.0 } else { 28.0 };
        for (port, spec) in outs.iter().enumerate() {
            let x = outs_x0 + (port as f32 + 0.5) * JACK_CELL;
            l.jacks
                .push((Jack { device: id, port }, false, spec.signal, pos2(x, y)));
        }
    }
    l
}

fn cable_shape(a: Pos2, b: Pos2, color: Color32, width: f32) -> CubicBezierShape {
    // Cables hang: both control points pulled down, more for longer cables.
    // A cable between jacks that are nearly above each other also gets some
    // sideways slack, or it would be drawn as a straight stick.
    let sag = 24.0 + a.distance(b) * 0.3;
    let dx = b.x - a.x;
    let slack = if dx.abs() < 80.0 {
        (80.0 - dx.abs()) * 0.6
    } else {
        0.0
    };
    let points = [a, a + vec2(slack, sag), b + vec2(slack, sag), b];
    CubicBezierShape::from_points_stroke(
        points,
        false,
        Color32::TRANSPARENT,
        Stroke::new(width, color),
    )
}

fn draw_cable(p: &egui::Painter, a: Pos2, b: Pos2, color: Color32) {
    // Slightly see-through, so jacks and labels under a cable stay legible.
    let color = color.gamma_multiply(0.85);
    p.add(cable_shape(a, b, Color32::from_black_alpha(110), 6.0));
    p.add(cable_shape(a, b, color, 3.5));
    for end in [a, b] {
        p.circle_filled(end, 4.5, color);
    }
}

/// What happened in the rack view this frame.
#[derive(Default)]
pub struct Output {
    /// Knob changes, for the engine. Structural changes bump `rack.version`
    /// instead, which the caller watches.
    pub params: Vec<(DeviceId, usize, f32)>,
    /// Set when a gesture that changed the rack has finished: the rack as it
    /// was before, for undo. A whole knob drag or cable move is one step.
    pub undo: Option<Rack>,
    /// A knob that was right-clicked: make it the cursor track's
    /// automation target (or unmake it).
    pub bind: Option<(DeviceId, usize)>,
}

impl RackView {
    /// `automated` is the knob the cursor's track automates, to mark it.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rack: &mut Rack,
        track_names: &[String],
        automated: Option<(DeviceId, usize)>,
    ) -> Output {
        let mut bind = None;
        let area = ui.max_rect();
        let font = FontId::monospace(13.0);
        let small = FontId::monospace(11.0);
        let response = ui.allocate_rect(area, Sense::click_and_drag());
        let mut changes = Vec::new();
        let mut undo = None;

        // Scrolling.
        let content = ADD_BAR + rack.devices.len() as f32 * (PANEL_HEIGHT + PANEL_GAP) + MARGIN;
        if response.hovered() {
            self.scroll -= ui.input(|i| i.smooth_scroll_delta.y);
        }
        self.scroll = self.scroll.clamp(0.0, (content - area.height()).max(0.0));

        let l = layout(rack, area, self.scroll, &font, ui);
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let shift = ui.input(|i| i.modifiers.shift);

        // Interaction.
        if response.drag_started()
            && let Some(p) = ui.input(|i| i.pointer.press_origin())
        {
            self.before = Some(rack.clone());
            if let Some((jack, is_input)) = l.jack_at(p) {
                self.drag = Some(if is_input {
                    match rack.disconnect(jack) {
                        // Pick up the plug; the other end stays put.
                        Some(c) => Drag::Cable {
                            anchor: c.from,
                            anchor_is_input: false,
                        },
                        None => Drag::Cable {
                            anchor: jack,
                            anchor_is_input: true,
                        },
                    }
                } else {
                    Drag::Cable {
                        anchor: jack,
                        anchor_is_input: false,
                    }
                });
            } else if let Some((device, param)) = l.knob_at(p) {
                let d = rack.device(device).unwrap();
                let start_knob = d.kind.params()[param].to_knob(d.params[param]);
                self.drag = Some(Drag::Knob {
                    device,
                    param,
                    start_y: p.y,
                    start_knob,
                });
            }
        }
        if response.dragged()
            && let (
                Some(Drag::Knob {
                    device,
                    param,
                    start_y,
                    start_knob,
                }),
                Some(p),
            ) = (&mut self.drag, pointer)
        {
            let scale = if shift {
                super::knob::FINE_DRAG_RANGE
            } else {
                super::knob::DRAG_RANGE
            };
            let d = rack.device(*device).unwrap();
            let spec = &d.kind.params()[*param];
            let v = spec.from_knob(*start_knob + (*start_y - p.y) / scale);
            if v != d.params[*param]
                && let Some(v) = rack.set_param(*device, *param, v)
            {
                changes.push((*device, *param, v));
            }
            if shift {
                // Re-anchor so switching to fine control doesn't jump.
                *start_y = p.y;
                *start_knob = spec.to_knob(v);
            }
        }
        if response.drag_stopped() {
            if let (
                Some(Drag::Cable {
                    anchor,
                    anchor_is_input,
                }),
                Some(p),
            ) = (self.drag.take(), pointer)
                && let Some((jack, is_input)) = l.jack_at(p)
            {
                let result = match (anchor_is_input, is_input) {
                    (false, true) => rack.connect(anchor, jack),
                    (true, false) => rack.connect(jack, anchor),
                    _ => Err("connect an output to an input"),
                };
                self.message = result.err().map(str::to_owned);
            }
            self.drag = None;
            undo = self.before.take().filter(|b| !b.same_patch(rack));
        }
        if response.secondary_clicked()
            && let Some(p) = pointer
        {
            bind = l.knob_at(p);
        }
        if response.double_clicked()
            && let Some(p) = pointer
            && let Some((device, param)) = l.knob_at(p)
        {
            let default = rack.device(device).unwrap().kind.params()[param].default;
            let before = rack.clone();
            if let Some(v) = rack.set_param(device, param, default) {
                changes.push((device, param, v));
            }
            undo = Some(before).filter(|b| !b.same_patch(rack));
        }
        if response.clicked()
            && let Some(p) = pointer
        {
            let before = rack.clone();
            if let Some(&(kind, _)) = l.adds.iter().find(|(_, r)| r.contains(p)) {
                rack.add(kind);
                self.scroll = f32::MAX; // show the new device at the bottom
            } else if let Some(&(id, _)) = l.removes.iter().find(|(_, r)| r.contains(p)) {
                rack.remove(id);
            }
            if !before.same_patch(rack) {
                undo = Some(before);
            }
        }

        // Drawing. Rebuild the layout if the rack changed above.
        let l = layout(rack, area, self.scroll.min(content), &font, ui);
        let painter = ui.painter_at(area);
        let p = &painter;
        for &(kind, r) in &l.adds {
            p.rect_stroke(r, 4.0, Stroke::new(1.0, DEVICE_EDGE), StrokeKind::Inside);
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                format!("+ {}", kind.name()),
                font.clone(),
                color::MUTED,
            );
        }
        let panels = Rect::from_x_y_ranges(area.x_range(), (area.top() + ADD_BAR)..=area.bottom());
        let p = &painter.with_clip_rect(panels);
        for &(id, panel) in &l.panels {
            let d = rack.device(id).unwrap();
            p.rect_filled(panel, 4.0, DEVICE_BG);
            p.rect_stroke(
                panel,
                4.0,
                Stroke::new(1.0, DEVICE_EDGE),
                StrokeKind::Inside,
            );
            let (title, tint) = match d.kind {
                Kind::Track(t) => {
                    let name = track_names
                        .get(t)
                        .cloned()
                        .unwrap_or_else(|| format!("t{}", t + 1));
                    (name, color::TRACKS[t % color::TRACKS.len()])
                }
                Kind::Master => ("master".into(), color::TEXT),
                k => (k.name().into(), color::ACCENT),
            };
            p.text(
                panel.left_top() + vec2(10.0, 8.0),
                Align2::LEFT_TOP,
                title,
                font.clone(),
                tint,
            );
            let kind = if matches!(d.kind, Kind::Track(_)) {
                "track"
            } else {
                ""
            };
            p.text(
                panel.left_top() + vec2(10.0, 26.0),
                Align2::LEFT_TOP,
                kind,
                small.clone(),
                color::DIM,
            );
        }
        for &(_, r) in &l.removes {
            p.text(
                r.left_center(),
                Align2::LEFT_CENTER,
                "remove",
                small.clone(),
                color::DIM,
            );
        }
        for &(id, param, c) in &l.knobs {
            let d = rack.device(id).unwrap();
            let marked = automated == Some((id, param));
            super::knob::draw(
                p,
                c,
                &d.kind.params()[param],
                d.params[param],
                &small,
                marked,
            );
        }
        for &(jack, is_input, signal, c) in &l.jacks {
            let d = rack.device(jack.device).unwrap();
            let specs = if is_input {
                d.kind.inputs()
            } else {
                d.kind.outputs()
            };
            let ring = if signal == Signal::Cv {
                color::ACCENT
            } else {
                color::MUTED
            };
            p.circle_filled(c, JACK_RADIUS, JACK_HOLE);
            p.circle_stroke(c, JACK_RADIUS, Stroke::new(1.5, ring));
            let label = if is_input {
                specs[jack.port].name.to_owned()
            } else {
                format!("{} ›", specs[jack.port].name)
            };
            p.text(
                c - vec2(0.0, JACK_RADIUS + 4.0),
                Align2::CENTER_BOTTOM,
                label,
                small.clone(),
                color::MUTED,
            );
        }
        // Cables, on top of everything.
        for (i, c) in rack.cables.iter().enumerate() {
            let (Some(a), Some(b)) = (l.jack_pos(c.from, false), l.jack_pos(c.to, true)) else {
                continue;
            };
            let signal = rack
                .device(c.from.device)
                .map(|d| d.kind.outputs()[c.from.port].signal);
            let color = if signal == Some(Signal::Cv) {
                CV_CABLE
            } else {
                AUDIO_CABLES[i % AUDIO_CABLES.len()]
            };
            draw_cable(p, a, b, color);
        }
        if let (
            Some(Drag::Cable {
                anchor,
                anchor_is_input,
            }),
            Some(ptr),
        ) = (&self.drag, pointer)
            && let Some(a) = l.jack_pos(*anchor, *anchor_is_input)
        {
            draw_cable(p, a, ptr, color::TEXT);
        }
        Output {
            params: changes,
            undo,
            bind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, Modifiers, PointerButton, RawInput};

    struct Harness {
        ctx: egui::Context,
        view: RackView,
        rack: Rack,
        changes: Vec<(DeviceId, usize, f32)>,
        undos: usize,
    }

    impl Harness {
        fn new(rack: Rack) -> Self {
            let mut h = Harness {
                ctx: egui::Context::default(),
                view: RackView::default(),
                rack,
                changes: vec![],
                undos: 0,
            };
            h.frame(vec![]);
            h
        }

        fn frame(&mut self, events: Vec<Event>) {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1400.0, 1000.0))),
                events,
                ..Default::default()
            };
            let (view, rack, changes) = (&mut self.view, &mut self.rack, &mut self.changes);
            let mut undos = 0;
            let mut full = self.ctx.run_ui(input, |ui| {
                let out = view.show(ui, rack, &[], None);
                changes.extend(out.params);
                undos += out.undo.is_some() as usize;
            });
            self.undos += undos;
            // No renderer here to apply the font textures.
            full.textures_delta.clear();
        }

        fn layout(&mut self) -> Layout {
            let mut out = None;
            let rack = self.rack.clone();
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1400.0, 1000.0))),
                ..Default::default()
            };
            let mut full = self.ctx.run_ui(input, |ui| {
                out = Some(layout(
                    &rack,
                    ui.max_rect(),
                    0.0,
                    &FontId::monospace(13.0),
                    ui,
                ));
            });
            full.textures_delta.clear();
            out.unwrap()
        }

        fn drag(&mut self, from: Pos2, to: Pos2) {
            self.drag_via(from, from + (to - from) * 0.5, to);
        }

        fn drag_via(&mut self, from: Pos2, via: Pos2, to: Pos2) {
            let button = |pos, pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            };
            self.frame(vec![Event::PointerMoved(from)]);
            self.frame(vec![button(from, true)]);
            for (a, b) in [(from, via), (via, to)] {
                for i in 1..=3 {
                    self.frame(vec![Event::PointerMoved(a + (b - a) * (i as f32 / 3.0))]);
                }
            }
            self.frame(vec![button(to, false)]);
            self.frame(vec![]);
        }
    }

    fn rack_with_filter() -> (Rack, DeviceId) {
        let mut rack = Rack::simple(1);
        let f = rack.add(Kind::Filter);
        (rack, f)
    }

    fn jack(device: DeviceId, port: usize) -> Jack {
        Jack { device, port }
    }

    fn mixer(rack: &Rack) -> DeviceId {
        rack.devices
            .iter()
            .find(|d| d.kind == Kind::Mixer)
            .unwrap()
            .id
    }

    #[test]
    fn drag_output_to_input_patches() {
        let (rack, f) = rack_with_filter();
        let mut h = Harness::new(rack);
        let track = h.rack.track_device(0).unwrap().id;
        let l = h.layout();
        let a = l.jack_pos(jack(track, 0), false).unwrap();
        let b = l.jack_pos(jack(f, 0), true).unwrap();
        h.drag(a, b);
        let c = h.rack.cable_into(jack(f, 0)).expect("patched");
        assert_eq!(c.from, jack(track, 0));
        assert_eq!(
            h.rack.cables.len(),
            3,
            "the track still feeds the mixer too"
        );
        assert_eq!(h.undos, 1);
    }

    #[test]
    fn dragging_a_plug_away_unplugs_it() {
        let mut h = Harness::new(Rack::simple(1));
        let into = jack(mixer(&h.rack), 0);
        assert!(h.rack.cable_into(into).is_some());
        let a = h.layout().jack_pos(into, true).unwrap();
        h.drag(a, a + vec2(0.0, 300.0));
        assert!(h.rack.cable_into(into).is_none());
        assert_eq!(h.undos, 1);
    }

    #[test]
    fn replugging_the_same_jack_is_not_an_undo_step() {
        let mut h = Harness::new(Rack::simple(1));
        let into = jack(mixer(&h.rack), 0);
        let a = h.layout().jack_pos(into, true).unwrap();
        // Pull the plug out and push it back into the same jack.
        h.drag_via(a, a + vec2(0.0, 80.0), a);
        assert!(h.rack.cable_into(into).is_some());
        assert_eq!(h.undos, 0);
    }

    #[test]
    fn output_to_output_is_refused() {
        let (rack, f) = rack_with_filter();
        let mut h = Harness::new(rack);
        let track = h.rack.track_device(0).unwrap().id;
        let l = h.layout();
        let a = l.jack_pos(jack(track, 0), false).unwrap();
        let b = l.jack_pos(jack(f, 0), false).unwrap();
        let before = h.rack.cables.clone();
        h.drag(a, b);
        assert_eq!(h.rack.cables, before);
        assert!(h.view.message.is_some());
        assert_eq!(h.undos, 0);
    }

    #[test]
    fn dragging_a_knob_up_turns_it_up() {
        let (rack, f) = rack_with_filter();
        let mut h = Harness::new(rack);
        let c = h
            .layout()
            .knobs
            .iter()
            .find(|k| k.0 == f && k.1 == 0)
            .unwrap()
            .2;
        let before = h.rack.device(f).unwrap().params[0];
        h.drag(c, c - vec2(0.0, 60.0));
        let after = h.rack.device(f).unwrap().params[0];
        assert!(after > before * 1.5, "{before} -> {after}");
        assert_eq!(h.changes.last().copied(), Some((f, 0, after)));
        assert_eq!(h.undos, 1, "a whole drag is one undo step");
    }
}
