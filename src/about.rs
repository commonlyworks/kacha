//! Jendela "About Kacha" gaya macOS: vertikal, dark (acrylic), logo di atas.
//!
//! - Satu jendela popup acrylic bersudut membulat, tanpa title bar bawaan Windows.
//! - Tombol tutup bulat merah di kiri atas (seperti macOS), atau tekan Esc.
//! - Bisa digeser dengan menarik bagian mana saja dari jendela.
//! - Digambar dengan GDI (double buffer), hanya ada saat dibuka.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::status;

// ---------------------------------------------------------------------------
// Pengaturan (silakan ubah)
// ---------------------------------------------------------------------------

/// Ukuran jendela (px pada 96 DPI).
const WIN_W: f32 = 280.0;
const WIN_H: f32 = 312.0;
/// Ukuran logo (px pada 96 DPI).
pub(crate) const LOGO: f32 = 84.0;
/// Radius sudut jendela.
pub(crate) const RADIUS: f32 = 22.0;
/// Warna tint acrylic, 0xAABBGGRR (sama seperti panel).
pub(crate) const TINT: u32 = 0xB8_18_18_18;

pub(crate) const WHITE: u32 = 0x00FF_FFFF;
pub(crate) const DIM: u32 = 0x00A6_A6A6;
pub(crate) const FAINT: u32 = 0x0070_7070;
pub(crate) const LINE: u32 = 0x0058_5858;
pub(crate) const RED: u32 = 0x003A_45FF; // 0xFF453A
pub(crate) const RED_DARK: u32 = 0x0010_1A8C;

// ---------------------------------------------------------------------------

const WM_MOUSELEAVE: u32 = 0x02A3;

static ABOUT_HWND: AtomicIsize = AtomicIsize::new(0);

struct About {
    sc: f32,
    w: i32,
    h: i32,
    logo: HICON,
    title_font: HFONT,
    body_font: HFONT,
    small_font: HFONT,
    hover_close: bool,
    tracking: bool,
}

thread_local! {
    static ABOUT: RefCell<Option<About>> = RefCell::new(None);
}

pub(crate) fn px(v: f32, sc: f32) -> i32 {
    (v * sc).round() as i32
}

