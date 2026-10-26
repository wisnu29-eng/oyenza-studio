//! Island geometry and its spring animation.

use crate::pill::motion;
use poni_core::geom::Rect;
use poni_core::spring::Spring;

/// Concave flare where the island meets the top screen edge.
pub const SHOULDER: f32 = 9.0;
/// Room around the island in the canvas. While expanded it is filled at
/// alpha 1/255 so it still catches the mouse (a forgiving edge for the big
/// panel); collapsed it stays fully transparent and clicks pass through.
/// SHOULDER + HIT_MARGIN is kept even (20) so the body edge lands on a whole
/// pixel at 150 % scaling.
pub const HIT_MARGIN: f32 = 11.0;

/// Spring tunings (stiffness, damping). Growing goes wide first, then drops:
/// height trails width a little. Shrinking is nearly critically damped so the
/// small pill doesn't squash below its size. Corners never wobble.
/// A floating island (the Pill theme) moves by [`motion`]'s tunings instead.
const GROW_W: (f32, f32) = (420.0, 30.0); // zeta 0.73
const GROW_H: (f32, f32) = (300.0, 26.0); // zeta 0.75
const SHRINK_W: (f32, f32) = (330.0, 33.0); // zeta 0.91
const SHRINK_H: (f32, f32) = (420.0, 38.0); // zeta 0.93
const RADIUS: (f32, f32) = (420.0, 41.0); // critical
/// A hurried grow (the pointer rested on the pill): stiff, critically damped
/// springs that put the menu there in about a fifth of a second and settle
/// in under a third, so the eye can't call it a wait.
const HURRY_W: (f32, f32) = (3600.0, 120.0); // critical
const HURRY_H: (f32, f32) = (3000.0, 110.0); // critical
const HURRY_R: (f32, f32) = (3600.0, 120.0); // critical
const HURRY_TOP: (f32, f32) = (1200.0, 69.3); // critical
/// Content opacity: critically damped, tau about 38 ms.
const CONTENT: (f32, f32) = (700.0, 52.9);
/// Delay before new content starts fading in (seconds).
const FADE_DELAY_GROW: f64 = 0.03;
const FADE_DELAY_SHRINK: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub w: f32,
    pub h: f32,
    /// Bottom corner radius (every corner's while the island floats).
    pub r: f32,
    /// Floating only: how far the body sits below its gap (DIPs, negative =
    /// up into the gap), such as the hover lift's. Ignored while it hangs.
    pub dy: f32,
}

impl Geom {
    pub const fn new(w: f32, h: f32, r: f32) -> Self {
        Self { w, h, r, dy: 0.0 }
    }

    /// The same shape, `dy` DIPs below the gap while it floats.
    pub const fn below(self, dy: f32) -> Self {
        Self { dy, ..self }
    }
}

pub struct Island {
    w: Spring,
    h: Spring,
    r: Spring,
    content: Spring,
    /// When set, content fades in once `now` passes this time.
    fade_in_at: Option<f64>,
    target: Geom,
    /// The body's distance below the top edge: 0 while it hangs from the
    /// edge, the gap while it floats (the Pill theme). A spring, so picking
    /// Pill drops the island off the edge and leaving Pill docks it again.
    top: Spring,
    /// `Some(gap)` while the island floats.
    gap: Option<f32>,
    /// Floating: the size a shrinking axis lands on and stops at.
    floor_w: Option<f32>,
    floor_h: Option<f32>,
    /// Whether a frame has moved the island yet. Until then a new gap is
    /// taken at once: the island starts where it rests, without a slide.
    stepped: bool,
    /// The next grow uses the hurried tunings (a hover open).
    quick: bool,
}

