//! Menu bar gaya macOS di atas layar.
//!
//! - Jendela popup acrylic setinggi ~28px, didaftarkan sebagai AppBar (edge atas)
//!   sehingga jendela lain tidak menutupinya dan area kerja menyesuaikan.
//! - Kiri : logo (menu "Apple" versi Windows) + nama aplikasi aktif.
//! - Kanan: volume, Wi-Fi, baterai, jam & tanggal.
//! - Semua digambar dengan GDI (double buffer), tanpa framework UI.
//! - Dock (dock.rs) dibuat saat start dan dihancurkan saat keluar.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::core::{s, w, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, TrackMouseEvent, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, TME_LEAVE,
    TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{SHAppBarMessage, ShellExecuteW, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::dock;
use crate::settings;
use crate::panel::{
    self, Kind, ID_ABOUT, ID_ABOUT_KACHA, ID_KACHA_SETTINGS, ID_LOCK, ID_QUIT, ID_RESTART, ID_SETTINGS, ID_SHUTDOWN, ID_SLEEP,
    ID_START, ID_TASKMGR, WM_PANEL_CLOSED, WM_PANEL_CMD,
};
use crate::status::{self, Battery};
use crate::volume;
use crate::taskbar;

// ---------------------------------------------------------------------------
// Pengaturan tampilan (silakan ubah)
// ---------------------------------------------------------------------------

// Tinggi bar, ukuran font/ikon, jarak antar item, dan tint: lihat settings.rs
// (diubah user lewat "Kacha Settings…").
/// 4 = acrylic. Kalau tampilannya aneh, coba 3 (blur biasa).
const ACCENT_STATE: u32 = 4;

// ---------------------------------------------------------------------------

const WM_APPBAR: u32 = WM_USER + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_TICK: usize = 1;
const TIMER_TRAY: usize = 2;
const HOTKEY_QUIT: i32 = 1;
const HOTKEY_SETTINGS: i32 = 2;


// Glyph Segoe MDL2 Assets (ada di Windows 10).
const GLYPH_WIFI: char = '\u{E701}';
const GLYPH_TRAY: char = '\u{E70D}'; // ChevronDown

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Logo,
    App,
    Tray,
    Volume,
    Wifi,
    Battery,
    Clock,
}

#[derive(Default)]
struct State {
    scale: f32,
    height: i32,
    text_font: HFONT,
    bold_font: HFONT,
    icon_font: HFONT,
    logo: HICON,
    clock: String,
    app: String,
    battery: Battery,
    hits: Vec<(RECT, Item)>,
    hover: Option<Item>,
    tracking: bool,
    tray_ticks: u32,
    /// Pengaturan yang sedang diterapkan.
    cfg: settings::Values,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

// ---------------------------------------------------------------------------
// Acrylic
// ---------------------------------------------------------------------------

#[repr(C)]
struct AccentPolicy {
    accent_state: u32,
    accent_flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WinCompAttrData {
    attrib: u32,
    data: *mut c_void,
    size: usize,
}

type SetWindowCompositionAttributeFn = unsafe extern "system" fn(HWND, *mut WinCompAttrData) -> i32;

pub(crate) unsafe fn enable_acrylic(hwnd: HWND, tint: u32) {
    let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else {
        return;
    };
    let Some(proc_addr) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else {
        return;
    };
    let set_wca: SetWindowCompositionAttributeFn = std::mem::transmute(proc_addr);

    let mut policy = AccentPolicy {
        accent_state: ACCENT_STATE,
        accent_flags: 2,
        gradient_color: tint,
        animation_id: 0,
    };
    let mut data = WinCompAttrData {
        attrib: 19, // WCA_ACCENT_POLICY
        data: &mut policy as *mut _ as *mut c_void,
        size: size_of::<AccentPolicy>(),
    };
    set_wca(hwnd, &mut data);
}

// ---------------------------------------------------------------------------
// AppBar (memesan area di tepi atas layar)
// ---------------------------------------------------------------------------

const ABM_NEW: u32 = 0;
const ABM_REMOVE: u32 = 1;
const ABM_QUERYPOS: u32 = 2;
const ABM_SETPOS: u32 = 3;
const ABE_TOP: u32 = 1;

unsafe fn appbar_register(hwnd: HWND) {
    let mut abd = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: WM_APPBAR,
        ..Default::default()
    };
    SHAppBarMessage(ABM_NEW, &mut abd);
}

unsafe fn appbar_remove(hwnd: HWND) {
    let mut abd = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        ..Default::default()
    };
    SHAppBarMessage(ABM_REMOVE, &mut abd);
}

