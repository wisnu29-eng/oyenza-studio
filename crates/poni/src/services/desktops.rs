//! Windows' virtual desktops, for the Desktops page: what desktops there are,
//! which app windows live on each, a small snapshot of those windows, switching
//! between desktops with the mouse, and moving a window to another desktop.
//!
//! The worker exists only while the Desktops page is open (`App::set_desktops`),
//! so nothing here runs at rest (AGENTS.md, Keep it light).
//!
//! Switching desktops has no public API: on Windows 11 24H2+ the worker asks
//! `winvd`'s internal manager to switch to the chosen desktop in one go, and
//! older builds fall back to Windows' own Win+Ctrl+Left/Right (SendInput).
//! Either way it reads the desktop registry back to confirm. Documented Win32
//! does the rest: `IVirtualDesktopManager` reads a window's desktop,
//! EnumWindows lists the windows, and PrintWindow takes the snapshots (apps
//! that can't be captured show their icon instead). The documented move only
//! works on windows this process owns, so moving another app also uses
//! `winvd` and says so on older builds. The registry gives the desktops'
//! order, which the arrow keys follow.

use crate::commands::DesktopCmd;
use crate::events::{AppEvent, DesktopEvent, EventSink};
use crate::gfx::{Image, ImageData};
use crate::state::{DesktopWindow, VirtualDesktop};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, REG_VALUE_TYPE, RRF_RT_REG_BINARY, RRF_RT_REG_SZ};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_LEFT,
    VK_RIGHT,
};
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsHungAppWindow,
    IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow, SwitchToThisWindow, GWL_EXSTYLE, PW_RENDERFULLCONTENT, SW_RESTORE,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

const REG_DESKTOPS: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VirtualDesktops";
/// Per-desktop wallpaper and (on Windows 11) the name the person gave it.
const REG_DESKTOP: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\VirtualDesktops\\Desktops";
const REG_DESKTOP_WALLPAPER: &str = "Control Panel\\Desktop";

/// Widest snapshot kept per window: the page draws them at about 70 DIPs.
const SHOT_W: u32 = 320;
const SHOT_H: u32 = 200;
/// Windows captured per desktop: the page previews at most two, with one spare.
const SHOTS_PER_DESKTOP: usize = 3;
/// How often the open page notices a switch made with the keyboard or a
/// touchpad. Only while the page is open; the worker is gone otherwise.
const WATCH: Duration = Duration::from_millis(1500);
/// Key presses that switch a desktop need a moment between them.
const KEY_GAP: Duration = Duration::from_millis(70);
/// How long a switch may take to show in the registry.
const SWITCH_WAIT: Duration = Duration::from_millis(1200);

pub struct Desktops {
    tx: Sender<DesktopCmd>,
    stop: Arc<AtomicBool>,
}

impl Desktops {
    pub fn start(sink: EventSink) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        // A failure to start the thread leaves the page with its error, never the app.
        if let Err(e) = thread::Builder::new().name("desktops".into()).spawn(move || worker(rx, sink, worker_stop)) {
            crate::pwarn!("desktops: couldn't start its thread ({e})");
        }
        Self { tx, stop }
    }

    pub fn send(&self, c: DesktopCmd) {
        let _ = self.tx.send(c);
    }
}

