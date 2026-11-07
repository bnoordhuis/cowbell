//! The devices panel: the selected track's sound, left to right. Its
//! instrument (a synth's knobs, or the sample it slices), its track device
//! (declick and volume), then each effect in its chain, and a card to add
//! another.
//!
//! Drag a knob up or down (shift: fine) or scroll over it; click a switch to
//! step it; double-click a knob to reset it. Scroll elsewhere to move along.
//! Like the other views it reports edits and changes nothing itself.

use eframe::egui::{self, Align2, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};

use super::edit::Edit;
use super::{color, knob};
use crate::rack::{DeviceId, Kind, ParamSpec};
use crate::song::{Instrument, Song};

const CARD_GAP: f32 = 10.0;
const TITLE_H: f32 = 30.0;
const PAD: f32 = 10.0;
/// Knob centres, below the card's top.
const KNOB_Y: f32 = TITLE_H + 44.0;
const CARD_BG: egui::Color32 = egui::Color32::from_rgb(0x26, 0x27, 0x2b);
const CARD_EDGE: egui::Color32 = egui::Color32::from_rgb(0x3a, 0x3b, 0x40);

/// The effects that can go in a chain, with what they do.
pub const EFFECTS: [(Kind, &str); 3] = [
    (Kind::Filter, "low, band or high pass"),
    (Kind::Crusher, "fewer bits, lower rate: grit"),
    (Kind::Delay, "echoes"),
];

/// A knob on the panel.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Knob {
    Synth { instrument: usize, param: usize },
    Rack { device: DeviceId, param: usize },
}

impl Knob {
    fn edit(self, value: f32) -> Edit {
        match self {
            Knob::Synth { instrument, param } => Edit::SynthParam {
                instrument,
                param,
                value,
            },
            Knob::Rack { device, param } => Edit::RackParam {
                device,
                param,
                value,
            },
        }
    }
}

struct KnobDrag {
    knob: Knob,
    start_y: f32,
    start_knob: f32,
    moved: bool,
}

#[derive(Default)]
pub struct DevicesView {
    drag: Option<KnobDrag>,
    wheel: knob::Wheel<Knob>,
    /// A run of wheel turns is one undo step; this one hasn't ended yet.
    wheeling: bool,
    /// Points scrolled along.
    scroll: f32,
}

/// What happened in the devices panel this frame.
#[derive(Default, Debug, PartialEq)]
pub struct Output {
    pub edits: Vec<Edit>,
    pub live: Option<Edit>,
    pub end_first: bool,
    pub gesture_done: bool,
}

/// A card on the panel and the knobs on it.
struct Card {
    rect: Rect,
    title: String,
    /// Shown instead of knobs.
    note: Option<String>,
    knobs: Vec<(Knob, &'static ParamSpec, f32, Pos2)>,
    /// The effect it is, if it can be taken out.
    remove: Option<(DeviceId, Rect)>,
}

impl DevicesView {
    pub fn end_wheel(&mut self) {
        self.wheeling = false;
    }