unsafe fn appbar_position(hwnd: HWND) {
    let height = STATE.with(|s| s.borrow().height);
    let screen_w = GetSystemMetrics(SM_CXSCREEN);

    let mut abd = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uEdge: ABE_TOP,
        rc: RECT {
            left: 0,
            top: 0,
            right: screen_w,
            bottom: height,
        },
        ..Default::default()
    };
    SHAppBarMessage(ABM_QUERYPOS, &mut abd);
    abd.rc.bottom = abd.rc.top + height;
    SHAppBarMessage(ABM_SETPOS, &mut abd);

    let _ = MoveWindow(
        hwnd,
        abd.rc.left,
        abd.rc.top,
        abd.rc.right - abd.rc.left,
        abd.rc.bottom - abd.rc.top,
        TRUE,
    );
}

// ---------------------------------------------------------------------------
// Font & helper gambar
// ---------------------------------------------------------------------------

unsafe fn make_font(face: &str, height_px: i32, weight: i32) -> HFONT {
    let mut lf = LOGFONTW::default();
    lf.lfHeight = -height_px;
    lf.lfWeight = weight;
    lf.lfCharSet = DEFAULT_CHARSET;
    lf.lfQuality = FONT_QUALITY(6); // 6 = CLEARTYPE_NATURAL_QUALITY
    for (i, c) in face.encode_utf16().take(31).enumerate() {
        lf.lfFaceName[i] = c;
    }
    CreateFontIndirectW(&lf)
}

/// Ukuran logo (px) pada skala DPI tertentu.
fn logo_size_px(scale: f32) -> i32 {
    ((settings::val(settings::BAR_LOGO) as f32 * scale) as i32).max(12)
}

/// Muat icon.ico yang ditanam sebagai resource ID 1 (lihat kacha.rc / build.rs).
unsafe fn load_logo(hinstance: HINSTANCE, scale: f32) -> HICON {
    let size = logo_size_px(scale);
    LoadImageW(
        hinstance,
        PCWSTR(1usize as *const u16), // MAKEINTRESOURCE(1)
        IMAGE_ICON,
        size,
        size,
        LR_DEFAULTCOLOR,
    )
    .map(|h| HICON(h.0))
    .unwrap_or_default()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

unsafe fn text_width(hdc: HDC, font: HFONT, text: &[u16]) -> (i32, i32) {
    SelectObject(hdc, font);
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, text, &mut size);
    (size.cx, size.cy)
}

unsafe fn draw_text(hdc: HDC, font: HFONT, text: &[u16], x: i32, bar_h: i32) {
    SelectObject(hdc, font);
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, text, &mut size);
    let y = (bar_h - size.cy) / 2;
    let _ = TextOutW(hdc, x, y, text);
}

unsafe fn highlight(hdc: HDC, rect: &RECT, bar_h: i32, scale: f32) {
    // Di atas permukaan "kaca", warna non-hitam menambah terang.
    let brush = CreateSolidBrush(COLORREF(0x00_46_46_46));
    let inset = (3.0 * scale) as i32;
    let r = RECT {
        left: rect.left,
        top: inset,
        right: rect.right,
        bottom: bar_h - inset,
    };
    FillRect(hdc, &r, brush);
    let _ = DeleteObject(brush);
}

// ---------------------------------------------------------------------------
// Menggambar menu bar
// ---------------------------------------------------------------------------

