//! Desktops: Windows' virtual desktops as a row of live previews. Click one
//! to switch to it; drag an app from the list below (or from another desktop's
//! preview) onto a desktop to move it there with the mouse, no three-finger
//! swipe. The worker behind it (`services::desktops`) runs only while this
//! page is open.

use crate::commands::{Command, DesktopCmd};
use crate::gfx::{TextStyle, Weight};
use crate::icons::Icon;
use crate::pages::{empty_state, snap};
use crate::state::{AppState, DesktopDrag, DesktopWindow};
use crate::ui::{wid, wid_n, CursorKind, Ui};
use poni_core::geom::{palette, Color, Rect};

/// Same height as the System page: the island never grows for desktops.
const HEIGHT: f32 = 256.0;
const CARD_GAP: f32 = 8.0;
/// Four cards fit the default 584 DIP content width.
const CARDS_ACROSS: f32 = 4.0;
const CARD_H: f32 = 102.0;
const CARD_R: f32 = 11.0;
const LABEL_H: f32 = 20.0;
const LIST_GAP: f32 = 12.0;
const ROW_H: f32 = 25.0;
const CHIP_GAP: f32 = 6.0;
const CHIP_R: f32 = 7.0;
/// A press that moves this far (DIPs) is a drag, not a click.
const DRAG_SLOP: f32 = 7.0;
/// Desktop tints when Windows has no wallpaper to show (blue, purple, teal,
/// orange, green, pink).
const TINTS: [Color; 6] = [palette::BLUE, palette::PURPLE, palette::CYAN, palette::ORANGE, palette::GREEN, palette::PINK];

pub fn height(_st: &AppState) -> f32 {
    HEIGHT
}

/// When the "Moved…" (or a failure) line runs out and the page needs a frame
/// to drop it. While it shows, nothing else moves.
pub fn next_wake(st: &AppState) -> Option<f64> {
    st.desktops.flash.as_ref().map(|(_, until)| *until).filter(|until| *until > st.now)
}

