//! Dialog konfirmasi Restart / Shut Down, gaya sama dengan jendela About (about.rs).
//!
//! - Popup acrylic bersudut membulat, tanpa title bar, tombol tutup bulat merah di kiri atas.
//! - Logo di atas, judul, pesan, garis pemisah, lalu tombol Cancel + aksi.
//! - Esc / tombol merah / Cancel = batal. Aksi menjalankan shutdown.exe seperti sebelumnya.
//! - Bisa digeser dengan menarik bagian mana saja dari jendela.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::about::{
    close_rect, ellipse, in_rect, make_font, px, rc, text, DIM, FAINT, LINE, LOGO, RADIUS, RED,
    RED_DARK, TINT, WHITE,
};

// Ukuran & layout (px pada 96 DPI), lebar sama dengan About.
const WIN_W: f32 = 280.0;
const WIN_H: f32 = 300.0;
const BTN_Y: f32 = 244.0;
const BTN_H: f32 = 34.0;
const BTN_W: f32 = 112.0;
const BTN_GAP: f32 = 12.0;

const CANCEL_BG: u32 = 0x0046_4646;
const CANCEL_HOT: u32 = 0x0060_6060;
const ACT_HOT: u32 = 0x0060_5FFF; // merah lebih terang saat hover

const WM_MOUSELEAVE: u32 = 0x02A3;

static POWER_HWND: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Restart,
    Shutdown,
}