unsafe fn draw(hdc: HDC, w: i32, h: i32, st: &mut State) {
    st.hits.clear();
    let sc = st.scale;
    let pad = (settings::val(settings::BAR_PADDING) as f32 * sc) as i32;
    let gap = (settings::val(settings::BAR_SPACING) as f32 * sc) as i32;
    let hit_pad = gap * 2 / 5;
    let hover = st.hover;
    let active = panel::current().map(|k| match k {
        Kind::Apple | Kind::Start => Item::Logo,
        Kind::Calendar => Item::Clock,
        Kind::Wifi => Item::Wifi,
    });
    let is_hot = |i: Item| hover == Some(i) || active == Some(i);

    // ---- kiri: logo (dari icon.ico yang ditanam di exe) ----
    let logo_size = logo_size_px(sc);
    let logo_x = pad;
    let logo_y = (h - logo_size) / 2;
    let logo_rect = RECT {
        left: logo_x - hit_pad,
        top: 0,
        right: logo_x + logo_size + hit_pad,
        bottom: h,
    };
    if is_hot(Item::Logo) {
        highlight(hdc, &logo_rect, h, sc);
    }
    if st.logo.is_invalid() {
        // Cadangan kalau icon gagal dimuat: 4 kotak putih.
        let cell = (logo_size - 2) / 2;
        let white = HBRUSH(GetStockObject(WHITE_BRUSH).0);
        for row in 0..2 {
            for col in 0..2 {
                let l = logo_x + col * (cell + 2);
                let t = logo_y + row * (cell + 2);
                let r = RECT { left: l, top: t, right: l + cell, bottom: t + cell };
                FillRect(hdc, &r, white);
            }
        }
    } else {
        let _ = DrawIconEx(
            hdc,
            logo_x,
            logo_y,
            st.logo,
            logo_size,
            logo_size,
            0,
            HBRUSH::default(),
            DI_NORMAL,
        );
    }
    st.hits.push((logo_rect, Item::Logo));

    // ---- kiri: nama aplikasi aktif (tebal) ----
    let app_text = wide(&st.app);
    let (app_w, _) = text_width(hdc, st.bold_font, &app_text);
    let app_x = logo_rect.right + (6.0 * sc) as i32;
    let app_rect = RECT {
        left: app_x - hit_pad,
        top: 0,
        right: app_x + app_w + hit_pad,
        bottom: h,
    };
    if is_hot(Item::App) {
        highlight(hdc, &app_rect, h, sc);
    }
    draw_text(hdc, st.bold_font, &app_text, app_x, h);
    st.hits.push((app_rect, Item::App));

    // ---- kanan (dari kanan ke kiri): jam, baterai, wifi, volume ----
    let mut right = w - pad;

    // jam & tanggal
    let clock_text = wide(&st.clock);
    let (cw, _) = text_width(hdc, st.text_font, &clock_text);
    let cx = right - cw;
    let clock_rect = RECT {
        left: cx - hit_pad,
        top: 0,
        right: right + hit_pad,
        bottom: h,
    };
    if is_hot(Item::Clock) {
        highlight(hdc, &clock_rect, h, sc);
    }
    draw_text(hdc, st.text_font, &clock_text, cx, h);
    st.hits.push((clock_rect, Item::Clock));
    right = cx - gap;

    // baterai
    if st.battery.present {
        let pct = wide(&format!("{}%", st.battery.percent));
        let (pw, _) = text_width(hdc, st.text_font, &pct);
        let glyph = wide(&battery_glyph(&st.battery).to_string());
        let (gw, _) = text_width(hdc, st.icon_font, &glyph);
        let spacing = (5.0 * sc) as i32;
        let total = gw + spacing + pw;
        let bx = right - total;
        let rect = RECT {
            left: bx - hit_pad,
            top: 0,
            right: right + hit_pad,
            bottom: h,
        };
        if is_hot(Item::Battery) {
            highlight(hdc, &rect, h, sc);
        }
        draw_text(hdc, st.icon_font, &glyph, bx, h);
        draw_text(hdc, st.text_font, &pct, bx + gw + spacing, h);
        st.hits.push((rect, Item::Battery));
        right = bx - gap;
    }

    // wifi
    {
        let glyph = wide(&GLYPH_WIFI.to_string());
        let (gw, _) = text_width(hdc, st.icon_font, &glyph);
        let gx = right - gw;
        let rect = RECT {
            left: gx - hit_pad,
            top: 0,
            right: right + hit_pad,
            bottom: h,
        };
        if is_hot(Item::Wifi) {
            highlight(hdc, &rect, h, sc);
        }
        draw_text(hdc, st.icon_font, &glyph, gx, h);
        st.hits.push((rect, Item::Wifi));
        right = gx - gap;
    }

    // volume
    {
        let (vl, vm) = volume::current();
        let glyph = wide(&volume::glyph(vl, vm).to_string());
        let (gw0, _) = text_width(hdc, st.icon_font, &glyph);
        let (gw, _) = text_width(hdc, st.icon_font, &wide(&volume::glyph(100, false).to_string()));
        let gx = right - gw;
        let rect = RECT {
            left: gx - hit_pad,
            top: 0,
            right: right + hit_pad,
            bottom: h,
        };
        if is_hot(Item::Volume) {
            highlight(hdc, &rect, h, sc);
        }
        draw_text(hdc, st.icon_font, &glyph, gx + (gw - gw0) / 2, h);
        st.hits.push((rect, Item::Volume));
        right = gx - gap;
    }

    // panah tray (ikon tersembunyi: eject media, dll.)
    {
        let glyph = wide(&GLYPH_TRAY.to_string());
        let (gw, _) = text_width(hdc, st.icon_font, &glyph);
        let gx = right - gw;
        let rect = RECT {
            left: gx - hit_pad,
            top: 0,
            right: right + hit_pad,
            bottom: h,
        };
        if is_hot(Item::Tray) {
            highlight(hdc, &rect, h, sc);
        }
        draw_text(hdc, st.icon_font, &glyph, gx, h);
        st.hits.push((rect, Item::Tray));
    }
}