impl Island {
    pub fn new(g: Geom) -> Self {
        Self {
            w: Spring::new(g.w, GROW_W.0, GROW_W.1),
            h: Spring::new(g.h, GROW_H.0, GROW_H.1),
            r: Spring::new(g.r, RADIUS.0, RADIUS.1),
            content: Spring::new(1.0, CONTENT.0, CONTENT.1),
            fade_in_at: None,
            target: g,
            top: Spring::new(0.0, motion::TOP.0, motion::TOP.1),
            gap: None,
            floor_w: None,
            floor_h: None,
            stepped: false,
            quick: false,
        }
    }

    /// Float `Some(gap)` DIPs below the top edge (the Pill theme), or hang
    /// from it (`None`). Called every frame; a change slides the island off
    /// or onto the edge (before the first frame it simply starts there), and
    /// a morph in flight carries on with the new tunings.
    pub fn set_floating(&mut self, gap: Option<f32>) {
        if gap == self.gap {
            return;
        }
        let retune = gap.is_some() != self.gap.is_some();
        self.gap = gap;
        if retune {
            self.tune();
        }
        self.aim();
        if !self.stepped {
            self.top.snap(self.top_target());
        }
    }

    /// Drawn floating: while it floats, and while it rises back to the edge.
    pub fn floating(&self) -> bool {
        self.gap.is_some() || self.top.value > 0.0
    }

    /// The body's distance below the top edge (DIPs), where it is now.
    pub fn top(&self) -> f32 {
        self.top.value
    }

    pub fn target(&self) -> Geom {
        self.target
    }

    /// Animate towards `g`. When `new_content` is true the content cross-fades:
    /// the new content starts from transparent and fades in shortly after the
    /// outline starts moving (the app fades the old content out meanwhile).
    /// Springs keep their value and velocity; only their tuning changes with
    /// the direction of the morph.
    pub fn set_target(&mut self, g: Geom, now: f64, new_content: bool) {
        self.target = g;
        let quick = std::mem::take(&mut self.quick);
        let growing = self.tune();
        // A hover open spends no time on the long grow: the pointer is already
        // on the pill, so the menu comes as soon as it can.
        let quick_open = quick && growing;
        if quick_open {
            self.w.retune(HURRY_W.0, HURRY_W.1);
            self.h.retune(HURRY_H.0, HURRY_H.1);
            self.r.retune(HURRY_R.0, HURRY_R.1);
            self.top.retune(HURRY_TOP.0, HURRY_TOP.1);
        }
        self.aim();
        if new_content {
            if quick_open {
                // The page is already there; the growing outline reveals it,
                // so a hover open has neither a wait nor a fade-in.
                self.content.snap(1.0);
                self.fade_in_at = None;
            } else {
                self.content.snap(0.0);
                self.fade_in_at = Some(now + if growing { FADE_DELAY_GROW } else { FADE_DELAY_SHRINK });
            }
        }
    }

    /// Use the hurried springs for the next grow (a hover open). A morph in
    /// any other direction keeps its usual tunings.
    pub fn hurry(&mut self) {
        self.quick = true;
    }

    /// Tune the springs for the morph to the target from where the outline
    /// is now (they keep value and velocity). Returns whether it grows.
    fn tune(&mut self) -> bool {
        let g = self.target;
        let growing = g.w * g.h > self.w.value * self.h.value;
        if self.gap.is_some() {
            // Floating: each axis by its own direction, so an axis that
            // shrinks while the other grows still lands on its size.
            let (tw, th) = (
                if g.w >= self.w.value { motion::GROW_W } else { motion::SHRINK_W },
                if g.h >= self.h.value { motion::GROW_H } else { motion::SHRINK_H },
            );
            let tr = if g.r >= self.r.value { motion::RADIUS_GROW } else { motion::RADIUS_SHRINK };
            self.w.retune(tw.0, tw.1);
            self.h.retune(th.0, th.1);
            self.r.retune(tr.0, tr.1);
            self.floor_w = floor(&self.w, g.w, self.floor_w);
            self.floor_h = floor(&self.h, g.h, self.floor_h);
        } else {
            let (tw, th) = if growing { (GROW_W, GROW_H) } else { (SHRINK_W, SHRINK_H) };
            self.w.retune(tw.0, tw.1);
            self.h.retune(th.0, th.1);
            self.r.retune(RADIUS.0, RADIUS.1);
            self.floor_w = None;
            self.floor_h = None;
        }
        growing
    }

