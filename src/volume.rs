//! Kontrol volume langsung (tanpa membuka Settings Windows).
//!
//! - Baca/ubah volume master + mute lewat Core Audio (IAudioEndpointVolume).
//! - Ikon volume di menu bar dinamis: mute, rendah, sedang, tinggi (`glyph`).
//! - Hover ikon volume -> popup slider acrylic kecil di bawahnya (gaya panel/About).
//!   Popup tertutup sendiri beberapa saat setelah kursor pergi dari ikon dan popup.
//! - Semua berjalan di thread UI; tidak ada thread latar belakang.

use std::cell::{Cell, RefCell};

use windows::core::{w, IUnknown, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::about::{make_font, px, rc, text};

// ---------------------------------------------------------------------------
// Core Audio
// ---------------------------------------------------------------------------

thread_local! {
    static EP: RefCell<Option<IAudioEndpointVolume>> = RefCell::new(None);
    static COM: Cell<bool> = Cell::new(false);
    /// (level 0..100, muted) terakhir yang diketahui.
    static CUR: Cell<(i32, bool)> = Cell::new((50, false));
    static TICKS: Cell<u32> = Cell::new(0);
    static BAR: Cell<HWND> = Cell::new(HWND::default());
}

fn endpoint() -> Option<IAudioEndpointVolume> {
    if let Some(e) = EP.with(|c| c.borrow().clone()) {
        return Some(e);
    }
    unsafe {
        if !COM.with(|c| c.replace(true)) {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let en: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None::<&IUnknown>, CLSCTX_ALL).ok()?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let ep: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None).ok()?;
        EP.with(|c| *c.borrow_mut() = Some(ep.clone()));
        Some(ep)
    }
}

fn drop_endpoint() {
    EP.with(|c| *c.borrow_mut() = None);
}

/// Level & mute terakhir.
pub fn current() -> (i32, bool) {
    CUR.with(|c| c.get())
}

/// Ikon Segoe MDL2 sesuai kondisi: mute, rendah, sedang, tinggi.
pub fn glyph(level: i32, muted: bool) -> char {
    if muted || level <= 0 {
        '\u{E74F}' // Mute
    } else if level < 34 {
        '\u{E993}' // Volume1
    } else if level < 67 {
        '\u{E994}' // Volume2
    } else {
        '\u{E995}' // Volume3
    }
}

/// Baca ulang dari Windows (dipanggil berkala oleh menu bar, menangkap perubahan dari
/// tombol media / aplikasi lain). Return true kalau berubah.
pub fn poll() -> bool {
    let n = TICKS.with(|t| {
        t.set(t.get().wrapping_add(1));
        t.get()
    });
    if n % 10 == 0 {
        drop_endpoint(); // ikuti perubahan perangkat output default
    }
    let Some(ep) = endpoint() else {
        return false;
    };
    let read = unsafe { (ep.GetMasterVolumeLevelScalar(), ep.GetMute()) };
    let (Ok(l), Ok(m)) = read else {
        drop_endpoint();
        return false;
    };
    let new = (((l * 100.0).round() as i32).clamp(0, 100), m.as_bool());
    let changed = CUR.with(|c| c.replace(new)) != new;
    if changed {
        repaint_popup();
    }
    changed
}

fn repaint_popup() {
    let h = POP.with(|c| c.borrow().as_ref().map(|p| p.hwnd));
    if let Some(h) = h {
        unsafe {
            let _ = InvalidateRect(h, None, false);
        }
    }
}

fn repaint() {
    let b = BAR.with(|c| c.get());
    if !b.is_invalid() {
        unsafe {
            let _ = InvalidateRect(b, None, false);
        }
    }
    repaint_popup();
}

/// Atur volume 0..100 langsung. Menaikkan dari 0/mute otomatis meng-unmute.
pub fn set_level(pct: i32) {
    let p = pct.clamp(0, 100);
    let (_, mut muted) = current();
    if let Some(ep) = endpoint() {
        unsafe {
            let _ = ep.SetMasterVolumeLevelScalar(p as f32 / 100.0, std::ptr::null());
            if p > 0 && muted {
                let _ = ep.SetMute(BOOL::from(false), std::ptr::null());
                muted = false;
            }
        }
        CUR.with(|c| c.set((p, muted)));
        repaint();
    }
}

pub fn toggle_mute() {
    let (l, m) = current();
    if let Some(ep) = endpoint() {
        unsafe {
            let _ = ep.SetMute(BOOL::from(!m), std::ptr::null());
        }
        CUR.with(|c| c.set((l, !m)));
        repaint();
    }
}

/// Geser volume relatif (dipakai scroll wheel di atas ikon).
pub fn nudge(delta: i32) {
    set_level(current().0 + delta);
}

// ---------------------------------------------------------------------------
// Popup slider
// ---------------------------------------------------------------------------

