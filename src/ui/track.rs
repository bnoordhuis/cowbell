//! The track screen's helpers: finding the clip to show when moving
//! between clips and tracks, and the strip of a track's clips that shows
//! which one is open.

use eframe::egui::{self, CornerRadius, CursorIcon, Rect, Sense, Stroke, StrokeKind, pos2, vec2};

use super::color;
use crate::song::{ClipRef, Song, TICKS_PER_BAR, Tick};

/// The clip `delta` places along from `at` on its track (−1 is the one
/// before). With no clip open on `track`, the first (going forward) or
/// last (going back). Stops at the ends.
pub fn step_clip(song: &Song, track: usize, at: Option<ClipRef>, delta: isize) -> Option<ClipRef> {
    let clips = song.tracks.get(track)?.clips.len();
    if clips == 0 {
        return None;
    }
    let i = match at.filter(|c| c.0 == track && c.1 < clips) {
        Some((_, i)) => i.saturating_add_signed(delta).min(clips - 1),
        None if delta < 0 => clips - 1,
        None => 0,
    };
    Some((track, i))
}

/// The clip on `track` nearest to `tick`: the one playing then, or else
/// the one starting or ending closest to it.
pub fn clip_near(song: &Song, track: usize, tick: Tick) -> Option<ClipRef> {
    let clips = &song.tracks.get(track)?.clips;
    let distance = |c: &crate::song::Clip| {
        if c.span().contains(&tick) {
            0
        } else {
            c.start.abs_diff(tick).min(c.end().abs_diff(tick + 1))
        }
    };
    let (i, _) = clips.iter().enumerate().min_by_key(|(_, c)| distance(c))?;
    Some((track, i))
}

/// A thin strip of `track`'s clips over the whole song, with `open`
/// highlighted and the playhead at `pos`. Returns a clip that was clicked.
pub fn strip(
    ui: &mut egui::Ui,
    song: &Song,
    track: usize,
    open: Option<ClipRef>,
    pos: Tick,
) -> Option<ClipRef> {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, color::PANEL);
    let t = song.tracks.get(track)?;
    // Some room past the end, so the last clip isn't jammed at the edge.
    let len = (song.end().max(t.end()) + TICKS_PER_BAR).max(4 * TICKS_PER_BAR) as f32;
    let x = |tick: Tick| rect.left() + rect.width() * tick as f32 / len;
    let tint = color::TRACKS[track % color::TRACKS.len()];
    let mut clicked = None;
    let pointer = response.hover_pos();
    for (i, c) in t.clips.iter().enumerate() {
        let r = Rect::from_min_max(
            pos2(x(c.start), rect.top() + 3.0),
            pos2(x(c.end()).max(x(c.start) + 3.0), rect.bottom() - 3.0),
        );
        let is_open = open == Some((track, i));
        let fill = if is_open {
            tint
        } else {
            tint.gamma_multiply(0.45)
        };
        p.rect_filled(r, CornerRadius::same(2), fill);
        if is_open {
            p.rect_stroke(r, 2.0, Stroke::new(2.0, color::ACCENT), StrokeKind::Outside);
        }
        if pointer.is_some_and(|q| r.expand(2.0).contains(q)) {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            if response.clicked() {
                clicked = Some((track, i));
            }
        }
    }
    let px = x(pos);
    p.vline(px, rect.y_range(), Stroke::new(1.5, color::ACCENT));
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song::{Clip, Track};

    /// Track 0: clips at bars 0, 2 and 6. Track 1: one at bar 3. Track 2:
    /// none.
    fn song() -> Song {
        let bar = TICKS_PER_BAR;
        let a = Track::new("a", 0)
            .with_clip(Clip::beats(0, 4, 4))
            .with_clip(Clip::beats(2 * bar, 4, 8))
            .with_clip(Clip::beats(6 * bar, 4, 4));
        let b = Track::new("b", 0).with_clip(Clip::beats(3 * bar, 4, 4));
        Song::new(120.0, Vec::new(), vec![a, b, Track::new("c", 0)])
    }

    #[test]
    fn stepping_along_a_track_stops_at_its_ends() {
        let s = song();
        assert_eq!(step_clip(&s, 0, Some((0, 0)), 1), Some((0, 1)));
        assert_eq!(step_clip(&s, 0, Some((0, 2)), 1), Some((0, 2)), "the last");
        assert_eq!(
            step_clip(&s, 0, Some((0, 0)), -1),
            Some((0, 0)),
            "the first"
        );
        assert_eq!(step_clip(&s, 0, None, 1), Some((0, 0)));
        assert_eq!(step_clip(&s, 0, None, -1), Some((0, 2)));
        assert_eq!(
            step_clip(&s, 0, Some((1, 0)), 1),
            Some((0, 0)),
            "a clip on another track counts as none"
        );
        assert_eq!(step_clip(&s, 2, None, 1), None, "no clips");
        assert_eq!(step_clip(&s, 9, None, 1), None);
    }

    #[test]
    fn the_nearest_clip_is_the_one_playing_or_else_the_closest() {
        let s = song();
        let bar = TICKS_PER_BAR;
        assert_eq!(clip_near(&s, 0, 2 * bar + 10), Some((0, 1)), "playing");
        assert_eq!(
            clip_near(&s, 0, 5 * bar),
            Some((0, 2)),
            "closer to the next"
        );
        assert_eq!(
            clip_near(&s, 0, 4 * bar + 10),
            Some((0, 1)),
            "just past one"
        );
        assert_eq!(clip_near(&s, 1, 0), Some((1, 0)), "the only one");
        assert_eq!(clip_near(&s, 2, 0), None);
    }
}