    /// Point every spring at the target.
    fn aim(&mut self) {
        let g = self.target;
        self.w.set_target(g.w);
        self.h.set_target(g.h);
        self.r.set_target(self.radius_for(g));
        self.top.set_target(self.top_target());
    }

    /// The corner radius the springs aim at for `g`: floating, never rounder
    /// than a capsule.
    fn radius_for(&self, g: Geom) -> f32 {
        if self.gap.is_some() {
            g.r.min(g.h / 2.0)
        } else {
            g.r
        }
    }

    fn top_target(&self) -> f32 {
        self.gap.map_or(0.0, |gap| (gap + self.target.dy).max(0.0))
    }

    /// The new content fades in at once from `alpha` (the same content coming
    /// back mid-fade), instead of from transparent after the usual delay.
    pub fn resume_content(&mut self, alpha: f32) {
        self.content.snap(alpha.clamp(0.0, 1.0));
        self.content.set_target(1.0);
        self.fade_in_at = None;
    }

    /// Jump straight to `g` without animating.
    pub fn snap(&mut self, g: Geom) {
        self.target = g;
        self.w.snap(g.w);
        self.h.snap(g.h);
        self.r.snap(self.radius_for(g));
        self.top.snap(self.top_target());
        self.floor_w = None;
        self.floor_h = None;
        self.quick = false;
        self.content.snap(1.0);
        self.fade_in_at = None;
    }

    pub fn step(&mut self, dt: f32, now: f64) {
        self.stepped = true;
        self.w.step(dt);
        self.h.step(dt);
        self.r.step(dt);
        if self.gap.is_some() {
            land(&mut self.w, self.floor_w);
            land(&mut self.h, self.floor_h);
            // A capsule's corners are its ends: once the height has landed,
            // so have they (they're drawn at half the height anyway).
            if self.h.is_settled() && self.r.target >= self.h.target / 2.0 && self.r.value >= self.h.value / 2.0 {
                self.r.snap(self.r.target);
            }
        }
        self.top.step(dt);
        if let Some(t) = self.fade_in_at {
            if now >= t {
                self.content.set_target(1.0);
                self.fade_in_at = None;
            }
        }
        self.content.step(dt);
    }

    pub fn settled(&self) -> bool {
        self.fade_in_at.is_none()
            && self.w.is_settled()
            && self.h.is_settled()
            && self.r.is_settled()
            && self.top.is_settled()
            && self.content.is_settled_eps(0.002)
    }

    pub fn current(&self) -> Geom {
        let (w, h) = (self.w.value.max(20.0), self.h.value.max(8.0));
        // Floating, a corner is never drawn rounder than half the height.
        let r = if self.gap.is_some() { self.r.value.min(h / 2.0) } else { self.r.value };
        Geom { w, h, r: r.max(0.0), dy: self.top.value - self.gap.unwrap_or(0.0) }
    }

    pub fn content_alpha(&self) -> f32 {
        self.content.value.clamp(0.0, 1.0)
    }

    /// The island body, hanging from the top edge or floating below it,
    /// centred on `cx` (DIPs).
    pub fn body_rect_at(&self, cx: f32) -> Rect {
        let g = self.current();
        Rect::new(cx - g.w / 2.0, self.top.value, g.w, g.h)
    }

    /// The island plus its shoulders (or, floating, the gap above it) and
    /// hit margin, centred on `cx` (DIPs).
    pub fn canvas_rect_at(&self, cx: f32) -> Rect {
        let g = self.current();
        let side = self.side_room();
        Rect::new(cx - g.w / 2.0 - side, 0.0, g.w + 2.0 * side, self.top.value + g.h + self.bottom_room())
    }