impl Mode {
    fn title(self) -> &'static str {
        match self {
            Mode::Restart => "Restart",
            Mode::Shutdown => "Shut Down",
        }
    }
    fn message(self) -> &'static str {
        match self {
            Mode::Restart => "Restart your PC now?",
            Mode::Shutdown => "Shut down your PC now?",
        }
    }
    fn args(self) -> &'static str {
        match self {
            Mode::Restart => "/r /t 0",
            Mode::Shutdown => "/s /t 0",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hover {
    None,
    Close,
    Cancel,
    Act,
}

struct Dialog {
    sc: f32,
    w: i32,
    h: i32,
    mode: Mode,
    logo: HICON,
    title_font: HFONT,
    body_font: HFONT,
    small_font: HFONT,
    btn_font: HFONT,
    hover: Hover,
    tracking: bool,
}

thread_local! {
    static DLG: RefCell<Option<Dialog>> = RefCell::new(None);
}

fn btn_rects(w: i32, sc: f32) -> (RECT, RECT) {
    let bw = px(BTN_W, sc);
    let gap = px(BTN_GAP, sc);
    let x0 = (w - (2 * bw + gap)) / 2;
    let y = px(BTN_Y, sc);
    let bh = px(BTN_H, sc);
    (rc(x0, y, x0 + bw, y + bh), rc(x0 + bw + gap, y, x0 + 2 * bw + gap, y + bh))
}

fn hit_test(d: &Dialog, x: i32, y: i32) -> Hover {
    let (cancel, act) = btn_rects(d.w, d.sc);
    if in_rect(&close_rect(d.sc), x, y) {
        Hover::Close
    } else if in_rect(&cancel, x, y) {
        Hover::Cancel
    } else if in_rect(&act, x, y) {
        Hover::Act
    } else {
        Hover::None
    }
}

unsafe fn button(hdc: HDC, r: RECT, radius: i32, color: u32) {
    let b = CreateSolidBrush(COLORREF(color));
    let ob = SelectObject(hdc, b);
    let op = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = RoundRect(hdc, r.left, r.top, r.right + 1, r.bottom + 1, radius * 2, radius * 2);
    SelectObject(hdc, op);
    SelectObject(hdc, ob);
    let _ = DeleteObject(b);
}

unsafe fn draw(hdc: HDC, d: &Dialog) {
    let sc = d.sc;
    let (w, h) = (d.w, d.h);

    // Hitam = "kaca" (acrylic terlihat), sama seperti About.
    FillRect(hdc, &rc(0, 0, w, h), HBRUSH(GetStockObject(BLACK_BRUSH).0));
    SetBkMode(hdc, TRANSPARENT);

    // ---- tombol tutup ----
    let cr = close_rect(sc);
    ellipse(hdc, cr, RED);
    if d.hover == Hover::Close {
        let pen = CreatePen(PS_SOLID, px(1.6, sc).max(1), COLORREF(RED_DARK));
        let op = SelectObject(hdc, pen);
        let m = px(4.0, sc);
        let _ = MoveToEx(hdc, cr.left + m, cr.top + m, None);
        let _ = LineTo(hdc, cr.right - m + 1, cr.bottom - m + 1);
        let _ = MoveToEx(hdc, cr.right - m, cr.top + m, None);
        let _ = LineTo(hdc, cr.left + m - 1, cr.bottom - m + 1);
        SelectObject(hdc, op);
        let _ = DeleteObject(pen);
    }

    // ---- logo ----
    let logo = px(LOGO, sc);
    let lx = (w - logo) / 2;
    let ly = px(40.0, sc);
    if !d.logo.is_invalid() {
        let _ = DrawIconEx(hdc, lx, ly, d.logo, logo, logo, 0, HBRUSH::default(), DI_NORMAL);
    }

    // ---- judul & pesan ----
    let name_y = ly + logo + px(14.0, sc);
    text(hdc, d.title_font, d.mode.title(), rc(0, name_y, w, name_y + px(32.0, sc)), WHITE, DT_CENTER);

    let msg_y = name_y + px(32.0, sc);
    text(hdc, d.body_font, d.mode.message(), rc(0, msg_y, w, msg_y + px(22.0, sc)), DIM, DT_CENTER);

    let small_y = msg_y + px(22.0, sc);
    text(
        hdc,
        d.small_font,
        "Unsaved work will be lost.",
        rc(0, small_y, w, small_y + px(20.0, sc)),
        FAINT,
        DT_CENTER,
    );

    // ---- garis pemisah ----
    let line_y = small_y + px(20.0, sc) + px(16.0, sc);
    let b = CreateSolidBrush(COLORREF(LINE));
    FillRect(hdc, &rc(px(36.0, sc), line_y, w - px(36.0, sc), line_y + 1), b);
    let _ = DeleteObject(b);

    // ---- tombol ----
    let (cancel, act) = btn_rects(w, sc);
    let radius = px(8.0, sc);
    button(hdc, cancel, radius, if d.hover == Hover::Cancel { CANCEL_HOT } else { CANCEL_BG });
    text(hdc, d.btn_font, "Cancel", cancel, WHITE, DT_CENTER);
    button(hdc, act, radius, if d.hover == Hover::Act { ACT_HOT } else { RED });
    text(hdc, d.btn_font, d.mode.title(), act, WHITE, DT_CENTER);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    DLG.with(|c| {
        let b = c.borrow();
        let Some(d) = b.as_ref() else {
            return;
        };
        if d.w <= 0 || d.h <= 0 {
            return;
        }
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, d.w, d.h);
        let old = SelectObject(mem, bmp);

        draw(mem, d);

        let _ = BitBlt(hdc, 0, 0, d.w, d.h, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
    });

    let _ = EndPaint(hwnd, &ps);
}

fn set_hover(hwnd: HWND, hover: Hover) {
    let changed = DLG.with(|c| {
        if let Some(d) = c.borrow_mut().as_mut() {
            if d.hover != hover {
                d.hover = hover;
                return true;
            }
        }
        false
    });
    if changed {
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}

unsafe fn cleanup() {
    DLG.with(|c| {
        if let Some(d) = c.borrow_mut().take() {
            if !d.logo.is_invalid() {
                let _ = DestroyIcon(d.logo);
            }
            for f in [d.title_font, d.body_font, d.small_font, d.btn_font] {
                if !f.is_invalid() {
                    let _ = DeleteObject(f);
                }
            }
        }
    });
    POWER_HWND.store(0, Ordering::SeqCst);
}

/// Tampilkan dialog konfirmasi. `owner` = jendela menu bar.
pub unsafe fn show(owner: HWND, mode: Mode) {
    // Kalau masih ada dialog lama, tutup dulu supaya mode-nya benar.
    let existing = POWER_HWND.load(Ordering::SeqCst);
    if existing != 0 {
        let h = HWND(existing as *mut _);
        if IsWindow(h).as_bool() {
            let _ = DestroyWindow(h);
        }
    }

    let hmodule = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
    let hinstance: HINSTANCE = hmodule.into();

    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: w!("KachaPower"),
        hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&wc); // gagal kalau sudah terdaftar: aman diabaikan

    let sc = GetDpiForSystem() as f32 / 96.0;
    let (w, h) = (px(WIN_W, sc), px(WIN_H, sc));

    // Logo yang sama dengan menu bar (icon.ico, resource ID 1).
    let logo_px = px(LOGO, sc);
    let logo = LoadImageW(
        hinstance,
        PCWSTR(1usize as *const u16),
        IMAGE_ICON,
        logo_px,
        logo_px,
        LR_DEFAULTCOLOR,
    )
    .map(|h| HICON(h.0))
    .unwrap_or_default();

    let face = crate::fonts::FACE;
    DLG.with(|c| {
        *c.borrow_mut() = Some(Dialog {
            sc,
            w,
            h,
            mode,
            logo,
            title_font: make_font(face, px(24.0, sc), 700),
            body_font: make_font(face, px(14.0, sc), 400),
            small_font: make_font(face, px(12.5, sc), 400),
            btn_font: make_font(face, px(14.0, sc), 600),
            hover: Hover::None,
            tracking: false,
        });
    });

    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let x = (sw - w) / 2;
    let y = ((sh - h) / 2 - px(20.0, sc)).max(0);

    let Ok(hwnd) = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        w!("KachaPower"),
        w!("Kacha"),
        WS_POPUP,
        x,
        y,
        w,
        h,
        owner,
        HMENU::default(),
        hinstance,
        None,
    ) else {
        cleanup();
        return;
    };
    POWER_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

    crate::bar::enable_acrylic(hwnd, TINT);
    let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, px(RADIUS, sc) * 2, px(RADIUS, sc) * 2);
    SetWindowRgn(hwnd, rgn, TRUE);

    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // Seluruh jendela bisa dipakai untuk menggeser, kecuali tombol.
        WM_NCHITTEST => {
            let mut pt = POINT {
                x: (lparam.0 & 0xFFFF) as i16 as i32,
                y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
            };
            let _ = ScreenToClient(hwnd, &mut pt);
            let on_ctrl = DLG.with(|c| {
                c.borrow()
                    .as_ref()
                    .map(|d| hit_test(d, pt.x, pt.y) != Hover::None)
                    .unwrap_or(false)
            });
            LRESULT(if on_ctrl { 1 } else { 2 }) // HTCLIENT : HTCAPTION
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let (hover, need_track) = DLG.with(|c| {
                let mut b = c.borrow_mut();
                match b.as_mut() {
                    Some(d) => {
                        let hv = hit_test(d, x, y);
                        let need = !d.tracking;
                        d.tracking = true;
                        (hv, need)
                    }
                    None => (Hover::None, false),
                }
            });
            if need_track {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
            }
            set_hover(hwnd, hover);
            LRESULT(0)
        }
        WM_MOUSELEAVE | WM_NCMOUSEMOVE => {
            if msg == WM_MOUSELEAVE {
                DLG.with(|c| {
                    if let Some(d) = c.borrow_mut().as_mut() {
                        d.tracking = false;
                    }
                });
            }
            set_hover(hwnd, Hover::None);
            if msg == WM_NCMOUSEMOVE {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let hit = DLG.with(|c| c.borrow().as_ref().map(|d| (hit_test(d, x, y), d.mode)));
            match hit {
                Some((Hover::Close, _)) | Some((Hover::Cancel, _)) => {
                    let _ = DestroyWindow(hwnd);
                }
                Some((Hover::Act, mode)) => {
                    let _ = DestroyWindow(hwnd);
                    crate::bar::open("shutdown.exe", Some(mode.args()), SW_HIDE);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 as u16 == VK_ESCAPE.0 {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            cleanup();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