const POP_W: f32 = 220.0;
const POP_H: f32 = 46.0;
const POP_RADIUS: f32 = 12.0;
/// Tint acrylic, sama seperti panel.
const POP_TINT: u32 = 0xB8_18_18_18;
const HIDE_MS: u32 = 250;
const TIMER_HIDE: usize = 1;
const WM_MOUSELEAVE: u32 = 0x02A3;

const WHITE: u32 = 0x00FF_FFFF;
const DIM: u32 = 0x00A6_A6A6;
const TRACK: u32 = 0x0058_5858;
const BLUE: u32 = 0x00FF_840A; // 0x0A84FF dalam BGR (aksen yang sama dengan panel)

struct Pop {
    hwnd: HWND,
    sc: f32,
    w: i32,
    h: i32,
    text_font: HFONT,
    icon_font: HFONT,
    drag: bool,
    tracking: bool,
}

thread_local! {
    static POP: RefCell<Option<Pop>> = RefCell::new(None);
}

fn glyph_rect(sc: f32) -> RECT {
    rc(px(10.0, sc), 0, px(40.0, sc), px(POP_H, sc))
}

fn track_rect(sc: f32) -> RECT {
    let cy = px(POP_H, sc) / 2;
    rc(px(48.0, sc), cy - px(2.0, sc), px(POP_W, sc) - px(60.0, sc), cy + px(2.0, sc))
}

fn pct_rect(sc: f32) -> RECT {
    rc(px(POP_W, sc) - px(52.0, sc), 0, px(POP_W, sc) - px(12.0, sc), px(POP_H, sc))
}

fn value_at(sc: f32, x: i32) -> i32 {
    let t = track_rect(sc);
    let f = ((x - t.left) as f32 / (t.right - t.left).max(1) as f32).clamp(0.0, 1.0);
    (f * 100.0).round() as i32
}

fn xy(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam.0 & 0xFFFF) as i16 as i32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    )
}

unsafe fn round(hdc: HDC, r: RECT, radius: i32, color: u32) {
    let b = CreateSolidBrush(COLORREF(color));
    let ob = SelectObject(hdc, b);
    let op = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = RoundRect(hdc, r.left, r.top, r.right + 1, r.bottom + 1, radius * 2, radius * 2);
    SelectObject(hdc, op);
    SelectObject(hdc, ob);
    let _ = DeleteObject(b);
}

unsafe fn disc(hdc: HDC, cx: i32, cy: i32, rad: i32, color: u32) {
    let b = CreateSolidBrush(COLORREF(color));
    let ob = SelectObject(hdc, b);
    let op = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = Ellipse(hdc, cx - rad, cy - rad, cx + rad + 1, cy + rad + 1);
    SelectObject(hdc, op);
    SelectObject(hdc, ob);
    let _ = DeleteObject(b);
}

unsafe fn draw(hdc: HDC, p: &Pop) {
    let sc = p.sc;
    let (level, muted) = current();

    // Hitam = "kaca" (acrylic terlihat), sama seperti menu bar & panel.
    FillRect(hdc, &rc(0, 0, p.w, p.h), HBRUSH(GetStockObject(BLACK_BRUSH).0));
    SetBkMode(hdc, TRANSPARENT);

    text(hdc, p.icon_font, &glyph(level, muted).to_string(), glyph_rect(sc), WHITE, DT_CENTER);

    let t = track_rect(sc);
    let cy = (t.top + t.bottom) / 2;
    round(hdc, t, px(2.0, sc), TRACK);
    let kx = t.left + ((t.right - t.left) as f32 * level as f32 / 100.0).round() as i32;
    if kx > t.left {
        round(hdc, RECT { right: kx, ..t }, px(2.0, sc), if muted { DIM } else { BLUE });
    }
    disc(hdc, kx, cy, px(7.0, sc), WHITE);

    text(hdc, p.text_font, &format!("{}%", level), pct_rect(sc), DIM, DT_RIGHT);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    POP.with(|c| {
        let b = c.borrow();
        let Some(p) = b.as_ref() else {
            return;
        };
        if p.w <= 0 || p.h <= 0 {
            return;
        }
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, p.w, p.h);
        let old = SelectObject(mem, bmp);
        draw(mem, p);
        let _ = BitBlt(hdc, 0, 0, p.w, p.h, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
    });
    let _ = EndPaint(hwnd, &ps);
}