    /// Room the window keeps on each side of the body: the shoulders and
    /// hit margin, or (floating) the shadow's room.
    pub fn side_room(&self) -> f32 {
        if self.floating() {
            crate::pill::shape::MARGIN
        } else {
            SHOULDER + HIT_MARGIN
        }
    }

    /// Room the window keeps below the body.
    pub fn bottom_room(&self) -> f32 {
        if self.floating() {
            crate::pill::shape::MARGIN
        } else {
            HIT_MARGIN
        }
    }

    /// Where the resting island takes the mouse: its body, and while it
    /// floats the gap above it too (a pointer thrown at the top edge).
    pub fn hover_rect_at(&self, cx: f32) -> Rect {
        let body = self.body_rect_at(cx);
        if self.floating() {
            crate::pill::shape::hover_rect(body)
        } else {
            body
        }
    }
}

/// The size an axis heading for `to` must not go below: `to` itself when it
/// shrinks there. Growing, a spring still falling from the last morph keeps
/// that morph's floor, so it turns around above it instead of squashing.
fn floor(s: &Spring, to: f32, was: Option<f32>) -> Option<f32> {
    if to < s.value {
        Some(to)
    } else if s.velocity < 0.0 {
        was
    } else {
        None
    }
}

/// A shrinking axis stops on its floor instead of dipping past it (a
/// spring turned around mid-flight can carry it through).
fn land(s: &mut Spring, floor: Option<f32>) {
    if let Some(f) = floor {
        if s.value < f {
            s.value = f;
            s.velocity = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PILL: Geom = Geom::new(120.0, 32.0, 16.0);
    const CARD: Geom = Geom::new(620.0, 216.0, 40.0);

    /// A floating island resting as `g`, the gap taken before the first frame.
    fn floating(g: Geom) -> Island {
        let mut i = Island::new(g);
        i.set_floating(Some(crate::pill::TOP_GAP));
        i
    }

    /// Step at `hz` until settled (or `max_s`); calls `each` after every frame.
    fn run(i: &mut Island, hz: f32, max_s: f32, mut each: impl FnMut(&Island)) -> f32 {
        let dt = 1.0 / hz;
        let mut t = 0.0;
        while !i.settled() && t < max_s {
            i.step(dt, t as f64);
            t += dt;
            each(i);
        }
        t
    }

    fn tuning(s: &Spring) -> (f32, f32) {
        (s.stiffness, s.damping)
    }

    #[test]
    fn classic_tunings_unchanged() {
        assert_eq!((GROW_W, GROW_H, SHRINK_W, SHRINK_H, RADIUS), ((420.0, 30.0), (300.0, 26.0), (330.0, 33.0), (420.0, 38.0), (420.0, 41.0)));
        assert_eq!((CONTENT, FADE_DELAY_GROW, FADE_DELAY_SHRINK), ((700.0, 52.9), 0.03, 0.05));
        let mut i = Island::new(PILL);
        i.set_floating(None);
        i.set_target(CARD, 0.0, true);
        assert_eq!((tuning(&i.w), tuning(&i.h), tuning(&i.r)), (GROW_W, GROW_H, RADIUS));
        assert_eq!(i.fade_in_at, Some(FADE_DELAY_GROW));
        run(&mut i, 60.0, 2.0, |i| assert_eq!(i.top(), 0.0));
        assert_eq!(i.current(), CARD);
        i.set_target(PILL, 0.0, true);
        assert_eq!((tuning(&i.w), tuning(&i.h), tuning(&i.r)), (SHRINK_W, SHRINK_H, RADIUS));
        assert_eq!(i.fade_in_at, Some(FADE_DELAY_SHRINK));
        // Hanging: no landing floor, the corner radius as given, never floating.
        assert_eq!((i.floor_w, i.floor_h), (None, None));
        run(&mut i, 60.0, 2.0, |i| assert!(!i.floating() && i.top() == 0.0));
        assert_eq!(i.current(), PILL);
    }

    #[test]
    fn classic_morph_matches_the_old_island() {
        // The spring code path Classic ran before the Pill theme, step for step.
        let mut i = Island::new(PILL);
        let (mut w, mut h, mut r) = (Spring::new(120.0, 420.0, 30.0), Spring::new(32.0, 300.0, 26.0), Spring::new(16.0, 420.0, 41.0));
        i.set_target(CARD, 0.0, false);
        (w.target, h.target, r.target) = (CARD.w, CARD.h, CARD.r);
        for k in 0..40 {
            if k == 12 {
                // Turned around mid-flight.
                i.set_target(PILL, 0.2, false);
                w.retune(SHRINK_W.0, SHRINK_W.1);
                h.retune(SHRINK_H.0, SHRINK_H.1);
                (w.target, h.target, r.target) = (PILL.w, PILL.h, PILL.r);
            }
            i.step(1.0 / 60.0, k as f64 / 60.0);
            w.step(1.0 / 60.0);
            h.step(1.0 / 60.0);
            r.step(1.0 / 60.0);
            let g = i.current();
            assert_eq!((g.w, g.h, g.r, g.dy), (w.value.max(20.0), h.value.max(8.0), r.value.max(0.0), 0.0), "frame {k}");
        }
    }

    #[test]
    fn floating_springs_settle() {
        for hz in [60.0, 144.0] {
            let mut i = floating(PILL);
            i.set_target(CARD, 0.0, true);
            assert_eq!((tuning(&i.w), tuning(&i.h), tuning(&i.r)), (motion::GROW_W, motion::GROW_H, motion::RADIUS_GROW));
            let t = run(&mut i, hz, 2.0, |_| {});
            assert!(i.settled() && t < 0.9, "grow at {hz} Hz settled after {t:.2} s");
            assert_eq!(i.current(), CARD);
            i.set_target(PILL, 0.0, true);
            assert_eq!((tuning(&i.w), tuning(&i.h), tuning(&i.r)), (motion::SHRINK_W, motion::SHRINK_H, motion::RADIUS_SHRINK));
            let t = run(&mut i, hz, 2.0, |_| {});
            assert!(i.settled() && t < 0.6, "shrink at {hz} Hz settled after {t:.2} s");
            assert_eq!(i.current(), PILL);
            assert_eq!(i.body_rect_at(300.0), Rect::new(240.0, 8.0, 120.0, 32.0));
        }
    }

    #[test]
    fn a_hurried_grow_settles_sooner() {
        let mut normal = floating(PILL);
        normal.set_target(CARD, 0.0, true);
        let t_normal = run(&mut normal, 60.0, 2.0, |_| {});

        let mut quick = floating(PILL);
        quick.hurry();
        quick.set_target(CARD, 0.0, true);
        assert_eq!((tuning(&quick.w), tuning(&quick.h), tuning(&quick.r)), (HURRY_W, HURRY_H, HURRY_R));
        let t_quick = run(&mut quick, 60.0, 2.0, |_| {});
        assert!(quick.settled() && t_quick < t_normal, "quick {t_quick:.2} s vs normal {t_normal:.2} s");
        // The tail (the last hundredth of a DIP) ends sooner than the normal
        // grow; the panel is visibly there well before either.
        assert!(t_quick < 0.35, "a hurried open took {t_quick:.2} s");
        assert_eq!(quick.current(), CARD);
        // Only the next morph is hurried.
        assert!(!quick.quick);
    }

    #[test]
    fn floating_grow_stretches_then_drops() {
        // The width leads: early on the pill is a wider capsule than either end.
        let mut i = floating(PILL);
        i.set_target(CARD, 0.0, false);
        let mut widest: f32 = 0.0;
        let (mut w_peak, mut h_peak): (f32, f32) = (0.0, 0.0);
        run(&mut i, 60.0, 2.0, |i| {
            let g = i.current();
            widest = widest.max(g.w / g.h);
            w_peak = w_peak.max(g.w);
            h_peak = h_peak.max(g.h);
        });
        assert!(widest > PILL.w / PILL.h + 0.25, "aspect peaked at {widest:.2}");
        // A little overshoot, inside the window's 5 % room.
        assert!(w_peak > CARD.w && w_peak < CARD.w * 1.035, "width peaked at {w_peak}");
        assert!(h_peak > CARD.h && h_peak < CARD.h * 1.035, "height peaked at {h_peak}");
    }

    #[test]
    fn floating_shrink_never_squashes() {
        // (from, targets with the time they come, the least size allowed): straight
        // down, an axis shrinking while the other grows, and morphs turned around
        // mid-flight (onto a lift just above the pill, and onto a size the width
        // has already passed: it carries on a little, then turns, above the pill).
        let lifted = motion::hover_lift(PILL);
        let cases: [(Geom, &[(f32, Geom)], (f32, f32)); 4] = [
            (CARD, &[(0.0, PILL)], (120.0, 32.0)),
            (Geom::new(340.0, 48.0, 24.0), &[(0.0, Geom::new(520.0, 32.0, 16.0))], (340.0, 32.0)),
            (CARD, &[(0.0, PILL), (0.12, lifted)], (126.0, 32.0)),
            (CARD, &[(0.0, PILL), (0.2, Geom::new(150.0, 36.0, 18.0))], (120.0, 32.0)),
        ];
        for (from, steps, least) in cases {
            let mut i = floating(from);
            let (mut t, mut next, mut want) = (0.0, 0, from);
            while t < 1.5 {
                if next < steps.len() && t >= steps[next].0 - 1e-4 {
                    want = steps[next].1;
                    i.set_target(want, t as f64, false);
                    next += 1;
                }
                i.step(1.0 / 60.0, t as f64);
                t += 1.0 / 60.0;
                let g = i.current();
                assert!(g.w >= least.0 && g.h >= least.1, "{from:?} -> {want:?}: {g:?} at {t:.3} s");
                assert!(g.r <= g.h / 2.0 + 1e-4, "rounder than a capsule: {g:?}");
            }
            assert!(i.settled(), "{from:?} -> {want:?} did not settle");
            assert_eq!((i.current().w, i.current().h), (want.w, want.h));
        }
    }

    #[test]
    fn a_turned_around_morph_keeps_its_floor() {
        // Falling fast just above the pill when a slightly wider target comes: it
        // turns around on the pill's size at the lowest, never below it.
        let mut i = floating(CARD);
        i.set_target(PILL, 0.0, false);
        (i.w.value, i.w.velocity) = (124.0, -1500.0);
        i.set_target(Geom::new(130.0, 32.0, 16.0), 0.0, false);
        assert_eq!(tuning(&i.w), motion::GROW_W);
        let mut least = f32::MAX;
        run(&mut i, 60.0, 2.0, |i| least = least.min(i.current().w));
        assert_eq!(least, 120.0);
        assert_eq!(i.current().w, 130.0);
    }

    #[test]
    fn capsule_stays_a_capsule() {
        let lifted = motion::hover_lift(PILL);
        let mut i = floating(PILL);
        for (to, max_s) in [(lifted, 0.6), (PILL, 0.5), (CARD, 0.9), (PILL, 0.6)] {
            i.set_target(to, 0.0, false);
            let t = run(&mut i, 60.0, 2.0, |i| {
                let g = i.current();
                assert!(g.r <= g.h / 2.0 + 1e-4, "{g:?}");
            });
            assert!(t < max_s, "to {to:?} took {t:.2} s");
            let g = i.current();
            assert_eq!((g.w, g.h, g.r), (to.w, to.h, to.r));
        }
    }

    #[test]
    fn hover_lift_rises_and_sinks() {
        let lifted = motion::hover_lift(PILL);
        let mut i = floating(PILL);
        i.set_target(lifted, 0.0, false);
        let mut highest: f32 = 8.0;
        run(&mut i, 60.0, 2.0, |i| highest = highest.min(i.top()));
        // Centred growth (3 up) less the 1 DIP sink: the top settles 2 DIPs up.
        assert_eq!(i.top(), 6.0);
        assert!(highest >= 6.0 - 1e-3, "rose to {highest}");
        assert_eq!(i.body_rect_at(300.0), Rect::new(237.0, 6.0, 126.0, 38.0));
        i.set_target(PILL, 0.0, false);
        run(&mut i, 60.0, 2.0, |i| assert!(i.top() <= 8.0 + 1e-3 && i.top() >= 6.0 - 1e-3));
        assert_eq!((i.top(), i.current()), (8.0, PILL));
    }

    #[test]
    fn first_gap_is_taken_at_once() {
        let mut i = Island::new(PILL);
        i.set_floating(Some(8.0));
        assert_eq!(i.top(), 8.0);
        assert!(i.floating() && i.settled());
        i.step(1.0 / 60.0, 0.0);
        assert_eq!(i.top(), 8.0);
        // Snapping takes the gap at once too.
        let mut j = Island::new(PILL);
        j.step(1.0 / 60.0, 0.0);
        j.set_floating(Some(8.0));
        j.snap(motion::hover_lift(PILL));
        assert_eq!(j.top(), 6.0);
        assert!(j.settled());
    }

    #[test]
    fn top_slides_off_and_back_onto_the_edge() {
        for hz in [60.0, 144.0] {
            let mut i = Island::new(PILL);
            i.step(1.0 / hz, 0.0);
            // Pill picked: the island drops off the edge and settles at the gap.
            i.set_floating(Some(8.0));
            assert!(i.floating() && !i.settled() && i.top() == 0.0);
            let mut between = 0;
            let mut last = 0.0;
            let t = run(&mut i, hz, 2.0, |i| {
                assert!(i.top() >= last && i.top() <= 8.0, "top {} after {last}", i.top());
                last = i.top();
                if i.top() > 0.5 && i.top() < 7.5 {
                    between += 1;
                }
            });
            assert!(between >= (0.1 * hz) as i32, "top jumped: {between} frames in between at {hz} Hz");
            assert!(t < 0.6, "gap settled after {t:.2} s");
            assert_eq!(i.top(), 8.0);
            // Another theme: it rises back, drawn floating until it docks.
            i.set_floating(None);
            assert!(i.floating());
            let mut last = 8.0;
            run(&mut i, hz, 2.0, |i| {
                assert!(i.top() <= last && i.top() >= 0.0);
                assert_eq!(i.floating(), i.top() > 0.0);
                last = i.top();
            });
            assert!(i.settled() && i.top() == 0.0 && !i.floating());
            assert_eq!((i.side_room(), i.bottom_room()), (SHOULDER + HIT_MARGIN, HIT_MARGIN));
        }
    }

    #[test]
    fn switching_mid_morph_retunes() {
        let mut i = Island::new(PILL);
        i.set_target(CARD, 0.0, false);
        for k in 0..5 {
            i.step(1.0 / 60.0, k as f64 / 60.0);
        }
        let v = i.w.velocity;
        i.set_floating(Some(8.0));
        // Same motion, floating tunings: nothing jumps.
        assert_eq!((tuning(&i.w), tuning(&i.h)), (motion::GROW_W, motion::GROW_H));
        assert_eq!(i.w.velocity, v);
        i.set_floating(None);
        assert_eq!((tuning(&i.w), tuning(&i.h), tuning(&i.r)), (GROW_W, GROW_H, RADIUS));
    }
}