    pub fn show(&mut self, ui: &mut egui::Ui, song: &Song, track: usize) -> Output {
        let mut out = Output::default();
        let area = ui.max_rect();
        let font = FontId::monospace(11.0);
        let char_w = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
        let response = ui.allocate_rect(area, Sense::click_and_drag());
        let Some(t) = song.tracks.get(track) else {
            return out;
        };
        let (cards, add) = layout(song, t.instrument, track, area, self.scroll, char_w);
        let content = add.right() + self.scroll - area.left() + PAD;
        let knob_at = |p: Pos2| {
            cards
                .iter()
                .flat_map(|c| &c.knobs)
                .find(|k| k.3.distance(p) <= knob::HIT)
                .copied()
        };
        let (pointer, shift, down) = ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.modifiers.shift,
                i.pointer.primary_down(),
            )
        });

        // The wheel turns the knob under the pointer, or scrolls along.
        if response.contains_pointer()
            && self.drag.is_none()
            && let Some(p) = pointer
        {
            let (delta, now) = ui.input(|i| (knob::wheel_delta(i), i.time));
            if let Some((k, spec, value, _)) = knob_at(p) {
                if let Some((v, first)) = self.wheel.turn(k, spec, value, delta, shift, now) {
                    if first && std::mem::take(&mut self.wheeling) {
                        out.end_first = true;
                    }
                    self.wheeling = true;
                    out.live = Some(k.edit(v));
                }
            } else if delta != 0.0 {
                self.scroll = (self.scroll - delta).clamp(0.0, (content - area.width()).max(0.0));
            }
        }

        // Pressing on a knob starts a drag; releasing without moving steps a
        // switch.
        if ui.input(|i| i.pointer.primary_pressed())
            && response.contains_pointer()
            && let Some(p) = pointer
        {
            if std::mem::take(&mut self.wheeling) {
                out.end_first = true;
            }
            if let Some((k, spec, value, _)) = knob_at(p) {
                self.drag = Some(KnobDrag {
                    knob: k,
                    start_y: p.y,
                    start_knob: spec.to_knob(value),
                    moved: false,
                });
            } else if let Some(id) = cards
                .iter()
                .filter_map(|c| c.remove)
                .find(|(_, r)| r.contains(p))
                .map(|(id, _)| id)
            {
                out.edits.push(Edit::RemoveEffect(id));
            }
        }
        if let (Some(d), Some(p)) = (&mut self.drag, pointer) {
            let spec_value = cards
                .iter()
                .flat_map(|c| &c.knobs)
                .find(|k| k.0 == d.knob)
                .map(|k| (k.1, k.2));
            if let Some((spec, value)) = spec_value {
                if (p.y - d.start_y).abs() > 2.0 {
                    d.moved = true;
                }
                let range = if shift {
                    knob::FINE_DRAG_RANGE
                } else {
                    knob::DRAG_RANGE
                };
                let v = spec.from_knob(d.start_knob + (d.start_y - p.y) / range);
                if d.moved && spec.labels.is_empty() && v != value {
                    out.live = Some(d.knob.edit(v));
                }
                if shift {
                    // Re-anchor so switching to fine control doesn't jump.
                    d.start_y = p.y;
                    d.start_knob = spec.to_knob(v);
                }
                if !down {
                    let d = self.drag.take().unwrap();
                    if d.moved && spec.labels.is_empty() {
                        out.gesture_done = true;
                    } else if !d.moved
                        && let Some(next) = spec.next_choice(value)
                    {
                        out.edits.push(d.knob.edit(next));
                    }
                }
            } else {
                self.drag = None;
            }
        }
        if response.double_clicked()
            && let Some(p) = pointer
            && let Some((k, spec, value, _)) = knob_at(p)
            && spec.labels.is_empty()
            && value != spec.default
        {
            out.edits.push(k.edit(spec.default));
        }

        self.draw(ui, &cards, &font, area);
        self.add_effect(ui, add, track, &mut out);
        out
    }

    fn draw(&self, ui: &egui::Ui, cards: &[Card], font: &FontId, area: Rect) {
        let p = ui.painter_at(area);
        for card in cards {
            p.rect_filled(card.rect, 6.0, CARD_BG);
            p.rect_stroke(
                card.rect,
                6.0,
                Stroke::new(1.0, CARD_EDGE),
                StrokeKind::Inside,
            );
            p.text(
                card.rect.left_top() + vec2(PAD, TITLE_H / 2.0 + 2.0),
                Align2::LEFT_CENTER,
                &card.title,
                FontId::proportional(15.0),
                color::TEXT,
            );
            if let Some((_, r)) = card.remove {
                p.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    "×",
                    FontId::proportional(18.0),
                    color::MUTED,
                );
            }
            if let Some(note) = &card.note {
                p.text(
                    card.rect.left_top() + vec2(PAD, KNOB_Y),
                    Align2::LEFT_CENTER,
                    note,
                    FontId::proportional(13.0),
                    color::MUTED,
                );
            }
            for &(_, spec, value, c) in &card.knobs {
                knob::draw(&p, c, spec, value, font, false);
            }
        }
    }

    fn add_effect(&self, ui: &mut egui::Ui, rect: Rect, track: usize, out: &mut Output) {
        if !ui.max_rect().intersects(rect) {
            return;
        }
        let clip = ui.max_rect();
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.set_clip_rect(clip);
            let button = egui::Button::new(egui::RichText::new("+ Add effect").size(15.0))
                .min_size(rect.size());
            egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
                for (kind, what) in EFFECTS {
                    let label = format!("{} — {what}", kind.name());
                    if ui.button(label).clicked() {
                        out.edits.push(Edit::AddEffect(track, kind));
                    }
                }
            });
        });
    }
}