impl Drop for Desktops {
    fn drop(&mut self) {
        // The worker checks this between windows; joining could wait on an app
        // that doesn't answer PrintWindow, so the UI thread never does.
        self.stop.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------- helpers

/// Balanced CoInitializeEx for the worker thread.
struct ComInit(bool);

impl ComInit {
    fn new() -> Self {
        ComInit(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for ComInit {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn reg_bytes(subkey: &str, value: &str) -> Option<Vec<u8>> {
    let (sub, val) = (wide(subkey), wide(value));
    let mut ty = REG_VALUE_TYPE::default();
    let mut len = 0u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_BINARY,
            Some(&mut ty),
            None,
            Some(&mut len),
        )
    };
    if r.is_err() || len == 0 || len > 4096 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_BINARY,
            Some(&mut ty),
            Some(buf.as_mut_ptr().cast::<c_void>()),
            Some(&mut len),
        )
    };
    if r.is_err() {
        return None;
    }
    buf.truncate(len as usize);
    Some(buf)
}

fn reg_string(subkey: &str, value: &str) -> Option<String> {
    let (sub, val) = (wide(subkey), wide(value));
    let mut ty = REG_VALUE_TYPE::default();
    let mut len = 0u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_SZ,
            Some(&mut ty),
            None,
            Some(&mut len),
        )
    };
    if r.is_err() || len == 0 || len > 8192 {
        return None;
    }
    let mut buf = vec![0u16; (len as usize).div_ceil(2).max(1)];
    let mut bytes = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_SZ,
            Some(&mut ty),
            Some(buf.as_mut_ptr().cast::<c_void>()),
            Some(&mut bytes),
        )
    };
    if r.is_err() {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..end]).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn guid_from_bytes(b: &[u8]) -> GUID {
    let mut d4 = [0u8; 8];
    d4.copy_from_slice(&b[8..16]);
    GUID {
        data1: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        data2: u16::from_le_bytes([b[4], b[5]]),
        data3: u16::from_le_bytes([b[6], b[7]]),
        data4: d4,
    }
}

fn guid_string(g: &GUID) -> String {
    let d = g.data4;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    )
}

fn parse_guid(s: &str) -> Option<GUID> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let mut d4 = [0u8; 8];
    for (i, b) in d4.iter_mut().enumerate() {
        *b = byte(16 + i * 2)?;
    }
    Some(GUID {
        data1: u32::from_str_radix(&hex[0..8], 16).ok()?,
        data2: u16::from_str_radix(&hex[8..12], 16).ok()?,
        data3: u16::from_str_radix(&hex[12..16], 16).ok()?,
        data4: d4,
    })
}

/// The desktops in the order Win+Ctrl+Left/Right visits them.
fn read_ids() -> Option<Vec<GUID>> {
    let b = reg_bytes(REG_DESKTOPS, "VirtualDesktopIDs")?;
    if b.len() < 16 || b.len() % 16 != 0 {
        return None;
    }
    Some(b.chunks_exact(16).map(guid_from_bytes).collect())
}

fn read_current() -> Option<String> {
    let b = reg_bytes(REG_DESKTOPS, "CurrentVirtualDesktop")?;
    (b.len() >= 16).then(|| guid_string(&guid_from_bytes(&b[..16])))
}

fn window_text(hwnd: HWND) -> String {
    let n = unsafe { GetWindowTextLengthW(hwnd) };
    if n <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; n as usize + 1];
    let got = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..got.max(0) as usize]).trim().to_string()
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn process_path(pid: u32) -> Option<String> {
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.is_ok();
    unsafe {
        let _ = CloseHandle(h);
    }
    if !ok || len == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

fn app_label(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file);
    if stem.is_empty() {
        "App".into()
    } else {
        stem.to_string()
    }
}

/// A window worth showing: top-level, visible, not a tray/tool helper, not ours.
fn window_info(hwnd: HWND, vdm: &IVirtualDesktopManager, own_pid: u32) -> Option<(String, DesktopWindow)> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0 | WS_EX_TRANSPARENT.0) != 0 {
            return None;
        }
        if matches!(
            class_name(hwnd).as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "TaskListThumbnailWnd" | "Windows.UI.Core.CoreWindow"
        ) {
            return None;
        }
        let mut pid = 0u32;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == own_pid {
            return None;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() || r.right - r.left < 40 || r.bottom - r.top < 30 {
            return None;
        }
        let title = window_text(hwnd);
        let app = process_path(pid).unwrap_or_default();
        if title.is_empty() && app.is_empty() {
            return None;
        }
        let id = vdm.GetWindowDesktopId(hwnd).ok()?;
        let minimized = IsIconic(hwnd).as_bool();
        Some((
            guid_string(&id),
            DesktopWindow { hwnd: hwnd.0 as isize, title, app_name: app_label(&app), app, minimized },
        ))
    }
}

