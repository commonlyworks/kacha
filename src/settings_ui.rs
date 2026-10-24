//! Jendela "Kacha Settings": dark mode, font Instrument Sans, digambar dengan GDI.
//!
//! - Dua kolom: Menu Bar (kiri) dan Dock (kanan). Slider + saklar.
//! - Nilai berubah saat slider digeser; disimpan dan diterapkan saat slider dilepas
//!   (dikirim ke menu bar lewat `settings::WM_APPLY`).
//! - Dibuka dari menu Apple ("Kacha Settings…") atau Ctrl+Alt+S.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::settings::{self as cfg, BAR_FIELDS, FIELDS, N};

// ---------------------------------------------------------------------------
// Warna (format 0x00BBGGRR) dan ukuran (px pada 96 DPI)
// ---------------------------------------------------------------------------

const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    (b << 16) | (g << 8) | r
}

const BG: u32 = rgb(28, 28, 30);
const TEXT: u32 = rgb(242, 242, 245);
const DIM: u32 = rgb(152, 152, 160);
const TRACK: u32 = rgb(72, 72, 78);
const LINE: u32 = rgb(54, 54, 60);
const ACCENT: u32 = rgb(10, 132, 255);
const ACCENT_HOT: u32 = rgb(64, 156, 255);
const BTN: u32 = rgb(58, 58, 64);
const BTN_HOT: u32 = rgb(76, 76, 84);
const WHITE: u32 = rgb(255, 255, 255);

const MARGIN: f32 = 24.0;
const COL_W: f32 = 290.0;
const GUTTER: f32 = 28.0;
const TITLE_H: f32 = 56.0;
const HEAD_H: f32 = 32.0;
const ROW_H: f32 = 46.0;
const FOOT_H: f32 = 64.0;
const KNOB_R: f32 = 8.0;

const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hot {
    Reset,
    Done,
}

struct Ui {
    hwnd: HWND,
    bar: HWND,
    sc: f32,
    text: HFONT,
    bold: HFONT,
    title: HFONT,
    drag: Option<usize>,
    hover: Option<Hot>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = RefCell::new(None);
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

fn px(v: f32, sc: f32) -> i32 {
    (v * sc).round() as i32
}

fn client(sc: f32) -> (i32, i32) {
    let w = 2.0 * MARGIN + 2.0 * COL_W + GUTTER;
    let h = TITLE_H + HEAD_H + BAR_FIELDS as f32 * ROW_H + FOOT_H;
    (px(w, sc), px(h, sc))
}

fn col_left(col: usize, sc: f32) -> i32 {
    px(MARGIN + col as f32 * (COL_W + GUTTER), sc)
}

fn row_rect(sc: f32, idx: usize) -> RECT {
    let (col, r) = if idx < BAR_FIELDS { (0, idx) } else { (1, idx - BAR_FIELDS) };
    let left = col_left(col, sc);
    let top = px(TITLE_H + HEAD_H + r as f32 * ROW_H, sc);
    RECT { left, top, right: left + px(COL_W, sc), bottom: top + px(ROW_H, sc) }
}

fn track_rect(sc: f32, idx: usize) -> RECT {
    let r = row_rect(sc, idx);
    let k = px(KNOB_R, sc);
    let cy = r.top + px(34.0, sc);
    RECT { left: r.left + k, top: cy - px(2.0, sc), right: r.right - k, bottom: cy + px(2.0, sc) }
}

fn toggle_rect(sc: f32, idx: usize) -> RECT {
    let r = row_rect(sc, idx);
    let top = r.top + px(4.0, sc);
    RECT { left: r.right - px(42.0, sc), top, right: r.right, bottom: top + px(22.0, sc) }
}

/// Area yang bisa diklik untuk satu field (slider: bagian bawah baris saja).
fn hit_rect(sc: f32, idx: usize) -> RECT {
    let mut r = row_rect(sc, idx);
    if !FIELDS[idx].toggle {
        r.top += px(22.0, sc);
    }
    r
}

fn btn_rect(sc: f32, done: bool) -> RECT {
    let (cw, ch) = client(sc);
    let top = ch - px(FOOT_H, sc) + px(14.0, sc);
    let bw = px(130.0, sc);
    let bh = px(34.0, sc);
    if done {
        RECT { left: cw - px(MARGIN, sc) - bw, top, right: cw - px(MARGIN, sc), bottom: top + bh }
    } else {
        RECT { left: px(MARGIN, sc), top, right: px(MARGIN, sc) + bw, bottom: top + bh }
    }
}

fn inside(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn value_at(sc: f32, idx: usize, x: i32) -> i32 {
    let t = track_rect(sc, idx);
    let f = ((x - t.left) as f32 / (t.right - t.left).max(1) as f32).clamp(0.0, 1.0);
    let fd = &FIELDS[idx];
    fd.min + (f * (fd.max - fd.min) as f32).round() as i32
}

fn xy(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam.0 & 0xFFFF) as i16 as i32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    )
}

