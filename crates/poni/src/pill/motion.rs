//! How the floating island moves: its springs (it can stretch and settle
//! like a drop, since nothing holds it to the edge), how it arrives when
//! the theme is picked, and where leaving content gathers.
//!
//! [`crate::island::Island`] takes these tunings while it floats and the
//! Classic ones while it hangs from the edge; the app picks the content
//! transforms below by the same test. They only shape motion that already
//! runs: nothing here adds a frame at rest.

use crate::island::Geom;
use poni_core::geom::{Point, Rect};

/// Spring tunings (stiffness, damping) while the island floats, picked per
/// axis by the way that axis moves (Classic picks both by the area).
///
/// Growing, the width leads and the height trails further than Classic's
/// (their frequencies differ by a factor of 1.4, Classic's by 1.2): the pill
/// first stretches into a wider capsule, then drops into the card, and both
/// overshoot about 3 % and settle, like a drop finding its shape. The window
/// leaves 5 % for overshoot, so this is as far as they may go.
pub const GROW_W: (f32, f32) = (500.0, 32.5); // zeta 0.73
pub const GROW_H: (f32, f32) = (250.0, 23.0); // zeta 0.73
/// Shrinking, the height leads (the card flattens into a wide capsule) and
/// the width gathers last: the grow played backwards. A little under
/// critical damping for a quick finish, and the island lands on its size:
/// a shrinking axis is never let below its target ([`crate::island::Island::step`]),
/// so the pill never squashes, even when a morph turns around mid-flight.
pub const SHRINK_W: (f32, f32) = (400.0, 36.0); // zeta 0.90
pub const SHRINK_H: (f32, f32) = (560.0, 43.0); // zeta 0.91
/// Corners, critically damped (they never wobble). Growing, they round out
/// at once, so the card grows round, never boxy. Shrinking, they stay round
/// while the card gathers; a corner is never drawn rounder than half the
/// height, so the card turns into a capsule as soon as it's thin enough and
/// gathers into the pill as one.
pub const RADIUS_GROW: (f32, f32) = (1200.0, 69.3); // critical
pub const RADIUS_SHRINK: (f32, f32) = (150.0, 24.5); // critical
/// The gap: the island drops off the edge when Pill is picked and rises
/// back onto it when another theme is; the hover sink too. Critically
/// damped, so it never pokes above the edge or bobs (tau about 58 ms).
pub const TOP: (f32, f32) = (300.0, 34.6); // critical

/// The resting pill under the pointer swells this much on every side (Classic
/// grows 12 wide and 4 down, from the edge it hangs on): an even outset keeps
/// a capsule a capsule.
pub const HOVER_OUTSET: f32 = 3.0;
/// ...and sinks this much as it swells, so it reaches 2 DIPs into the gap,
/// not 3, and reads as hanging a little lower rather than climbing the edge.
pub const HOVER_SINK: f32 = 1.0;

/// The resting island `g` under the pointer.
pub fn hover_lift(g: Geom) -> Geom {
    let d = HOVER_OUTSET;
    Geom::new(g.w + 2.0 * d, g.h + 2.0 * d, g.r + d).below(g.dy - d + HOVER_SINK)
}

/// Where content is anchored during a morph to `target`: the middle of the
/// morph's small end, so the leaving picture shrinks back into the pill
/// (Classic's is the top centre, where the island hangs). Opening, that's
/// the body's middle as it grows; closing, the middle of the pill it's
/// becoming, near the top, so the leaving card gathers into the pill instead
/// of into the middle of a card that's no longer there.
pub fn pivot(body: Rect, target: Geom) -> Point {
    Point::new(body.center().x, body.y + body.h.min(target.h) / 2.0)
}

/// How far the outgoing content, laid out at its old size `was`, has shrunk
/// with the outline `now`: as much as the axis that has shrunk more (Classic
/// follows the width), so the picture shrinks inside the outline instead of
/// being cut by its rounded top corners. Never below `min`, never grown.
pub fn ghost_scale(now: Geom, was: Geom, min: f32) -> f32 {
    (now.w / was.w.max(1.0)).min(now.h / was.h.max(1.0)).clamp(min, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zeta(t: (f32, f32)) -> f32 {
        t.1 / (2.0 * t.0.sqrt())
    }

    #[test]
    fn tunings_are_what_the_comments_say() {
        for (t, z) in [(GROW_W, 0.73), (GROW_H, 0.73), (SHRINK_W, 0.90), (SHRINK_H, 0.91), (RADIUS_GROW, 1.0), (RADIUS_SHRINK, 1.0), (TOP, 1.0)] {
            assert!((zeta(t) - z).abs() < 0.006, "{t:?}: zeta {} (comment says {z})", zeta(t));
        }
        // The width leads the height growing, the height leads shrinking.
        assert!(GROW_W.0 > GROW_H.0 && SHRINK_H.0 > SHRINK_W.0);
    }

    #[test]
    fn hover_lift_keeps_a_capsule() {
        let g = hover_lift(Geom::new(120.0, 32.0, 16.0));
        assert_eq!((g.w, g.h, g.r), (126.0, 38.0, 19.0));
        assert_eq!(g.r, g.h / 2.0);
        // Centred on the resting pill, then 1 DIP lower: 2 up into the gap, 4 down.
        assert_eq!(g.dy, -2.0);
    }

    #[test]
    fn content_gathers_about_the_small_end() {
        let pill = Geom::new(120.0, 32.0, 16.0);
        let card = Geom::new(620.0, 216.0, 40.0);
        // Opening: the middle of the body as it grows.
        let growing = Rect::new(100.0, 8.0, 300.0, 90.0);
        assert_eq!(pivot(growing, card), Point::new(250.0, 53.0));
        // Closing: the middle of the pill it's becoming.
        let closing = Rect::new(40.0, 8.0, 420.0, 140.0);
        assert_eq!(pivot(closing, pill), Point::new(250.0, 24.0));
        // At rest, the body's middle.
        assert_eq!(pivot(Rect::new(190.0, 8.0, 120.0, 32.0), pill), Point::new(250.0, 24.0));
    }

    #[test]
    fn ghost_shrinks_with_the_faster_axis() {
        let card = Geom::new(620.0, 216.0, 40.0);
        assert_eq!(ghost_scale(card, card, 0.6), 1.0);
        // The height leads a floating shrink: it sets the scale.
        let mid = Geom::new(496.0, 162.0, 30.0);
        assert!((ghost_scale(mid, card, 0.6) - 0.75).abs() < 1e-6);
        assert_eq!(ghost_scale(Geom::new(120.0, 32.0, 16.0), card, 0.6), 0.6);
        // Growing never enlarges the old picture.
        assert_eq!(ghost_scale(card, Geom::new(120.0, 32.0, 16.0), 0.6), 1.0);
    }
}