#[allow(clippy::type_complexity)]
unsafe extern "system" fn enum_window(hwnd: HWND, lp: LPARAM) -> BOOL {
    let out = &mut *(lp.0 as *mut Vec<HWND>);
    out.push(hwnd);
    TRUE
}

/// Read the desktops and group every window under its desktop.
fn read_desktops(vdm: Option<&IVirtualDesktopManager>) -> Result<(Vec<VirtualDesktop>, usize), String> {
    let ids = read_ids().ok_or_else(|| "Windows didn't say which desktops it has.".to_string())?;
    if ids.is_empty() {
        return Err("Windows has no virtual desktops to show.".into());
    }
    let current_id = read_current();
    let mut desktops: Vec<VirtualDesktop> = ids
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let id = guid_string(g);
            let key = format!("{REG_DESKTOP}\\{id}");
            VirtualDesktop {
                name: reg_string(&key, "Name").unwrap_or_else(|| format!("Desktop {}", i + 1)),
                wallpaper: reg_string(&key, "Wallpaper").or_else(|| reg_string(REG_DESKTOP_WALLPAPER, "Wallpaper")),
                id,
                windows: Vec::new(),
            }
        })
        .collect();

    if let Some(vdm) = vdm {
        let own_pid = std::process::id();
        let mut windows = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(enum_window), LPARAM((&mut windows as *mut Vec<HWND>) as isize));
        }
        for hwnd in windows {
            if let Some((id, w)) = window_info(hwnd, vdm, own_pid) {
                if let Some(d) = desktops.iter_mut().find(|d| d.id == id) {
                    d.windows.push(w);
                }
            }
        }
        for d in &mut desktops {
            // Windows lists windows in z-order; minimized ones go last so the
            // preview picks what is actually on screen.
            d.windows.sort_by_key(|w| w.minimized);
        }
    }

    // The registry is the quickest answer; a window on the current desktop is
    // the fallback when it is missing.
    let current = current_id
        .as_deref()
        .and_then(|id| desktops.iter().position(|d| d.id == id))
        .or_else(|| {
            vdm.and_then(|_| {
                desktops.iter().position(|d| {
                    d.windows.iter().any(|w| {
                        vdm.is_some_and(|v| unsafe {
                            v.IsWindowOnCurrentVirtualDesktop(HWND(w.hwnd as *mut c_void)).map(|b| b.as_bool()).unwrap_or(false)
                        })
                    })
                })
            })
        })
        .unwrap_or(0);
    Ok((desktops, current))
}

// ---------------------------------------------------------------- snapshots