/// Buat jendela popup kalau belum ada. Return (hwnd, scale, lebar).
unsafe fn ensure() -> Option<(HWND, f32, i32)> {
    if let Some(r) = POP.with(|c| c.borrow().as_ref().map(|p| (p.hwnd, p.sc, p.w))) {
        return Some(r);
    }

    let hmodule = GetModuleHandleW(PCWSTR::null()).ok()?;
    let hinstance: HINSTANCE = hmodule.into();
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: w!("KachaVolume"),
        hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&wc); // gagal kalau sudah terdaftar: aman diabaikan

    let sc = GetDpiForSystem() as f32 / 96.0;
    let (w, h) = (px(POP_W, sc), px(POP_H, sc));
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        w!("KachaVolume"),
        w!("Kacha Volume"),
        WS_POPUP,
        0,
        0,
        w,
        h,
        HWND::default(),
        HMENU::default(),
        hinstance,
        None,
    )
    .ok()?;

    crate::bar::enable_acrylic(hwnd, POP_TINT);
    let r = px(POP_RADIUS, sc) * 2;
    let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, r, r);
    SetWindowRgn(hwnd, rgn, TRUE);

    let face = crate::fonts::FACE;
    POP.with(|c| {
        *c.borrow_mut() = Some(Pop {
            hwnd,
            sc,
            w,
            h,
            text_font: make_font(face, px(13.0, sc), 400),
            icon_font: make_font("Segoe MDL2 Assets", px(16.0, sc), 400),
            drag: false,
            tracking: false,
        });
    });
    Some((hwnd, sc, w))
}

/// Kursor di atas ikon volume: tampilkan popup di bawahnya.
/// `cx` = tengah ikon (koordinat layar), `bottom` = tepi bawah menu bar.
pub unsafe fn hover(bar: HWND, cx: i32, bottom: i32) {
    BAR.with(|c| c.set(bar));
    let Some((hwnd, sc, w)) = ensure() else {
        return;
    };
    let _ = KillTimer(hwnd, TIMER_HIDE);
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let x = (cx - w / 2).clamp(0, (sw - w).max(0));
    let y = bottom + px(4.0, sc);
    let _ = SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        x,
        y,
        0,
        0,
        SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    let _ = InvalidateRect(hwnd, None, false);
}

/// Kursor meninggalkan ikon volume: jadwalkan tutup (dibatalkan kalau kursor masuk popup).
pub unsafe fn leave() {
    let h = POP.with(|c| c.borrow().as_ref().map(|p| p.hwnd));
    if let Some(h) = h {
        if IsWindowVisible(h).as_bool() {
            SetTimer(h, TIMER_HIDE, HIDE_MS, None);
        }
    }
}

pub unsafe fn destroy() {
    let h = POP.with(|c| c.borrow().as_ref().map(|p| p.hwnd));
    if let Some(h) = h {
        let _ = DestroyWindow(h);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => {
            let (x, y) = xy(lparam);
            let Some(sc) = POP.with(|c| c.borrow().as_ref().map(|p| p.sc)) else {
                return LRESULT(0);
            };
            if x < glyph_rect(sc).right && y >= 0 {
                toggle_mute(); // klik ikon di popup = mute/unmute
            } else {
                POP.with(|c| {
                    if let Some(p) = c.borrow_mut().as_mut() {
                        p.drag = true;
                    }
                });
                SetCapture(hwnd);
                set_level(value_at(sc, x));
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, _) = xy(lparam);
            let _ = KillTimer(hwnd, TIMER_HIDE);
            let st = POP.with(|c| {
                c.borrow_mut().as_mut().map(|p| {
                    let need = !p.tracking;
                    p.tracking = true;
                    (p.sc, p.drag, need)
                })
            });
            if let Some((sc, drag, need)) = st {
                if need {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                }
                if drag {
                    set_level(value_at(sc, x));
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let was = POP.with(|c| {
                c.borrow_mut()
                    .as_mut()
                    .map(|p| std::mem::replace(&mut p.drag, false))
                    .unwrap_or(false)
            });
            if was {
                let _ = ReleaseCapture();
                SetTimer(hwnd, TIMER_HIDE, HIDE_MS, None);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let drag = POP.with(|c| {
                c.borrow_mut()
                    .as_mut()
                    .map(|p| {
                        p.tracking = false;
                        p.drag
                    })
                    .unwrap_or(false)
            });
            if !drag {
                SetTimer(hwnd, TIMER_HIDE, HIDE_MS, None);
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_HIDE {
                let _ = KillTimer(hwnd, TIMER_HIDE);
                let drag = POP.with(|c| c.borrow().as_ref().map(|p| p.drag).unwrap_or(false));
                let mut pt = POINT::default();
                let mut wr = RECT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = GetWindowRect(hwnd, &mut wr);
                let inside = pt.x >= wr.left && pt.x < wr.right && pt.y >= wr.top && pt.y < wr.bottom;
                if !drag && !inside {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let p = POP.with(|c| c.borrow_mut().take());
            if let Some(p) = p {
                let _ = KillTimer(hwnd, TIMER_HIDE);
                let _ = DeleteObject(p.text_font);
                let _ = DeleteObject(p.icon_font);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