pub fn draw(ui: &mut Ui, st: &mut AppState, area: Rect) {
    let scale = ui.c.scale().max(0.5);
    if !st.desktops.available {
        empty_state(ui, area, Icon::Desktops, "Virtual desktops are on Windows", "macOS keeps its own Spaces.");
        return;
    }
    if st.desktops.list.is_empty() {
        let (title, hint) = match (&st.desktops.error, st.desktops.loading) {
            (Some(e), _) => ("Virtual desktops", e.as_str()),
            (None, true) => ("Finding desktops\u{2026}", "Windows is telling us what it has."),
            (None, false) => ("Virtual desktops", "Switch tabs and come back to look again."),
        };
        empty_state(ui, area, Icon::Desktops, title, hint);
        return;
    }

    let new_w = 36.0;
    let cards_w = (area.w - new_w - CARD_GAP).max(1.0);
    let card_w = snap(((cards_w - (CARDS_ACROSS - 1.0) * CARD_GAP) / CARDS_ACROSS).clamp(104.0, 168.0), scale);
    let card_h = CARD_H;
    let strip = Rect::new(area.x, area.y, cards_w, card_h);
    let dragging = st.ui.desktop_drag.clone();
    let mut cards: Vec<(String, Rect)> = Vec::with_capacity(st.desktops.list.len());
    let total = st.desktops.list.len() as f32 * (card_w + CARD_GAP) - CARD_GAP;
    let max_scroll = (total - strip.w).max(0.0);
    let wheel = ui.wheel_in(strip);
    if wheel != 0.0 && max_scroll > 0.0 {
        st.ui.desktop_scroll = (st.ui.desktop_scroll - wheel * 46.0).clamp(0.0, max_scroll);
        ui.request_repaint();
    }
    st.ui.desktop_scroll = st.ui.desktop_scroll.clamp(0.0, max_scroll);

    ui.c.clip(strip);
    for i in 0..st.desktops.list.len() {
        let x = strip.x + i as f32 * (card_w + CARD_GAP) - st.ui.desktop_scroll;
        if x > strip.right() || x + card_w < strip.x {
            continue;
        }
        let card = Rect::new(snap(x, scale), strip.y, card_w, card_h);
        cards.push((st.desktops.list[i].id.clone(), card));
        draw_card(ui, st, i, card, dragging.as_ref(), scale);
    }
    ui.c.unclip();

    // A new desktop, pinned to the right of the strip.
    let new_r = Rect::new(area.right() - new_w, area.y, new_w, card_h);
    let new_resp = ui.button(wid("desk.new"), new_r);
    let new_fill = if new_resp.pressed { Color::white(0.12) } else if new_resp.hovered { Color::white(0.09) } else { Color::white(0.04) };
    ui.c.fill_rrect(new_r, CARD_R, new_fill);
    ui.c.stroke_rrect(new_r, CARD_R, Color::white(if new_resp.hovered { 0.24 } else { 0.12 }), 1.0);
    ui.c.icon(Icon::Plus, Rect::from_center(new_r.center(), 18.0, 18.0).offset(0.0, -7.0), palette::TEXT_DIM);
    ui.c.text("New", Rect::new(new_r.x, new_r.bottom() - 26.0, new_r.w, 16.0), &TextStyle::caption().color(palette::TEXT_FAINT).center());
    if new_resp.clicked && dragging.is_none() {
        st.desktops.loading = true;
        st.cmd(Command::Desktop(DesktopCmd::New));
    }

    // Horizontal scrollbar, only when there is more than fits.
    if max_scroll > 0.0 {
        let track = Rect::new(strip.x, strip.bottom() + 3.0, strip.w, 2.0);
        ui.c.fill_rrect(track, 1.0, Color::white(0.08));
        let frac = strip.w / total;
        let bar_w = (track.w * frac).max(24.0);
        let t = st.ui.desktop_scroll / max_scroll;
        ui.c.fill_rrect(Rect::new(track.x + (track.w - bar_w) * t, track.y, bar_w, 2.0), 1.0, Color::white(0.24));
    }

    draw_apps(ui, st, area, strip.bottom() + LIST_GAP, dragging.as_ref());
    draw_drag_ghost(ui, st, dragging.as_ref());

    // A release ends a drag: the card under the pointer (if it is another
    // desktop) gets the window.
    if let Some((_, p)) = ui.input.released {
        if let Some(drag) = st.ui.desktop_drag.take() {
            let target = cards.iter().find(|(id, r)| *id != drag.from && strip.contains(p) && r.contains(p)).map(|(id, _)| id.clone());
            match target {
                Some(id) => {
                    st.desktops.moving = Some(drag.hwnd);
                    st.desktops.flash_bad = false;
                    st.desktops.flash = Some((format!("Moving {} to {}", drag.app, desktop_name(&st.desktops, &id)), st.now + 3.0));
                    st.cmd(Command::Desktop(DesktopCmd::MoveWindow { hwnd: drag.hwnd, desktop: id }));
                }
                None => st.ui.desktop_drag = None,
            }
        }
    } else if !ui.input.down {
        // Capture lost mid-drag (another window took the mouse): drop it.
        st.ui.desktop_drag = None;
    }
}