/// The cards for a track, and where the add-effect card goes after them.
fn layout(
    song: &Song,
    instrument: usize,
    track: usize,
    area: Rect,
    scroll: f32,
    char_w: f32,
) -> (Vec<Card>, Rect) {
    let top = area.top() + PAD;
    let height = (area.height() - 2.0 * PAD).max(TITLE_H + 90.0);
    let mut x = area.left() + PAD - scroll;
    let mut cards = Vec::new();
    let mut card = |title: String,
                    note: Option<String>,
                    knobs: Vec<(Knob, &'static ParamSpec, f32)>,
                    remove: Option<DeviceId>| {
        let widths: Vec<f32> = knobs.iter().map(|k| knob::width(k.1, char_w)).collect();
        let title_w = title.chars().count() as f32 * 9.0 + 2.0 * PAD + 24.0;
        let note_w = note
            .as_ref()
            .map_or(0.0, |n| n.chars().count() as f32 * 7.5 + 2.0 * PAD);
        let w = (widths.iter().sum::<f32>() + 2.0 * PAD)
            .max(title_w)
            .max(note_w)
            .max(140.0);
        let rect = Rect::from_min_size(pos2(x, top), vec2(w, height));
        let mut kx = rect.left() + PAD;
        let knobs = knobs
            .into_iter()
            .zip(widths)
            .map(|((k, spec, v), kw)| {
                let c = pos2(kx + kw / 2.0, rect.top() + KNOB_Y);
                kx += kw;
                (k, spec, v, c)
            })
            .collect();
        let remove = remove.map(|id| {
            let r = Rect::from_min_size(
                pos2(rect.right() - 30.0, rect.top() + 2.0),
                vec2(28.0, 28.0),
            );
            (id, r)
        });
        cards.push(Card {
            rect,
            title,
            note,
            knobs,
            remove,
        });
        x += w + CARD_GAP;
    };

    match song.instruments.get(instrument) {
        Some(Instrument::Synth(patch)) => {
            let knobs = patch
                .kind
                .params()
                .iter()
                .zip(&patch.params)
                .enumerate()
                .map(|(param, (spec, &v))| (Knob::Synth { instrument, param }, spec, v))
                .collect();
            card(patch.name.clone(), None, knobs, None);
        }
        Some(Instrument::Sampler(sample)) => {
            let note = format!("sample, {} slices", sample.slices.len());
            card(sample.name.clone(), Some(note), Vec::new(), None);
        }
        None => card("no instrument".into(), None, Vec::new(), None),
    }
    let rack_knobs = |id: DeviceId| {
        let d = song.rack.device(id).unwrap();
        d.kind
            .params()
            .iter()
            .zip(&d.params)
            .enumerate()
            .map(|(param, (spec, &v))| (Knob::Rack { device: id, param }, spec, v))
            .collect::<Vec<_>>()
    };
    if let Some(d) = song.rack.track_device(track) {
        card("track".into(), None, rack_knobs(d.id), None);
    }
    for id in song.rack.chain(track) {
        let name = song.rack.device(id).unwrap().kind.name().to_owned();
        card(name, None, rack_knobs(id), Some(id));
    }
    let add = Rect::from_min_size(pos2(x, top), vec2(160.0, 44.0));
    (cards, add)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song::{Clip, Track};
    use crate::synthvoice::{SynthKind, SynthPatch};
    use eframe::egui::{Event, Modifiers, PointerButton, RawInput};
    use std::sync::Arc;

    const SCREEN: Rect = Rect::from_min_max(Pos2::ZERO, pos2(1400.0, 300.0));

    fn song() -> Song {
        let patch = SynthPatch::new(SynthKind::Fm, "fm");
        let track = Track::new("a", 0).with_clip(Clip::beats(0, 4, 4));
        let mut song = Song::new(120.0, vec![Instrument::Synth(Arc::new(patch))], vec![track]);
        song.rack.append_effect(0, Kind::Delay);
        song
    }

    struct Harness {
        ctx: egui::Context,
        view: DevicesView,
        song: Song,
        out: Vec<Output>,
    }

    impl Harness {
        fn new() -> Self {
            let mut h = Harness {
                ctx: egui::Context::default(),
                view: DevicesView::default(),
                song: song(),
                out: Vec::new(),
            };
            h.frame(vec![]);
            h
        }

        fn frame(&mut self, events: Vec<Event>) {
            let input = RawInput {
                screen_rect: Some(SCREEN),
                events,
                ..Default::default()
            };
            let (view, song, out) = (&mut self.view, &self.song, &mut self.out);
            let mut full = self.ctx.run_ui(input, |ui| {
                let o = view.show(ui, song, 0);
                if o != Output::default() {
                    out.push(o);
                }
            });
            full.textures_delta.clear();
        }

        fn cards(&mut self) -> Vec<Card> {
            let w = self
                .ctx
                .fonts_mut(|f| f.glyph_width(&FontId::monospace(11.0), '0'));
            layout(&self.song, 0, 0, SCREEN, 0.0, w).0
        }

        fn button(&mut self, pos: Pos2, pressed: bool) {
            self.frame(vec![Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            }]);
        }
    }

    #[test]
    fn cards_for_instrument_track_and_chain() {
        let mut h = Harness::new();
        let titles: Vec<String> = h.cards().into_iter().map(|c| c.title).collect();
        assert_eq!(titles, ["fm", "track", "delay"]);
    }

    #[test]
    fn dragging_a_synth_knob_turns_it() {
        let mut h = Harness::new();
        let (k, _, _, c) = h.cards()[0].knobs[1];
        h.frame(vec![Event::PointerMoved(c)]);
        h.button(c, true);
        for dy in [10.0, 20.0, 30.0] {
            h.frame(vec![Event::PointerMoved(c - vec2(0.0, dy))]);
        }
        h.button(c - vec2(0.0, 30.0), false);
        let lives: Vec<&Edit> = h.out.iter().filter_map(|o| o.live.as_ref()).collect();
        let Some(Edit::SynthParam { param, value, .. }) = lives.last() else {
            panic!("{lives:?}")
        };
        assert_eq!(
            k,
            Knob::Synth {
                instrument: 0,
                param: 1
            }
        );
        assert_eq!(*param, 1);
        let spec = &SynthKind::Fm.params()[1];
        assert!(*value > spec.default, "up turns it up");
        assert!(h.out.iter().any(|o| o.gesture_done));
    }

    #[test]
    fn clicking_a_switch_steps_it() {
        let mut h = Harness::new();
        let (_, spec, value, c) = h.cards()[1].knobs[0];
        assert_eq!(spec.name, "declick");
        h.frame(vec![Event::PointerMoved(c)]);
        h.button(c, true);
        h.button(c, false);
        let edits: Vec<&Edit> = h.out.iter().flat_map(|o| &o.edits).collect();
        let Some(Edit::RackParam {
            param: 0, value: v, ..
        }) = edits.first()
        else {
            panic!("{edits:?}")
        };
        assert_ne!(*v, value);
    }

    #[test]
    fn the_cross_removes_an_effect() {
        let mut h = Harness::new();
        let (id, r) = h.cards()[2].remove.unwrap();
        h.frame(vec![Event::PointerMoved(r.center())]);
        h.button(r.center(), true);
        h.button(r.center(), false);
        let edits: Vec<&Edit> = h.out.iter().flat_map(|o| &o.edits).collect();
        assert_eq!(edits, [&Edit::RemoveEffect(id)]);
    }
}