// ---------------------------------------------------------------------------
// Gambar
// ---------------------------------------------------------------------------

unsafe fn make_font(face: &str, height_px: i32, weight: i32) -> HFONT {
    let mut lf = LOGFONTW::default();
    lf.lfHeight = -height_px;
    lf.lfWeight = weight;
    lf.lfCharSet = DEFAULT_CHARSET;
    lf.lfQuality = FONT_QUALITY(6);
    for (i, c) in face.encode_utf16().take(31).enumerate() {
        lf.lfFaceName[i] = c;
    }
    CreateFontIndirectW(&lf)
}

unsafe fn fill(hdc: HDC, r: RECT, color: u32) {
    let b = CreateSolidBrush(COLORREF(color));
    FillRect(hdc, &r, b);
    let _ = DeleteObject(b);
}

/// Bentuk membulat dengan anti-aliasing (GDI RoundRect/Ellipse bergerigi).
/// Piksel tepi dicampur dengan isi DC saat ini, jadi cocok di atas latar apa pun.
unsafe fn aa_shape(hdc: HDC, r: RECT, radius: i32, color: u32) {
    let (l, t) = (r.left, r.top);
    let (w, h) = (r.right + 1 - l, r.bottom + 1 - t);
    if w <= 0 || h <= 0 {
        return;
    }
    let rad = radius.min(w / 2).min(h / 2).max(0) as f32;
    let (hx, hy) = (w as f32 / 2.0, h as f32 / 2.0);
    let (fr, fg, fb) = (
        (color & 0xFF) as f32,
        ((color >> 8) & 0xFF) as f32,
        ((color >> 16) & 0xFF) as f32,
    );
    for y in 0..h {
        let py = y as f32 + 0.5 - hy;
        let mut run: Option<i32> = None;
        for x in 0..=w {
            let cov = if x < w {
                let px = x as f32 + 0.5 - hx;
                let qx = px.abs() - (hx - rad);
                let qy = py.abs() - (hy - rad);
                let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt()
                    + qx.max(qy).min(0.0)
                    - rad;
                (0.5 - d).clamp(0.0, 1.0)
            } else {
                0.0
            };
            if cov >= 0.999 {
                if run.is_none() {
                    run = Some(x);
                }
                continue;
            }
            if let Some(s) = run.take() {
                fill(hdc, RECT { left: l + s, top: t + y, right: l + x, bottom: t + y + 1 }, color);
            }
            if cov > 0.0 {
                let bg = GetPixel(hdc, l + x, t + y).0;
                if bg == 0xFFFF_FFFF {
                    continue; // di luar bitmap
                }
                let (br, bgc, bb) = (
                    (bg & 0xFF) as f32,
                    ((bg >> 8) & 0xFF) as f32,
                    ((bg >> 16) & 0xFF) as f32,
                );
                let mix = |b: f32, f: f32| (b + (f - b) * cov).round().clamp(0.0, 255.0) as u32;
                let out = mix(br, fr) | (mix(bgc, fg) << 8) | (mix(bb, fb) << 16);
                let _ = SetPixelV(hdc, l + x, t + y, COLORREF(out));
            }
        }
    }
}

unsafe fn rround(hdc: HDC, r: RECT, radius: i32, color: u32) {
    aa_shape(hdc, r, radius, color);
}

unsafe fn disc(hdc: HDC, cx: i32, cy: i32, rad: i32, color: u32) {
    aa_shape(
        hdc,
        RECT { left: cx - rad, top: cy - rad, right: cx + rad, bottom: cy + rad },
        rad,
        color,
    );
}

unsafe fn text(hdc: HDC, font: HFONT, s: &str, r: RECT, color: u32, flags: DRAW_TEXT_FORMAT) {
    SelectObject(hdc, font);
    SetTextColor(hdc, COLORREF(color));
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    let mut rc = r;
    DrawTextW(hdc, &mut buf, &mut rc, flags | DT_SINGLELINE | DT_NOPREFIX);
}