/// One desktop's card: its wallpaper (or a tint), small snapshots of what is
/// open there, its name, and a ring when it is the one in front or the drop
/// target. Clicking it switches to it.
fn draw_card(ui: &mut Ui, st: &mut AppState, i: usize, card: Rect, dragging: Option<&DesktopDrag>, scale: f32) {
    let current = st.desktops.current == i;
    let picked = st.desktops.pick == Some(i);
    let drop = dragging.is_some_and(|d| st.desktops.list[i].id != d.from && ui.hovered(card));
    let resp = ui.button(wid_n("desk.card", i as u64), card);
    if resp.hovered && dragging.is_none() {
        st.desktops.pick = Some(i);
    }
    if resp.clicked && dragging.is_none() {
        st.desktops.pick = Some(i);
        st.cmd(Command::Desktop(DesktopCmd::Switch(i)));
    }

    let preview = Rect::new(card.x + 3.0, card.y + 3.0, card.w - 6.0, card.h - LABEL_H - 4.0);
    let fill = if resp.pressed { Color::white(0.12) } else if resp.hovered { Color::white(0.09) } else { Color::white(0.05) };
    ui.c.fill_rrect(card, CARD_R, fill);

    // Wallpaper thumbnail if Windows has one (the Shelf's worker loads it);
    // otherwise a tint that differs per desktop.
    let wallpaper = st.desktops.list[i].wallpaper.clone();
    let wall = wallpaper.as_ref().and_then(|p| {
        if !st.shelf_thumbs.contains_key(p) {
            st.cmd(Command::LoadThumb(p.clone()));
        }
        st.shelf_thumbs.get(p).cloned().flatten()
    });
    match wall {
        Some(img) => {
            ui.c.image(&img, preview, 9.0);
            ui.c.fill_rrect(preview, 9.0, Color::black(0.34));
        }
        None => {
            let tint = TINTS[i % TINTS.len()];
            let h = preview.h / 3.0;
            for k in 0..3 {
                ui.c.fill_rect(Rect::new(preview.x, preview.y + k as f32 * h, preview.w, h + 1.0), tint.with_alpha(0.12 + k as f32 * 0.045));
            }
            ui.c.fill_rect(Rect::new(preview.x, preview.y + preview.h * 0.55, preview.w, preview.h * 0.45), Color::black(0.12));
        }
    }

    // Up to two windows, side by side, as small live cards.
    let wins: Vec<DesktopWindow> = st.desktops.list[i].windows.iter().filter(|w| !w.minimized).take(2).cloned().collect();
    let inner = preview.inset(9.0, 8.0);
    // An empty desktop shows just its wallpaper: a lone app icon here reads as
    // an open window, while the app list below already says "Nothing open here".
    if !wins.is_empty() {
        let n = wins.len();
        let gap = 5.0;
        let mw = if n == 1 { (inner.w * 0.68).min(inner.w) } else { ((inner.w - gap) / 2.0).max(1.0) };
        let mh = (mw * 0.63).min(inner.h);
        let y = preview.center().y - mh / 2.0 + 2.0;
        for (k, win) in wins.iter().enumerate() {
            let x = if n == 1 { preview.center().x - mw / 2.0 } else { inner.x + k as f32 * (mw + gap) };
            let r = Rect::new(snap(x, scale), snap(y, scale), mw, mh);
            let thumb = st.desktops.thumbs.get(&win.hwnd).cloned().flatten();
            let id = wid_n("desk.shot", (i as u64).wrapping_mul(1_000_003) ^ win.hwnd as u64);
            let wr = ui.interact(id, r, CursorKind::Hand);
            // A soft outline keeps a white app readable on a light wallpaper.
            ui.c.fill_rrect(r.offset(0.0, 1.5), 3.5, Color::black(0.28));
            if let Some(img) = thumb {
                ui.c.image(&img, r, 3.5);
            } else {
                ui.c.fill_rrect(r, 3.5, Color::rgba(0.10, 0.11, 0.13, 1.0));
                app_icon(ui, st, win, Rect::from_center(r.center(), 13.0, 13.0));
            }
            if wr.hovered {
                ui.c.stroke_rrect(r, 3.5, Color::white(0.65), 1.0);
            }
            if wr.clicked && dragging.is_none() {
                st.cmd(Command::Desktop(DesktopCmd::Activate { hwnd: win.hwnd, desktop: st.desktops.list[i].id.clone() }));
            }
            start_drag(ui, st, &wr, i, win);
        }
    }

    // "Now" on the desktop in front, top-left of the preview.
    if current {
        let badge = Rect::new(preview.x + 6.0, preview.y + 6.0, 34.0, 16.0);
        ui.c.fill_rrect(badge, 8.0, Color::black(0.58));
        ui.c.text("Now", badge, &TextStyle::caption().weight(Weight::Semibold).color(st.accent).center());
    }
    let count = st.desktops.list[i].windows.len();
    if count > 0 {
        let text = count.to_string();
        let tw = ui.c.measure(&text, &TextStyle::caption().weight(Weight::Semibold), 40.0).0;
        let badge = Rect::new(preview.right() - tw - 15.0, preview.y + 6.0, tw + 11.0, 16.0);
        ui.c.fill_rrect(badge, 8.0, Color::black(0.58));
        ui.c.text(&text, badge, &TextStyle::caption().weight(Weight::Semibold).color(Color::white(0.85)).center());
    }

    // Name and app count under the preview.
    let name = st.desktops.list[i].name.clone();
    let name_r = Rect::new(card.x + 4.0, preview.bottom() + 2.0, card.w - 8.0, LABEL_H - 2.0);
    let name_st = TextStyle::footnote().weight(Weight::Semibold).color(if current { palette::TEXT } else { palette::TEXT_DIM });
    ui.c.text(&name, name_r, &name_st);

    if current {
        ui.c.stroke_rrect(card, CARD_R, st.accent, 1.5);
    } else if drop {
        ui.c.stroke_rrect(card, CARD_R, st.accent, 2.0);
        ui.c.fill_rrect(preview, 9.0, st.accent.with_alpha(0.16));
    } else if resp.hovered || picked {
        ui.c.stroke_rrect(card, CARD_R, Color::white(if resp.hovered { 0.24 } else { 0.12 }), 1.0);
    }
}