pub(crate) fn rc(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

pub(crate) unsafe fn make_font(face: &str, height_px: i32, weight: i32) -> HFONT {
    let mut lf = LOGFONTW::default();
    lf.lfHeight = -height_px;
    lf.lfWeight = weight;
    lf.lfCharSet = DEFAULT_CHARSET;
    lf.lfQuality = FONT_QUALITY(6); // CLEARTYPE_NATURAL_QUALITY
    for (i, c) in face.encode_utf16().take(31).enumerate() {
        lf.lfFaceName[i] = c;
    }
    CreateFontIndirectW(&lf)
}

pub(crate) unsafe fn text(hdc: HDC, font: HFONT, s: &str, r: RECT, color: u32, align: DRAW_TEXT_FORMAT) {
    SelectObject(hdc, font);
    SetTextColor(hdc, COLORREF(color));
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    let mut rect = r;
    DrawTextW(
        hdc,
        &mut buf,
        &mut rect,
        align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
    );
}

pub(crate) unsafe fn ellipse(hdc: HDC, r: RECT, color: u32) {
    let b = CreateSolidBrush(COLORREF(color));
    let ob = SelectObject(hdc, b);
    let op = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = Ellipse(hdc, r.left, r.top, r.right + 1, r.bottom + 1);
    SelectObject(hdc, op);
    SelectObject(hdc, ob);
    let _ = DeleteObject(b);
}

unsafe fn fill(hdc: HDC, r: RECT, color: u32) {
    let b = CreateSolidBrush(COLORREF(color));
    FillRect(hdc, &r, b);
    let _ = DeleteObject(b);
}

/// Area tombol tutup (kiri atas).
pub(crate) fn close_rect(sc: f32) -> RECT {
    let d = px(13.0, sc);
    let x = px(16.0, sc);
    let y = px(16.0, sc);
    rc(x, y, x + d, y + d)
}

pub(crate) fn in_rect(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

// ---------------------------------------------------------------------------
// Menggambar
// ---------------------------------------------------------------------------

unsafe fn draw(hdc: HDC, a: &About) {
    let sc = a.sc;
    let (w, h) = (a.w, a.h);

    // Hitam = "kaca" (acrylic terlihat), sama seperti menu bar & panel.
    FillRect(hdc, &rc(0, 0, w, h), HBRUSH(GetStockObject(BLACK_BRUSH).0));
    SetBkMode(hdc, TRANSPARENT);

    // ---- tombol tutup ----
    let cr = close_rect(sc);
    ellipse(hdc, cr, RED);
    if a.hover_close {
        // Tanda "x" kecil di tengah saat hover.
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

    // ---- logo di tengah atas ----
    let logo = px(LOGO, sc);
    let lx = (w - logo) / 2;
    let ly = px(40.0, sc);
    if !a.logo.is_invalid() {
        let _ = DrawIconEx(
            hdc,
            lx,
            ly,
            a.logo,
            logo,
            logo,
            0,
            HBRUSH::default(),
            DI_NORMAL,
        );
    }

    // ---- nama & versi ----
    let name_y = ly + logo + px(14.0, sc);
    text(
        hdc,
        a.title_font,
        "Kacha",
        rc(0, name_y, w, name_y + px(32.0, sc)),
        WHITE,
        DT_CENTER,
    );

    let ver_y = name_y + px(32.0, sc);
    let version = format!("Version {}", env!("CARGO_PKG_VERSION"));
    text(
        hdc,
        a.body_font,
        &version,
        rc(0, ver_y, w, ver_y + px(22.0, sc)),
        DIM,
        DT_CENTER,
    );

    // ---- garis pemisah ----
    let line_y = ver_y + px(22.0, sc) + px(18.0, sc);
    fill(
        hdc,
        rc(px(36.0, sc), line_y, w - px(36.0, sc), line_y + 1),
        LINE,
    );

    // ---- copyright & lisensi ----
    let c_y = line_y + px(14.0, sc);
    let copyright = format!("\u{00A9} {} Commonly Works", status::current_year());
    text(
        hdc,
        a.small_font,
        &copyright,
        rc(0, c_y, w, c_y + px(20.0, sc)),
        DIM,
        DT_CENTER,
    );
    let l_y = c_y + px(20.0, sc);
    text(
        hdc,
        a.small_font,
        "Released under the MIT license.",
        rc(0, l_y, w, l_y + px(20.0, sc)),
        FAINT,
        DT_CENTER,
    );
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    ABOUT.with(|c| {
        let b = c.borrow();
        let Some(a) = b.as_ref() else {
            return;
        };
        if a.w <= 0 || a.h <= 0 {
            return;
        }
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, a.w, a.h);
        let old = SelectObject(mem, bmp);

        draw(mem, a);

        let _ = BitBlt(hdc, 0, 0, a.w, a.h, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
    });

    let _ = EndPaint(hwnd, &ps);
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

/// Tampilkan jendela About (kalau sudah terbuka, cukup bawa ke depan).
pub unsafe fn show(owner: HWND) {
    let existing = ABOUT_HWND.load(Ordering::SeqCst);
    if existing != 0 {
        let h = HWND(existing as *mut _);
        if IsWindow(h).as_bool() {
            let _ = SetForegroundWindow(h);
            return;
        }
    }

    let hmodule = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
    let hinstance: HINSTANCE = hmodule.into();

    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: w!("KachaAbout"),
        hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&wc); // gagal kalau sudah terdaftar: aman diabaikan

    let sc = GetDpiForSystem() as f32 / 96.0;
    let (w, h) = (px(WIN_W, sc), px(WIN_H, sc));

    // Muat icon.ico (resource ID 1) langsung di ukuran yang dipakai.
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
    ABOUT.with(|c| {
        *c.borrow_mut() = Some(About {
            sc,
            w,
            h,
            logo,
            title_font: make_font(face, px(24.0, sc), 700),
            body_font: make_font(face, px(14.0, sc), 400),
            small_font: make_font(face, px(12.5, sc), 400),
            hover_close: false,
            tracking: false,
        });
    });

    // Di tengah layar, sedikit di atas titik tengah.
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let x = (sw - w) / 2;
    let y = ((sh - h) / 2 - px(20.0, sc)).max(0);

    let Ok(hwnd) = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        w!("KachaAbout"),
        w!("About Kacha"),
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
    ABOUT_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

    crate::bar::enable_acrylic(hwnd, TINT);
    let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, px(RADIUS, sc) * 2, px(RADIUS, sc) * 2);
    SetWindowRgn(hwnd, rgn, TRUE);

    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd);
}

unsafe fn cleanup() {
    ABOUT.with(|c| {
        if let Some(a) = c.borrow_mut().take() {
            if !a.logo.is_invalid() {
                let _ = DestroyIcon(a.logo);
            }
            for f in [a.title_font, a.body_font, a.small_font] {
                if !f.is_invalid() {
                    let _ = DeleteObject(f);
                }
            }
        }
    });
    ABOUT_HWND.store(0, Ordering::SeqCst);
}

fn set_hover(hwnd: HWND, hover: bool) {
    let changed = ABOUT.with(|c| {
        if let Some(a) = c.borrow_mut().as_mut() {
            if a.hover_close != hover {
                a.hover_close = hover;
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

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        // Seluruh jendela bisa dipakai untuk menggeser, kecuali tombol tutup.
        WM_NCHITTEST => {
            let mut pt = POINT {
                x: (lparam.0 & 0xFFFF) as i16 as i32,
                y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
            };
            let _ = ScreenToClient(hwnd, &mut pt);
            let on_close = ABOUT.with(|c| {
                c.borrow()
                    .as_ref()
                    .map(|a| in_rect(&close_rect(a.sc), pt.x, pt.y))
                    .unwrap_or(false)
            });
            LRESULT(if on_close { 1 } else { 2 }) // HTCLIENT : HTCAPTION
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let (on_close, need_track) = ABOUT.with(|c| {
                let mut b = c.borrow_mut();
                match b.as_mut() {
                    Some(a) => {
                        let on = in_rect(&close_rect(a.sc), x, y);
                        let need = !a.tracking;
                        a.tracking = true;
                        (on, need)
                    }
                    None => (false, false),
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
            set_hover(hwnd, on_close);
            LRESULT(0)
        }
        WM_MOUSELEAVE | WM_NCMOUSEMOVE => {
            if msg == WM_MOUSELEAVE {
                ABOUT.with(|c| {
                    if let Some(a) = c.borrow_mut().as_mut() {
                        a.tracking = false;
                    }
                });
            }
            set_hover(hwnd, false);
            if msg == WM_NCMOUSEMOVE {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let on_close = ABOUT.with(|c| {
                c.borrow()
                    .as_ref()
                    .map(|a| in_rect(&close_rect(a.sc), x, y))
                    .unwrap_or(false)
            });
            if on_close {
                let _ = DestroyWindow(hwnd);
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