fn battery_glyph(b: &Battery) -> char {
    let level = (b.percent as u32 / 10).min(10);
    let code = if b.charging {
        if level >= 10 {
            0xE83E
        } else {
            0xE85A + level
        }
    } else if level >= 10 {
        0xE83F
    } else {
        0xE850 + level
    };
    char::from_u32(code).unwrap_or('\u{E83F}')
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);

    if w > 0 && h > 0 {
        // Double buffer supaya tidak berkedip.
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp);

        // Hitam = "kaca" (acrylic terlihat); teks putih terlihat di atasnya.
        FillRect(mem, &rc, HBRUSH(GetStockObject(BLACK_BRUSH).0));
        SetBkMode(mem, TRANSPARENT);
        SetTextColor(mem, COLORREF(0x00FF_FFFF));

        STATE.with(|s| draw(mem, w, h, &mut s.borrow_mut()));

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

pub(crate) unsafe fn open(file: &str, params: Option<&str>, show: SHOW_WINDOW_CMD) {
    let file = HSTRING::from(file);
    match params {
        Some(p) => {
            let p = HSTRING::from(p);
            ShellExecuteW(
                HWND::default(),
                w!("open"),
                PCWSTR(file.as_ptr()),
                PCWSTR(p.as_ptr()),
                PCWSTR::null(),
                show,
            );
        }
        None => {
            ShellExecuteW(
                HWND::default(),
                w!("open"),
                PCWSTR(file.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                show,
            );
        }
    }
}

/// Jendela "About Kacha" (lihat about.rs).
unsafe fn show_about(hwnd: HWND) {
    crate::about::show(hwnd);
}

/// Jalankan perintah dari menu Apple (dikirim panel lewat WM_PANEL_CMD).
unsafe fn run_command(hwnd: HWND, id: usize) {
    match id {
        ID_ABOUT => open("ms-settings:about", None, SW_SHOWNORMAL),
        ID_SETTINGS => open("ms-settings:", None, SW_SHOWNORMAL),
        ID_KACHA_SETTINGS => crate::settings_ui::show(hwnd),
        ID_START => {
            // Start Menu buatan sendiri, dibuka di bawah logo.
            let rect = STATE.with(|s| {
                s.borrow()
                    .hits
                    .iter()
                    .find(|(_, i)| *i == Item::Logo)
                    .map(|(r, _)| *r)
            });
            if let Some(rect) = rect {
                toggle_panel(hwnd, Kind::Start, rect);
            }
        }
        ID_ABOUT_KACHA => show_about(hwnd),
        ID_TASKMGR => open("taskmgr.exe", None, SW_SHOWNORMAL),
        ID_SLEEP => open(
            "rundll32.exe",
            Some("powrprof.dll,SetSuspendState 0,1,0"),
            SW_HIDE,
        ),
        ID_RESTART => crate::power::show(hwnd, crate::power::Mode::Restart),
        ID_SHUTDOWN => crate::power::show(hwnd, crate::power::Mode::Shutdown),
        ID_LOCK => open("rundll32.exe", Some("user32.dll,LockWorkStation"), SW_HIDE),
        ID_QUIT => {
            let _ = DestroyWindow(hwnd);
        }
        _ => {}
    }
}

/// Buka panel di bawah item yang diklik; klik lagi item yang sama = tutup.
unsafe fn toggle_panel(hwnd: HWND, kind: Kind, rect: RECT) {
    if panel::current() == Some(kind) {
        panel::close();
        invalidate(hwnd);
        return;
    }
    let mut tl = POINT { x: rect.left, y: rect.top };
    let mut br = POINT { x: rect.right, y: rect.bottom };
    let _ = ClientToScreen(hwnd, &mut tl);
    let _ = ClientToScreen(hwnd, &mut br);
    let scale = STATE.with(|s| s.borrow().scale);
    panel::open(
        kind,
        hwnd,
        RECT { left: tl.x, top: tl.y, right: br.x, bottom: br.y },
        scale,
    );
    invalidate(hwnd);
}

fn hit_test(x: i32, y: i32) -> Option<(RECT, Item)> {
    STATE.with(|s| {
        s.borrow()
            .hits
            .iter()
            .find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
            .copied()
    })
}

unsafe fn on_left_click(hwnd: HWND, x: i32, y: i32) {
    let Some((rect, item)) = hit_test(x, y) else {
        return;
    };
    match item {
        Item::Logo => toggle_panel(hwnd, Kind::Apple, rect),
        Item::Volume => volume::toggle_mute(),
        Item::Tray => toggle_tray(hwnd),
        Item::Wifi => toggle_panel(hwnd, Kind::Wifi, rect),
        Item::Battery => open("ms-settings:batterysaver", None, SW_SHOWNORMAL),
        Item::Clock => toggle_panel(hwnd, Kind::Calendar, rect),
        Item::App => {}
    }
}

/// Buka popup ikon tray Windows, lalu geser ke bawah tombol panah.
unsafe fn toggle_tray(hwnd: HWND) {
    panel::close();
    taskbar::toggle_overflow();
    STATE.with(|s| s.borrow_mut().tray_ticks = 15);
    SetTimer(hwnd, TIMER_TRAY, 40, None);
}

unsafe fn place_tray(hwnd: HWND) {
    let rect = STATE.with(|s| {
        s.borrow()
            .hits
            .iter()
            .find(|(_, i)| *i == Item::Tray)
            .map(|(r, _)| *r)
    });
    if let Some(r) = rect {
        let mut br = POINT { x: r.right, y: r.bottom };
        let _ = ClientToScreen(hwnd, &mut br);
        taskbar::place_overflow(br.x, br.y + 2);
    }
    let done = STATE.with(|s| {
        let mut st = s.borrow_mut();
        st.tray_ticks = st.tray_ticks.saturating_sub(1);
        st.tray_ticks == 0
    });
    if done {
        let _ = KillTimer(hwnd, TIMER_TRAY);
    }
}

unsafe fn on_right_click(x: i32, y: i32) {
    if let Some((_, item)) = hit_test(x, y) {
        match item {
            Item::Volume => open("ms-settings:sound", None, SW_SHOWNORMAL),
            Item::Wifi => open("ms-settings:network-wifi", None, SW_SHOWNORMAL),
            _ => {}
        }
    }
}

unsafe fn on_wheel(delta: i16) {
    let hovering_volume = STATE.with(|s| s.borrow().hover == Some(Item::Volume));
    if !hovering_volume {
        return;
    }
    let notches = (delta as i32 / 120).abs().max(1);
    volume::nudge(if delta > 0 { notches * 2 } else { -notches * 2 });
}

/// Perbarui jam, baterai, dan nama app. Return true kalau ada yang berubah.
fn refresh(hwnd: HWND) -> bool {
    let vol_changed = volume::poll();
    STATE.with(|s| {
        let mut st = s.borrow_mut();
        let mut changed = vol_changed;

        let clock = status::clock_text();
        if clock != st.clock {
            st.clock = clock;
            changed = true;
        }
        let battery = status::battery();
        if battery != st.battery {
            st.battery = battery;
            changed = true;
        }
        if let Some(app) = status::foreground_app(hwnd) {
            if app != st.app {
                st.app = app;
                changed = true;
            }
        }
        changed
    })
}

/// Tampilkan popup slider volume di bawah ikon volume.
unsafe fn show_volume_popup(hwnd: HWND) {
    let rect = STATE.with(|s| {
        s.borrow()
            .hits
            .iter()
            .find(|(_, i)| *i == Item::Volume)
            .map(|(r, _)| *r)
    });
    if let Some(r) = rect {
        let mut tl = POINT { x: r.left, y: r.top };
        let mut br = POINT { x: r.right, y: r.bottom };
        let _ = ClientToScreen(hwnd, &mut tl);
        let _ = ClientToScreen(hwnd, &mut br);
        volume::hover(hwnd, (tl.x + br.x) / 2, br.y);
    }
}

unsafe fn invalidate(hwnd: HWND) {
    let _ = InvalidateRect(hwnd, None, FALSE);
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

/// Terapkan pengaturan baru (dikirim jendela Settings lewat settings::WM_APPLY).
unsafe fn apply_settings(hwnd: HWND) {
    let new = settings::get();
    let (old, scale) = STATE.with(|s| {
        let st = s.borrow();
        (st.cfg, st.scale)
    });
    if new == old {
        return;
    }
    let Ok(hmodule) = GetModuleHandleW(PCWSTR::null()) else {
        return;
    };
    let hinstance: HINSTANCE = hmodule.into();

    STATE.with(|s| {
        let mut st = s.borrow_mut();
        let _ = DeleteObject(st.text_font);
        let _ = DeleteObject(st.bold_font);
        let _ = DeleteObject(st.icon_font);
        st.text_font = make_font(crate::fonts::FACE, (new[settings::BAR_FONT] as f32 * scale) as i32, 400);
        st.bold_font = make_font(crate::fonts::FACE, (new[settings::BAR_FONT] as f32 * scale) as i32, 700);
        st.icon_font = make_font("Segoe MDL2 Assets", (new[settings::BAR_ICON] as f32 * scale) as i32, 400);
        if !st.logo.is_invalid() {
            let _ = DestroyIcon(st.logo);
        }
        st.logo = load_logo(hinstance, scale);
        st.height = (new[settings::BAR_HEIGHT] as f32 * scale).round() as i32;
        st.cfg = new;
    });

    appbar_position(hwnd);
    enable_acrylic(hwnd, settings::bar_tint());
    if settings::dock_changed(&old, &new) {
        dock::destroy();
        dock::create(scale);
    }
    invalidate(hwnd);
}

pub fn run() -> Result<()> {
    unsafe {
        let _ = SetProcessDPIAware();

        let hmodule = GetModuleHandleW(PCWSTR::null())?;
        let hinstance: HINSTANCE = hmodule.into();

        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Relaxed,
        );

        let scale = GetDpiForSystem() as f32 / 96.0;
        let height = (settings::val(settings::BAR_HEIGHT) as f32 * scale).round() as i32;

        let logo = load_logo(hinstance, scale);

        STATE.with(|s| {
            let mut st = s.borrow_mut();
            st.logo = logo;
            st.scale = scale;
            st.height = height;
            st.text_font = make_font(crate::fonts::FACE, (settings::val(settings::BAR_FONT) as f32 * scale) as i32, 400);
            st.bold_font = make_font(crate::fonts::FACE, (settings::val(settings::BAR_FONT) as f32 * scale) as i32, 700);
            st.icon_font = make_font("Segoe MDL2 Assets", (settings::val(settings::BAR_ICON) as f32 * scale) as i32, 400);
            st.cfg = settings::get();
            st.clock = status::clock_text();
            st.battery = status::battery();
            st.app = "Desktop".to_string();
        });

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: w!("KachaMenuBar"),
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("KachaMenuBar"),
            w!("Kacha Menu Bar"),
            WS_POPUP,
            0,
            0,
            screen_w,
            height,
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )?;

        enable_acrylic(hwnd, settings::bar_tint());
        appbar_register(hwnd);
        appbar_position(hwnd);

        // Ctrl + Alt + Q = keluar (dan mengembalikan taskbar).
        let _ = RegisterHotKey(
            hwnd,
            HOTKEY_QUIT,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            'Q' as u32,
        );

        // Ctrl + Alt + S = buka Kacha Settings.
        let _ = RegisterHotKey(
            hwnd,
            HOTKEY_SETTINGS,
            MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
            'S' as u32,
        );

        // Satu timer ringan: jam, baterai, app aktif, dock, dan memastikan taskbar tetap mati.
        SetTimer(hwnd, TIMER_TICK, 500, None);

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        refresh(hwnd);

        // Dock di tengah bawah layar (jendela terpisah, tidak memesan work area).
        dock::create(scale);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
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
        WM_TIMER => {
            if wparam.0 == TIMER_TRAY {
                place_tray(hwnd);
            } else if wparam.0 == TIMER_TICK {
                taskbar::enforce_hidden();
                dock::keep_on_top(hwnd);
                dock::refresh();
                if refresh(hwnd) {
                    invalidate(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let new_hover = hit_test(x, y).map(|(_, i)| i);
            let (changed, need_track, old_hover) = STATE.with(|s| {
                let mut st = s.borrow_mut();
                let old = st.hover;
                let changed = old != new_hover;
                st.hover = new_hover;
                let need_track = !st.tracking;
                st.tracking = true;
                (changed, need_track, old)
            });
            if need_track {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
            }
            if changed {
                invalidate(hwnd);
                if new_hover == Some(Item::Volume) {
                    show_volume_popup(hwnd);
                } else if old_hover == Some(Item::Volume) {
                    volume::leave();
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            let was_volume = STATE.with(|s| {
                let mut st = s.borrow_mut();
                let was = st.hover == Some(Item::Volume);
                st.hover = None;
                st.tracking = false;
                was
            });
            if was_volume {
                volume::leave();
            }
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            on_left_click(hwnd, x, y);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            on_right_click(x, y);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
            on_wheel(delta);
            LRESULT(0)
        }
        settings::WM_APPLY => {
            apply_settings(hwnd);
            LRESULT(0)
        }
        WM_PANEL_CMD => {
            run_command(hwnd, wparam.0);
            LRESULT(0)
        }
        WM_PANEL_CLOSED => {
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_HOTKEY => {
            if wparam.0 as i32 == HOTKEY_QUIT {
                let _ = DestroyWindow(hwnd);
            } else if wparam.0 as i32 == HOTKEY_SETTINGS {
                crate::settings_ui::show(hwnd);
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            appbar_position(hwnd);
            dock::reposition();
            LRESULT(0)
        }
        WM_APPBAR => {
            match wparam.0 as u32 {
                1 => appbar_position(hwnd), // ABN_POSCHANGED
                2 => {
                    // ABN_FULLSCREENAPP: turun saat ada aplikasi fullscreen (menu bar + dock).
                    let insert_after = if lparam.0 != 0 {
                        HWND_BOTTOM
                    } else {
                        HWND_TOPMOST
                    };
                    let _ = SetWindowPos(
                        hwnd,
                        insert_after,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                    dock::set_fullscreen(lparam.0 != 0);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            dock::destroy();
            volume::destroy();
            panel::close();
            let _ = KillTimer(hwnd, TIMER_TICK);
            let _ = KillTimer(hwnd, TIMER_TRAY);
            appbar_remove(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            // explorer.exe restart: taskbar baru muncul & appbar hilang.
            taskbar::hide();
            appbar_register(hwnd);
            appbar_position(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}