/// The apps open on the picked desktop (the one under the pointer), as a
/// two-column list of draggable rows.
fn draw_apps(ui: &mut Ui, st: &mut AppState, area: Rect, top: f32, dragging: Option<&DesktopDrag>) {
    let di = st.desktops.pick.unwrap_or(st.desktops.current);
    if di >= st.desktops.list.len() {
        return;
    }
    let name = st.desktops.list[di].name.clone();
    let windows = st.desktops.list[di].windows.clone();
    let label_r = Rect::new(area.x, top, area.w, 17.0);
    ui.c.text(&format!("On {name}"), label_r, &TextStyle::footnote().weight(Weight::Semibold));

    let flash = st.desktops.flash.as_ref().filter(|(_, until)| *until > st.now).map(|(s, _)| s.clone());
    let right_st = TextStyle::caption().color(palette::TEXT_FAINT).right();
    let hint = flash.clone().unwrap_or_else(|| {
        if dragging.is_some() {
            "Drop it on a desktop above".into()
        } else if windows.is_empty() {
            "Nothing open here".into()
        } else {
            "Drag an app onto a desktop to move it".into()
        }
    });
    let hint_st = if flash.is_some() { TextStyle::caption().color(if st.desktops.flash_bad { palette::RED } else { palette::GREEN }).right() } else { right_st };
    ui.c.text(&hint, label_r, &hint_st);

    let grid = Rect::new(area.x, top + 21.0, area.w, (area.bottom() - top - 21.0).max(0.0));
    if windows.is_empty() {
        ui.c.text("Open an app on this desktop and it shows up here.", Rect::new(grid.x, grid.y + 14.0, grid.w, 18.0), &TextStyle::footnote().color(palette::TEXT_FAINT).center());
        return;
    }
    let wheel = ui.wheel_in(grid);
    let cols = 2.0;
    let col_w = (grid.w - CHIP_GAP) / cols;
    let rows = ((windows.len() as f32 / cols).ceil()).max(1.0);
    let total = rows * (ROW_H + CHIP_GAP) - CHIP_GAP;
    let max_scroll = (total - grid.h).max(0.0);
    if wheel != 0.0 && max_scroll > 0.0 {
        st.ui.desktop_app_scroll = (st.ui.desktop_app_scroll - wheel * 32.0).clamp(0.0, max_scroll);
        ui.request_repaint();
    }
    st.ui.desktop_app_scroll = st.ui.desktop_app_scroll.clamp(0.0, max_scroll);
    ui.c.clip(grid);
    for (k, win) in windows.iter().enumerate() {
        let col = k % 2;
        let row = k / 2;
        let x = grid.x + col as f32 * (col_w + CHIP_GAP);
        let y = grid.y + row as f32 * (ROW_H + CHIP_GAP) - st.ui.desktop_app_scroll;
        let r = Rect::new(x, y, col_w, ROW_H);
        if r.bottom() < grid.y || r.y > grid.bottom() {
            continue;
        }
        let id = wid_n("desk.app", (di as u64).wrapping_mul(1_000_003) ^ win.hwnd as u64);
        let resp = ui.interact(id, r, CursorKind::Hand);
        if resp.hovered {
            ui.c.fill_rrect(r, CHIP_R, palette::SURFACE);
        }
        let icon_r = Rect::new(r.x + 6.0, r.center().y - 7.5, 15.0, 15.0);
        app_icon(ui, st, win, icon_r);
        let title = if win.title.is_empty() { win.app_name.clone() } else { win.title.clone() };
        let text_r = Rect::new(r.x + 27.0, r.y, r.w - 33.0, r.h);
        let col = if win.minimized { palette::TEXT_FAINT } else { palette::TEXT };
        ui.c.text(&title, text_r, &TextStyle::footnote().color(col));
        if resp.clicked && dragging.is_none() {
            st.cmd(Command::Desktop(DesktopCmd::Activate { hwnd: win.hwnd, desktop: st.desktops.list[di].id.clone() }));
        }
        start_drag(ui, st, &resp, di, win);
    }
    ui.c.unclip();
}