/// A window snapshot: PrintWindow into a 32-bit DIB, then a box filter down to
/// [`SHOT_W`]. None when Windows (or the app) won't draw it; its icon shows.
fn capture(hwnd: isize) -> Option<Image> {
    let hwnd = HWND(hwnd as *mut c_void);
    unsafe {
        if IsIconic(hwnd).as_bool() || IsHungAppWindow(hwnd).as_bool() {
            return None;
        }
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).ok()?;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w < 40 || h < 30 {
            return None;
        }
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(screen);
        if mem.is_invalid() {
            ReleaseDC(None, screen);
            return None;
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bmp: HBITMAP = match CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(b) if !bits.is_null() => b,
            _ => {
                let _ = DeleteDC(mem);
                ReleaseDC(None, screen);
                return None;
            }
        };
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        let drew = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)).as_bool();
        SelectObject(mem, old);
        let mut image = None;
        if drew {
            let src_w = w as usize;
            let src_h = h as usize;
            let dst_w = SHOT_W.min(src_w as u32).max(1) as usize;
            let dst_h = ((src_h as f32 * dst_w as f32 / src_w as f32).round() as u32).clamp(1, SHOT_H) as usize;
            let src = std::slice::from_raw_parts(bits as *const u8, src_w * src_h * 4);
            let mut pixels = vec![0u8; dst_w * dst_h * 4];
            let mut bright = 0u32;
            for y in 0..dst_h {
                let sy0 = y * src_h / dst_h;
                let sy1 = (((y + 1) * src_h / dst_h).max(sy0 + 1)).min(src_h);
                for x in 0..dst_w {
                    let sx0 = x * src_w / dst_w;
                    let sx1 = (((x + 1) * src_w / dst_w).max(sx0 + 1)).min(src_w);
                    let (mut b, mut g, mut rr, mut n) = (0u32, 0u32, 0u32, 0u32);
                    for sy in sy0..sy1 {
                        for sx in sx0..sx1 {
                            let o = (sy * src_w + sx) * 4;
                            b += src[o] as u32;
                            g += src[o + 1] as u32;
                            rr += src[o + 2] as u32;
                            n += 1;
                        }
                    }
                    let n = n.max(1);
                    let o = (y * dst_w + x) * 4;
                    pixels[o] = (b / n) as u8;
                    pixels[o + 1] = (g / n) as u8;
                    pixels[o + 2] = (rr / n) as u8;
                    pixels[o + 3] = 255;
                    if (b / n).max(g / n).max(rr / n) > 10 {
                        bright += 1;
                    }
                }
            }
            // A full black frame is a failed PrintWindow, not a black app.
            if bright > (dst_w * dst_h) as u32 / 100 {
                image = Some(ImageData::new(dst_w as u32, dst_h as u32, pixels));
            }
        }
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        image
    }
}

// ---------------------------------------------------------------- switching

fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn send_keys(keys: &[INPUT]) {
    unsafe {
        SendInput(keys, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Win+Ctrl+`vk` (Left/Right switches, D adds a desktop).
fn win_ctrl(vk: VIRTUAL_KEY) {
    let keys = [
        key_input(VK_LWIN, false),
        key_input(VK_CONTROL, false),
        key_input(vk, false),
        key_input(vk, true),
        key_input(VK_CONTROL, true),
        key_input(VK_LWIN, true),
    ];
    send_keys(&keys);
}

fn switch_by(delta: isize) {
    let (vk, n) = if delta > 0 { (VK_RIGHT, delta) } else { (VK_LEFT, -delta) };
    for i in 0..n {
        if i > 0 {
            thread::sleep(KEY_GAP);
        }
        win_ctrl(vk);
    }
}

/// Index of the desktop in front, read from the registry.
fn current_index() -> Option<usize> {
    let ids = read_ids()?;
    let cur = read_current()?;
    ids.iter().position(|g| guid_string(g) == cur)
}

/// Switch straight to `target` (or the wait runs out). Windows' internal
/// manager (Windows 11 24H2+, `winvd`) jumps there in one go; pressing
/// Win+Ctrl+Left/Right once per desktop would walk through every desktop in
/// between, showing each one on the way. The keys remain the fallback for
/// older builds, where the internal manager isn't there.
fn switch_to(target: usize) -> Option<usize> {
    let ids = read_ids()?;
    let cur = read_current()?;
    let start = ids.iter().position(|g| guid_string(g) == cur)?;
    if start == target {
        return Some(start);
    }
    let direct = ids.get(target).is_some_and(|id| winvd::switch_desktop(*id).is_ok());
    if !direct {
        switch_by(target as isize - start as isize);
    }
    let until = Instant::now() + SWITCH_WAIT;
    while Instant::now() < until {
        thread::sleep(Duration::from_millis(40));
        if current_index() == Some(target) {
            return Some(target);
        }
    }
    current_index()
}

fn activate(hwnd: isize) {
    let h = HWND(hwnd as *mut c_void);
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
        SwitchToThisWindow(h, false);
    }
}

/// Move any app's window to the desktop `desktop` (its GUID). The documented
/// `IVirtualDesktopManager` refuses windows this process doesn't own, so use
/// Windows 11 24H2+'s internal manager for those (`winvd`), and keep the
/// documented call as the fallback for our own windows on older builds.
fn move_window(vdm: Option<&IVirtualDesktopManager>, hwnd: isize, desktop: &str) -> Result<(), String> {
    let id = parse_guid(desktop).ok_or_else(|| "That desktop is gone.".to_string())?;
    let h = HWND(hwnd as *mut c_void);
    match winvd::move_window_to_desktop(id, &h) {
        Ok(()) => Ok(()),
        Err(e) => {
            let mut pid = 0u32;
            unsafe {
                let _ = GetWindowThreadProcessId(h, Some(&mut pid));
            }
            if pid != 0 && pid == std::process::id() {
                if let Some(vdm) = vdm {
                    if unsafe { vdm.MoveWindowToDesktop(h, &id) }.is_ok() {
                        return Ok(());
                    }
                }
            }
            crate::pwarn!("desktops: couldn't move {hwnd:#x}: {e:?}");
            let build = crate::window::windows_build().unwrap_or(0);
            Err(if build < 26_100 {
                "Moving other apps needs Windows 11 24H2 or later.".into()
            } else {
                "Windows wouldn't move this app.".into()
            })
        }
    }
}

// ---------------------------------------------------------------- worker

fn send(sink: &EventSink, ev: DesktopEvent) {
    sink.send(AppEvent::Desktops(ev));
}

fn refresh(sink: &EventSink, vdm: Option<&IVirtualDesktopManager>, stop: &AtomicBool) {
    let (desktops, current) = match read_desktops(vdm) {
        Ok(v) => v,
        Err(e) => {
            send(sink, DesktopEvent::Failed(e));
            return;
        }
    };
    send(sink, DesktopEvent::Snapshot { desktops: desktops.clone(), current });
    for d in &desktops {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        for w in d.windows.iter().filter(|w| !w.minimized).take(SHOTS_PER_DESKTOP) {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let image = capture(w.hwnd);
            // The page may have closed while PrintWindow drew: don't put the
            // snapshot back into a cache it just freed.
            if stop.load(Ordering::Relaxed) {
                return;
            }
            send(sink, DesktopEvent::Thumb { hwnd: w.hwnd, image });
        }
    }
}

fn worker(rx: Receiver<DesktopCmd>, sink: EventSink, stop: Arc<AtomicBool>) {
    let _com = ComInit::new();
    let vdm: Option<IVirtualDesktopManager> = unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_INPROC_SERVER).ok() };
    if vdm.is_none() {
        crate::pwarn!("desktops: virtual desktop manager unavailable");
    }
    let mut last_current = None;
    // The registry poll (below) sleeps while nobody is at the PC or battery
    // saver is on; commands like a click still arrive and run.
    let mut watch = true;
    loop {
        let job = if stop.load(Ordering::Relaxed) {
            return;
        } else if watch {
            rx.recv_timeout(WATCH)
        } else {
            match rx.recv() {
                Ok(c) => Ok(c),
                Err(_) => Err(RecvTimeoutError::Disconnected),
            }
        };
        match job {
            Ok(DesktopCmd::Refresh) => {
                refresh(&sink, vdm.as_ref(), &stop);
                last_current = current_index();
            }
            Ok(DesktopCmd::Switch(i)) => {
                if let Some(cur) = switch_to(i) {
                    last_current = Some(cur);
                    send(&sink, DesktopEvent::Current(cur));
                } else {
                    send(&sink, DesktopEvent::Failed("Couldn't switch desktops.".into()));
                }
            }
            Ok(DesktopCmd::Activate { hwnd, desktop }) => {
                if let Some(ids) = read_ids() {
                    if let Some(i) = ids.iter().position(|g| guid_string(g) == desktop) {
                        switch_to(i);
                    }
                }
                thread::sleep(Duration::from_millis(120));
                activate(hwnd);
                last_current = current_index();
                if let Some(cur) = last_current {
                    send(&sink, DesktopEvent::Current(cur));
                }
            }
            Ok(DesktopCmd::MoveWindow { hwnd, desktop }) => {
                let (ok, why) = match move_window(vdm.as_ref(), hwnd, &desktop) {
                    Ok(()) => {
                        thread::sleep(Duration::from_millis(80));
                        (true, String::new())
                    }
                    Err(e) => (false, e),
                };
                send(&sink, DesktopEvent::Moved { hwnd, desktop, ok, why });
                if ok {
                    refresh(&sink, vdm.as_ref(), &stop);
                }
            }
            Ok(DesktopCmd::New) => {
                // Win+Ctrl+D is a key press, not an answer: wait for the
                // registry to list one more desktop before reading it all.
                let before = read_ids().map(|v| v.len());
                win_ctrl(VIRTUAL_DESK_VK);
                let until = Instant::now() + SWITCH_WAIT;
                while Instant::now() < until {
                    thread::sleep(Duration::from_millis(60));
                    if read_ids().map(|v| v.len()) != before {
                        break;
                    }
                }
                refresh(&sink, vdm.as_ref(), &stop);
                last_current = current_index();
            }
            Ok(DesktopCmd::Rest(away)) => watch = !away,
            Err(RecvTimeoutError::Timeout) => {
                // Someone switched with the keyboard or a touchpad while the
                // page was open: follow them. Nothing runs once it closes.
                let cur = current_index();
                if cur != last_current {
                    last_current = cur;
                    if let Some(cur) = cur {
                        send(&sink, DesktopEvent::Current(cur));
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Win+Ctrl+D: a new desktop after the last.
const VIRTUAL_DESK_VK: VIRTUAL_KEY = VIRTUAL_KEY(0x44);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_round_trip_through_their_strings() {
        let g = GUID { data1: 0x0AA04317, data2: 0x3D37, data3: 0x402D, data4: [0xA4, 0xEB, 0x4B, 0x77, 0x84, 0x9D, 0x32, 0x31] };
        let s = guid_string(&g);
        assert_eq!(s, "{0AA04317-3D37-402D-A4EB-4B77849D3231}");
        assert_eq!(parse_guid(&s), Some(g));
        assert_eq!(parse_guid("nonsense"), None);
    }

    #[test]
    fn the_registry_blob_is_a_list_of_guids() {
        // Two desktops in the order the arrow keys visit them.
        let mut b = Vec::new();
        b.extend_from_slice(&0x0AA04317u32.to_le_bytes());
        b.extend_from_slice(&0x3D37u16.to_le_bytes());
        b.extend_from_slice(&0x402Du16.to_le_bytes());
        b.extend_from_slice(&[0xA4, 0xEB, 0x4B, 0x77, 0x84, 0x9D, 0x32, 0x31]);
        b.extend_from_slice(&0xF7C61E06u32.to_le_bytes());
        b.extend_from_slice(&0x44C4u16.to_le_bytes());
        b.extend_from_slice(&0xA0FBu16.to_le_bytes());
        b.extend_from_slice(&[0x80, 0x6F, 0x12, 0x2C, 0x0A, 0x52, 0x36, 0x96]);
        let ids: Vec<String> = b.chunks_exact(16).map(|c| guid_string(&guid_from_bytes(c))).collect();
        assert_eq!(ids, ["{0AA04317-3D37-402D-A4EB-4B77849D3231}", "{F7C61E06-44C4-A0FB-806F-122C0A523696}"]);
    }

    #[test]
    fn app_labels_drop_the_folder_and_the_extension() {
        assert_eq!(app_label("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"), "chrome");
        assert_eq!(app_label("C:\\Windows\\explorer.EXE"), "explorer");
        assert_eq!(app_label(""), "App");
    }
}