unsafe fn draw(hdc: HDC, u: &Ui, w: i32) {
    let sc = u.sc;
    let vals = cfg::get();

    // Judul
    text(
        hdc,
        u.title,
        "Kacha Settings",
        RECT { left: px(MARGIN, sc), top: 0, right: w, bottom: px(TITLE_H, sc) },
        TEXT,
        DT_LEFT | DT_VCENTER,
    );

    // Header kolom
    for (col, name) in ["MENU BAR", "DOCK"].iter().enumerate() {
        let x = col_left(col, sc);
        let r = RECT {
            left: x,
            top: px(TITLE_H, sc),
            right: x + px(COL_W, sc),
            bottom: px(TITLE_H + HEAD_H, sc),
        };
        text(hdc, u.bold, name, r, DIM, DT_LEFT | DT_VCENTER);
        fill(hdc, RECT { top: r.bottom - 1, ..r }, LINE);
    }

    // Baris pengaturan
    for i in 0..N {
        let f = &FIELDS[i];
        let r = row_rect(sc, i);
        let v = vals[i];
        let label = RECT { left: r.left, top: r.top + px(4.0, sc), right: r.right, bottom: r.top + px(26.0, sc) };
        text(hdc, u.text, f.label, label, TEXT, DT_LEFT | DT_VCENTER);

        if f.toggle {
            let sw = toggle_rect(sc, i);
            let on = v != 0;
            rround(hdc, sw, (sw.bottom - sw.top) / 2, if on { ACCENT } else { TRACK });
            let kr = (sw.bottom - sw.top) / 2 - px(2.0, sc);
            let cx = if on { sw.right - kr - px(2.0, sc) } else { sw.left + kr + px(2.0, sc) };
            disc(hdc, cx, (sw.top + sw.bottom) / 2, kr, WHITE);
        } else {
            let color = if u.drag == Some(i) { ACCENT } else { DIM };
            text(hdc, u.text, &format!("{}{}", v, f.unit), label, color, DT_RIGHT | DT_VCENTER);

            let t = track_rect(sc, i);
            rround(hdc, t, px(2.0, sc), TRACK);
            let frac = (v - f.min) as f32 / (f.max - f.min).max(1) as f32;
            let kx = t.left + (frac * (t.right - t.left) as f32).round() as i32;
            if kx > t.left {
                rround(hdc, RECT { right: kx, ..t }, px(2.0, sc), ACCENT);
            }
            disc(hdc, kx, (t.top + t.bottom) / 2, px(KNOB_R, sc), WHITE);
        }
    }

    // Tombol
    let reset = btn_rect(sc, false);
    rround(hdc, reset, px(8.0, sc), if u.hover == Some(Hot::Reset) { BTN_HOT } else { BTN });
    text(hdc, u.text, "Reset to defaults", reset, TEXT, DT_CENTER | DT_VCENTER);

    let done = btn_rect(sc, true);
    rround(hdc, done, px(8.0, sc), if u.hover == Some(Hot::Done) { ACCENT_HOT } else { ACCENT });
    text(hdc, u.bold, "Done", done, WHITE, DT_CENTER | DT_VCENTER);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);

    if w > 0 && h > 0 {
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp);

        fill(mem, rc, BG);
        SetBkMode(mem, TRANSPARENT);
        UI.with(|c| {
            if let Some(u) = c.borrow().as_ref() {
                draw(mem, u, w);
            }
        });

        let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
    }

    let _ = EndPaint(hwnd, &ps);
}

// ---------------------------------------------------------------------------
// Aksi
// ---------------------------------------------------------------------------

/// Simpan ke disk dan minta menu bar menerapkan pengaturan.
unsafe fn commit(bar: HWND) {
    cfg::save();
    let _ = PostMessageW(bar, cfg::WM_APPLY, WPARAM(0), LPARAM(0));
}

unsafe fn on_down(hwnd: HWND, x: i32, y: i32) {
    let Some((sc, bar)) = UI.with(|c| c.borrow().as_ref().map(|u| (u.sc, u.bar))) else {
        return;
    };

    if inside(&btn_rect(sc, true), x, y) {
        let _ = DestroyWindow(hwnd);
        return;
    }
    if inside(&btn_rect(sc, false), x, y) {
        cfg::reset();
        commit(bar);
        let _ = InvalidateRect(hwnd, None, false);
        return;
    }

    for i in 0..N {
        if !inside(&hit_rect(sc, i), x, y) {
            continue;
        }
        if FIELDS[i].toggle {
            cfg::set(i, 1 - cfg::val(i));
            commit(bar);
        } else {
            cfg::set(i, value_at(sc, i, x));
            UI.with(|c| {
                if let Some(u) = c.borrow_mut().as_mut() {
                    u.drag = Some(i);
                }
            });
            SetCapture(hwnd);
        }
        let _ = InvalidateRect(hwnd, None, false);
        return;
    }
}