/// Turn a press that has moved far enough into a drag of `win`.
fn start_drag(ui: &mut Ui, st: &mut AppState, resp: &crate::ui::Resp, di: usize, win: &DesktopWindow) {
    let Some((dx, dy)) = resp.drag else { return };
    if !resp.pressed || dx.hypot(dy) < DRAG_SLOP {
        return;
    }
    if st.ui.desktop_drag.is_none() {
        let from = st.desktops.list.get(di).map(|d| d.id.clone()).unwrap_or_default();
        let title = if win.title.is_empty() { win.app_name.clone() } else { win.title.clone() };
        st.ui.desktop_drag = Some(DesktopDrag { hwnd: win.hwnd, app: win.app_name.clone(), title, from });
    }
    ui.request_repaint();
}

/// The little capsule that follows the pointer while an app is dragged.
fn draw_drag_ghost(ui: &mut Ui, st: &mut AppState, drag: Option<&DesktopDrag>) {
    let Some(drag) = drag else { return };
    let Some(p) = ui.input.mouse else { return };
    let title = if drag.title.is_empty() { drag.app.clone() } else { drag.title.clone() };
    let st_t = TextStyle::footnote().weight(Weight::Semibold);
    let tw = ui.c.measure(&title, &st_t, 150.0).0.min(150.0);
    let r = Rect::new(p.x + 12.0, p.y + 10.0, tw + 42.0, 28.0);
    ui.c.fill_rrect(r, 9.0, Color::rgba(0.10, 0.10, 0.12, 0.97));
    ui.c.stroke_rrect(r, 9.0, st.accent.with_alpha(0.7), 1.0);
    let w = find_window(st, drag.hwnd);
    if let Some(w) = w {
        app_icon(ui, st, &w, Rect::new(r.x + 8.0, r.center().y - 7.5, 15.0, 15.0));
    } else {
        ui.c.icon(Icon::App, Rect::new(r.x + 8.0, r.center().y - 7.5, 15.0, 15.0), palette::TEXT_DIM);
    }
    ui.c.text(&title, Rect::new(r.x + 29.0, r.y, r.w - 34.0, r.h), &st_t.color(palette::TEXT));
}

fn find_window(st: &AppState, hwnd: isize) -> Option<DesktopWindow> {
    st.desktops.list.iter().flat_map(|d| d.windows.iter()).find(|w| w.hwnd == hwnd).cloned()
}

/// An app's icon from the System page's cache, asking for it the first time.
fn app_icon(ui: &mut Ui, st: &mut AppState, win: &DesktopWindow, r: Rect) {
    if !win.app.is_empty() {
        if !st.app_icons.contains_key(&win.app) {
            st.cmd(Command::LoadAppIcon(win.app.clone()));
        }
        if let Some(Some(img)) = st.app_icons.get(&win.app) {
            ui.c.image(img, r, 3.5);
            return;
        }
    }
    ui.c.icon(Icon::App, r, palette::TEXT_DIM);
}

fn desktop_name(d: &crate::state::DesktopsState, id: &str) -> String {
    d.list.iter().find(|d| d.id == id).map(|d| d.name.clone()).unwrap_or_else(|| "that desktop".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use poni_core::settings::Settings;
    use poni_core::store::Store;

    fn state() -> AppState {
        AppState::new(Settings::default(), Store::default())
    }

    #[test]
    fn a_new_island_keeps_the_page_as_tall_as_the_system_one() {
        let st = state();
        assert!(height(&st) <= crate::pages::content_height(crate::pages::PageKind::System, &st));
    }

    #[test]
    fn the_tab_is_hidden_where_windows_is_not() {
        let mut st = state();
        st.desktops.available = false;
        assert!(!crate::pages::tabs(&st).contains(&crate::pages::PageKind::Desktops));
        st.desktops.available = true;
        assert!(crate::pages::tabs(&st).contains(&crate::pages::PageKind::Desktops));
    }
}