unsafe fn on_move(hwnd: HWND, x: i32, y: i32) {
    let Some((sc, drag, old_hover)) =
        UI.with(|c| c.borrow().as_ref().map(|u| (u.sc, u.drag, u.hover)))
    else {
        return;
    };

    if let Some(i) = drag {
        let nv = value_at(sc, i, x);
        if nv != cfg::val(i) {
            cfg::set(i, nv);
            let _ = InvalidateRect(hwnd, None, false);
        }
        return;
    }

    let hover = if inside(&btn_rect(sc, false), x, y) {
        Some(Hot::Reset)
    } else if inside(&btn_rect(sc, true), x, y) {
        Some(Hot::Done)
    } else {
        None
    };
    if hover != old_hover {
        UI.with(|c| {
            if let Some(u) = c.borrow_mut().as_mut() {
                u.hover = hover;
            }
        });
        let _ = InvalidateRect(hwnd, None, false);
    }
    let mut tme = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    let _ = TrackMouseEvent(&mut tme);
}

unsafe fn on_up(hwnd: HWND) {
    let (drag, bar) = UI.with(|c| match c.borrow_mut().as_mut() {
        Some(u) => (u.drag.take(), u.bar),
        None => (None, HWND::default()),
    });
    if drag.is_some() {
        let _ = ReleaseCapture();
        commit(bar);
        let _ = InvalidateRect(hwnd, None, false);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_LBUTTONDOWN => {
            let (x, y) = xy(lparam);
            on_down(hwnd, x, y);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lparam);
            on_move(hwnd, x, y);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            on_up(hwnd);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let changed = UI.with(|c| match c.borrow_mut().as_mut() {
                Some(u) => u.hover.take().is_some(),
                None => false,
            });
            if changed {
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 == VK_ESCAPE.0 as usize {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let ui = UI.with(|c| c.borrow_mut().take());
            if let Some(u) = ui {
                let _ = DeleteObject(u.text);
                let _ = DeleteObject(u.bold);
                let _ = DeleteObject(u.title);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Buka jendela Settings (kalau sudah terbuka, bawa ke depan). `bar` = jendela menu bar.
pub unsafe fn show(bar: HWND) {
    let existing = UI.with(|c| c.borrow().as_ref().map(|u| u.hwnd));
    if let Some(h) = existing {
        let _ = ShowWindow(h, SW_SHOW);
        let _ = SetForegroundWindow(h);
        return;
    }

    let Ok(hmodule) = GetModuleHandleW(PCWSTR::null()) else {
        return;
    };
    let hinstance: HINSTANCE = hmodule.into();
    let sc = GetDpiForSystem() as f32 / 96.0;

    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: w!("KachaSettings"),
        hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&wc);

    let Ok(hwnd) = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        w!("KachaSettings"),
        w!("Kacha Settings"),
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
        0,
        0,
        100,
        100,
        HWND::default(),
        HMENU::default(),
        hinstance,
        None,
    ) else {
        return;
    };

    // Title bar gelap.
    let dark: i32 = 1;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &dark as *const i32 as *const c_void,
        size_of::<i32>() as u32,
    );

    // Ukuran jendela = area klien yang kita mau + bingkai, di tengah layar.
    let (cw, ch) = client(sc);
    let mut wr = RECT::default();
    let mut cr = RECT::default();
    let _ = GetWindowRect(hwnd, &mut wr);
    let _ = GetClientRect(hwnd, &mut cr);
    let ow = (wr.right - wr.left) - (cr.right - cr.left) + cw;
    let oh = (wr.bottom - wr.top) - (cr.bottom - cr.top) + ch;
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let _ = SetWindowPos(
        hwnd,
        HWND::default(),
        ((sw - ow) / 2).max(0),
        ((sh - oh) / 2).max(0),
        ow,
        oh,
        SWP_NOZORDER | SWP_NOACTIVATE,
    );

    // Ikon title bar = logo Kacha (icon.ico, resource ID 1), sama seperti di menu bar.
    for (which, size) in [(0usize, GetSystemMetrics(SM_CXSMICON)), (1usize, GetSystemMetrics(SM_CXICON))] {
        if let Ok(ic) = LoadImageW(
            hinstance,
            PCWSTR(1usize as *const u16),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR | LR_SHARED,
        ) {
            let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(which), LPARAM(ic.0 as isize));
        }
    }

    let face = crate::fonts::FACE;
    UI.with(|c| {
        *c.borrow_mut() = Some(Ui {
            hwnd,
            bar,
            sc,
            text: make_font(face, px(13.0, sc), 400),
            bold: make_font(face, px(13.0, sc), 700),
            title: make_font(face, px(20.0, sc), 700),
            drag: None,
            hover: None,
        });
    });

    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
}
