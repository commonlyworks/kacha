//! Panel dropdown gaya macOS: menu Apple, kalender, dan Wi-Fi.
//!
//! - Satu jendela popup acrylic dengan sudut membulat, muncul meluncur dari menu bar.
//! - Digambar dengan GDI (double buffer), tanpa framework UI.
//! - Hanya ada saat dibuka: tidak ada timer / memori saat panel tertutup.
//! - Tertutup otomatis saat kehilangan fokus (klik di luar) atau tekan Esc.

use std::cell::RefCell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, TRUE, WPARAM,
};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::{GetLocalTime, GetTickCount64};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::apps::{self, App};
use crate::wifi::{Net, Sec, Wifi};

// ---------------------------------------------------------------------------
// Pengaturan (silakan ubah)
// ---------------------------------------------------------------------------

/// Warna tint acrylic panel, 0xAABBGGRR. Lebih pekat dari menu bar supaya teks terbaca.
const PANEL_TINT: u32 = 0xB8_18_18_18;
/// Awal minggu di kalender: 0 = Minggu, 1 = Senin.
const WEEK_START: i32 = 1;
/// Lama animasi meluncur (ms).
const ANIM_MS: f32 = 170.0;

const WHITE: u32 = 0x00FF_FFFF;
const DIM: u32 = 0x00A6_A6A6;
const FAINT: u32 = 0x0070_7070;
const BLUE: u32 = 0x00FF_840A; // 0x0A84FF dalam BGR (aksen macOS)
const RED: u32 = 0x003A_45FF; // 0xFF453A
const SOFT: u32 = 0x0046_4646;
const LINE: u32 = 0x0058_5858;
const OFF: u32 = 0x0058_5858;
const BLUE_HI: u32 = 0x00FF_A04A; // hover tombol utama

// ---------------------------------------------------------------------------

pub const WM_PANEL_CLOSED: u32 = WM_USER + 2;
pub const WM_PANEL_CMD: u32 = WM_USER + 3;
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_ANIM: usize = 1;
const TIMER_WIFI: usize = 2;

pub const ID_ABOUT: usize = 1;
pub const ID_SETTINGS: usize = 2;
pub const ID_START: usize = 3;
pub const ID_TASKMGR: usize = 4;
pub const ID_SLEEP: usize = 5;
pub const ID_RESTART: usize = 6;
pub const ID_SHUTDOWN: usize = 7;
pub const ID_LOCK: usize = 8;
pub const ID_QUIT: usize = 9;
pub const ID_ABOUT_KACHA: usize = 10;
pub const ID_KACHA_SETTINGS: usize = 11;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Apple,
    Calendar,
    Wifi,
    Start,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Cmd(usize),
    Prev,
    Next,
    Today,
    OpenCalendar,
    DateSettings,
    Scan,
    Select(usize),
    Connect(usize),
    Disconnect,
    Forget(usize),
    JoinOk,
    JoinCancel,
    Launch(usize),
}

enum Row {
    Sep,
    Item(usize, char, &'static str, &'static str),
}

const MENU: [Row; 14] = [
    Row::Item(ID_ABOUT, '\u{E946}', "About This PC", ""),
    Row::Sep,
    Row::Item(ID_SETTINGS, '\u{E713}', "System Settings…", "Win+I"),
    Row::Item(ID_START, '\u{E71D}', "Start Menu", ""),
    Row::Item(ID_KACHA_SETTINGS, '\u{E790}', "Kacha Settings…", "Ctrl+Alt+S"),
    Row::Item(ID_TASKMGR, '\u{E7BA}', "Force Quit…", "Ctrl+Shift+Esc"),
    Row::Sep,
    Row::Item(ID_SLEEP, '\u{E708}', "Sleep", ""),
    Row::Item(ID_RESTART, '\u{E72C}', "Restart…", ""),
    Row::Item(ID_SHUTDOWN, '\u{E7E8}', "Shut Down…", ""),
    Row::Sep,
    Row::Item(ID_LOCK, '\u{E72E}', "Lock Screen", "Win+L"),
    Row::Item(ID_ABOUT_KACHA, '\u{E946}', "About Kacha", ""),
    Row::Item(ID_QUIT, '\u{E8BB}', "Quit Kacha", "Ctrl+Alt+Q"),
];

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];
const DOW: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];

const START_ROWS: usize = 12;
const START_ROW_H: f32 = 28.0;

#[derive(Clone, Copy)]
enum Line {
    Header(char),
    App(usize),
}

#[derive(Clone, Copy, Default)]
struct Fonts {
    text: HFONT,
    small: HFONT,
    title: HFONT,
    icon: HFONT,
}

struct Join {
    ssid: String,
    sec: Sec,
}

struct Panel {
    hwnd: HWND,
    bar: HWND,
    kind: Kind,
    scale: f32,
    w: i32,
    full_h: i32,
    cur_h: i32,
    anim_start: u64,
    fonts: Fonts,
    hits: Vec<(RECT, Act)>,
    hover: Option<Act>,
    tracking: bool,
    // kalender
    cal_year: i32,
    cal_month: i32,
    // wifi
    wifi: Option<Wifi>,
    nets: Vec<Net>,
    refreshes: u32,
    connecting: Option<String>,
    selected: Option<String>,
    join: Option<Join>,
    pw: String,
    notice: Option<String>,
    notice_at: u32,
    /// Profile yang baru dibuat dan masih dicoba; dihapus kalau gagal tersambung.
    fresh: Option<Vec<u16>>,
    conn_ticks: u32,
    ticks: u32,
    // start menu
    apps: Vec<App>,
    rows: Vec<Line>,
    query: String,
    top: usize,
}

impl Panel {
    /// Posisi jaringan terpilih di daftar yang terlihat.
    fn sel_index(&self) -> Option<usize> {
        let s = self.selected.as_deref()?;
        self.nets.iter().take(WIFI_MAX_ROWS).position(|n| n.ssid == s)
    }

    fn say(&mut self, s: String) {
        self.notice = Some(s);
        self.notice_at = self.ticks;
    }
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = RefCell::new(None);
}

// ---------------------------------------------------------------------------
// API publik
// ---------------------------------------------------------------------------

pub fn current() -> Option<Kind> {
    PANEL.with(|c| c.borrow().as_ref().map(|p| p.kind))
}

pub unsafe fn close() {
    let hwnd = PANEL.with(|c| c.borrow().as_ref().map(|p| p.hwnd));
    if let Some(h) = hwnd {
        let _ = DestroyWindow(h);
    }
}

/// `anchor` = rect item menu bar dalam koordinat layar.
pub unsafe fn open(kind: Kind, bar: HWND, anchor: RECT, scale: f32) {
    close();

    let Ok(hm) = GetModuleHandleW(PCWSTR::null()) else {
        return;
    };
    let hinstance: HINSTANCE = hm.into();
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: w!("KachaPanel"),
        hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&wc); // kalau sudah terdaftar, hasilnya 0 dan tidak masalah

    let sc = scale;
    let now = GetLocalTime();

    let mut wifi = None;
    let mut nets = Vec::new();
    if kind == Kind::Wifi {
        wifi = Wifi::open();
        if let Some(wf) = &wifi {
            wf.scan();
            nets = wf.networks();
        }
    }

    let mut app_list: Vec<App> = Vec::new();
    let mut rows: Vec<Line> = Vec::new();
    if kind == Kind::Start {
        app_list = apps::scan();
        rows = build_rows(&app_list, "");
    }

    let (pw, full_h) = match kind {
        Kind::Apple => (px(260.0, sc), apple_height(sc)),
        Kind::Calendar => (px(292.0, sc), calendar_height(sc)),
        Kind::Wifi => (px(312.0, sc), wifi_height(sc, nets.len(), false, false)),
        Kind::Start => (px(300.0, sc), start_height(sc)),
    };

    let screen_w = GetSystemMetrics(SM_CXSCREEN);
    let margin = px(8.0, sc);
    let x = match kind {
        Kind::Apple | Kind::Start => anchor.left,
        _ => anchor.right - pw,
    }
    .clamp(margin, (screen_w - pw - margin).max(margin));
    let y = anchor.bottom + px(6.0, sc);

    let Ok(hwnd) = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        w!("KachaPanel"),
        w!("Kacha Panel"),
        WS_POPUP,
        x,
        y,
        pw,
        1,
        HWND::default(),
        HMENU::default(),
        hinstance,
        None,
    ) else {
        return;
    };

    let fonts = Fonts {
        text: make_font(crate::fonts::FACE, px(14.0, sc), 400),
        small: make_font(crate::fonts::FACE, px(12.0, sc), 400),
        title: make_font(crate::fonts::FACE, px(17.0, sc), 700),
        icon: make_font("Segoe MDL2 Assets", px(14.0, sc), 400),
    };

    PANEL.with(|c| {
        *c.borrow_mut() = Some(Panel {
            hwnd,
            bar,
            kind,
            scale: sc,
            w: pw,
            full_h,
            cur_h: 1,
            anim_start: GetTickCount64(),
            fonts,
            hits: Vec::new(),
            hover: None,
            tracking: false,
            cal_year: now.wYear as i32,
            cal_month: now.wMonth as i32,
            wifi,
            nets,
            refreshes: 0,
            connecting: None,
            selected: None,
            join: None,
            pw: String::new(),
            notice: None,
            notice_at: 0,
            fresh: None,
            conn_ticks: 0,
            ticks: 0,
            apps: app_list,
            rows,
            query: String::new(),
            top: 0,
        });
    });

    load_visible_icons();
    crate::bar::enable_acrylic(hwnd, PANEL_TINT);
    apply_size(hwnd, pw, 1, sc);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let _ = SetForegroundWindow(hwnd); // supaya klik di luar menutup panel
    SetTimer(hwnd, TIMER_ANIM, 12, None);
    if kind == Kind::Wifi {
        SetTimer(hwnd, TIMER_WIFI, 1500, None);
    }
}

// ---------------------------------------------------------------------------
// Helper gambar
// ---------------------------------------------------------------------------

fn px(v: f32, sc: f32) -> i32 {
    (v * sc).round() as i32
}

fn rc(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

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

unsafe fn text(hdc: HDC, font: HFONT, s: &str, r: RECT, color: u32, align: DRAW_TEXT_FORMAT) {
    SelectObject(hdc, font);
    SetTextColor(hdc, COLORREF(color));
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    let mut rect = r;
    DrawTextW(
        hdc,
        &mut buf,
        &mut rect,
        align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
    );
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
                fill(hdc, rc(l + s, t + y, l + x, t + y + 1), color);
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

unsafe fn ellipse(hdc: HDC, r: RECT, color: u32) {
    let rad = (r.right - r.left + 1).min(r.bottom - r.top + 1) / 2;
    aa_shape(hdc, r, rad, color);
}

/// Gambar glyph ikon tepat di tengah `r` berdasarkan batas tinta glyph (bukan kotak font),
/// supaya ikon Segoe MDL2 benar-benar center di dalam lingkaran/tombol.
unsafe fn icon_center(hdc: HDC, font: HFONT, glyph: char, r: RECT, color: u32) {
    SelectObject(hdc, font);
    SetTextColor(hdc, COLORREF(color));
    let mut gm = GLYPHMETRICS::default();
    let mat = MAT2 {
        eM11: FIXED { fract: 0, value: 1 },
        eM12: FIXED { fract: 0, value: 0 },
        eM21: FIXED { fract: 0, value: 0 },
        eM22: FIXED { fract: 0, value: 1 },
    };
    let res = GetGlyphOutlineW(hdc, glyph as u32, GGO_METRICS, &mut gm, 0, None, &mat);
    if res == u32::MAX {
        text(hdc, font, &glyph.to_string(), r, color, DT_CENTER);
        return;
    }
    let (iw, ih) = (gm.gmBlackBoxX as i32, gm.gmBlackBoxY as i32);
    let cx = (r.left + r.right + 1) / 2;
    let cy = (r.top + r.bottom + 1) / 2;
    let x = cx - iw / 2 - gm.gmptGlyphOrigin.x;
    let baseline = cy - ih / 2 + gm.gmptGlyphOrigin.y;
    SetTextAlign(hdc, TA_BASELINE | TA_LEFT);
    let buf: Vec<u16> = glyph.to_string().encode_utf16().collect();
    let _ = TextOutW(hdc, x, baseline, &buf);
    SetTextAlign(hdc, TA_TOP | TA_LEFT);
}

/// Baris menu bergaya macOS: ikon, label, hint; saat hover jadi biru membulat.
unsafe fn menu_row(
    hdc: HDC,
    p: &mut Panel,
    r: RECT,
    act: Act,
    glyph: char,
    label: &str,
    hint: &str,
) {
    let sc = p.scale;
    let f = p.fonts;
    let hot = p.hover == Some(act);
    if hot {
        rround(hdc, r, px(6.0, sc), BLUE);
    }
    text(
        hdc,
        f.icon,
        &glyph.to_string(),
        rc(r.left + px(8.0, sc), r.top, r.left + px(34.0, sc), r.bottom),
        if hot { WHITE } else { DIM },
        DT_CENTER,
    );
    text(
        hdc,
        f.text,
        label,
        rc(r.left + px(42.0, sc), r.top, r.right - px(10.0, sc), r.bottom),
        WHITE,
        DT_LEFT,
    );
    if !hint.is_empty() {
        text(
            hdc,
            f.small,
            hint,
            rc(r.left + px(42.0, sc), r.top, r.right - px(12.0, sc), r.bottom),
            if hot { 0x00E6_E6E6 } else { DIM },
            DT_RIGHT,
        );
    }
    p.hits.push((r, act));
}

unsafe fn separator(hdc: HDC, x1: i32, x2: i32, y: i32) {
    fill(hdc, rc(x1, y, x2, y + 1), LINE);
}

// ---------------------------------------------------------------------------
// Menu Apple
// ---------------------------------------------------------------------------

fn apple_height(sc: f32) -> i32 {
    let mut h = px(6.0, sc) * 2;
    for row in MENU.iter() {
        h += match row {
            Row::Sep => px(9.0, sc),
            Row::Item(..) => px(30.0, sc),
        };
    }
    h
}

unsafe fn draw_apple(hdc: HDC, p: &mut Panel, w: i32) {
    let sc = p.scale;
    let pad = px(6.0, sc);
    let mut y = pad;
    for row in MENU.iter() {
        match row {
            Row::Sep => {
                separator(hdc, pad + px(8.0, sc), w - pad - px(8.0, sc), y + px(4.0, sc));
                y += px(9.0, sc);
            }
            Row::Item(id, glyph, label, hint) => {
                let r = rc(pad, y, w - pad, y + px(30.0, sc));
                menu_row(hdc, p, r, Act::Cmd(*id), *glyph, label, hint);
                y += px(30.0, sc);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Kalender
// ---------------------------------------------------------------------------

fn days_in_month(y: i32, m: i32) -> i32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

/// 0 = Minggu (algoritma Sakamoto).
fn weekday(y: i32, m: i32, d: i32) -> i32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    (y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d) % 7
}

fn calendar_height(sc: f32) -> i32 {
    px(14.0, sc) // atas
        + px(34.0, sc) // judul bulan
        + px(22.0, sc) // nama hari
        + 6 * px(32.0, sc) // 6 baris tanggal
        + px(10.0, sc) // pemisah
        + 2 * px(30.0, sc) // footer
        + px(8.0, sc)
}

unsafe fn draw_calendar(hdc: HDC, p: &mut Panel, w: i32) {
    let sc = p.scale;
    let f = p.fonts;
    let pad = px(14.0, sc);
    let now = GetLocalTime();
    let (ty, tm, td) = (now.wYear as i32, now.wMonth as i32, now.wDay as i32);
    let mut y = pad;

    // ---- judul bulan + tombol ‹ Today › ----
    let hh = px(34.0, sc);
    let btn = px(28.0, sc);
    let gap = px(2.0, sc);
    let by = y + (hh - btn) / 2;
    let next_r = rc(w - pad - btn, by, w - pad, by + btn);
    let today_w = px(54.0, sc);
    let today_r = rc(next_r.left - gap - today_w, by, next_r.left - gap, by + btn);
    let prev_r = rc(today_r.left - gap - btn, by, today_r.left - gap, by + btn);

    let title = format!("{} {}", MONTHS[(p.cal_month - 1) as usize], p.cal_year);
    text(
        hdc,
        f.title,
        &title,
        rc(pad, y, prev_r.left - px(4.0, sc), y + hh),
        WHITE,
        DT_LEFT,
    );
    for (r, act) in [
        (prev_r, Act::Prev),
        (today_r, Act::Today),
        (next_r, Act::Next),
    ] {
        if p.hover == Some(act) {
            rround(hdc, r, px(6.0, sc), SOFT);
        }
        match act {
            Act::Prev => text(hdc, f.icon, "\u{E76B}", r, WHITE, DT_CENTER),
            Act::Next => text(hdc, f.icon, "\u{E76C}", r, WHITE, DT_CENTER),
            _ => text(hdc, f.small, "Today", r, WHITE, DT_CENTER),
        }
        p.hits.push((r, act));
    }
    y += hh;

    // ---- nama hari ----
    let cw = (w - 2 * pad) as f32 / 7.0;
    let col_l = |c: i32| pad + (c as f32 * cw) as i32;
    let wk_h = px(22.0, sc);
    for c in 0..7 {
        let name = DOW[((WEEK_START + c) % 7) as usize];
        text(hdc, f.small, name, rc(col_l(c), y, col_l(c + 1), y + wk_h), FAINT, DT_CENTER);
    }
    y += wk_h;

    // ---- grid tanggal (selalu 6 baris supaya tinggi tetap) ----
    let first = weekday(p.cal_year, p.cal_month, 1);
    let lead = (first - WEEK_START + 7) % 7;
    let dim = days_in_month(p.cal_year, p.cal_month);
    let (py_, pm_) = if p.cal_month == 1 {
        (p.cal_year - 1, 12)
    } else {
        (p.cal_year, p.cal_month - 1)
    };
    let pdim = days_in_month(py_, pm_);
    let rh = px(32.0, sc);
    let d = px(26.0, sc);

    for i in 0..42i32 {
        let (day, in_month) = if i < lead {
            (pdim - lead + i + 1, false)
        } else if i - lead < dim {
            (i - lead + 1, true)
        } else {
            (i - lead - dim + 1, false)
        };
        let (row, col) = (i / 7, i % 7);
        let cell = rc(col_l(col), y + row * rh, col_l(col + 1), y + (row + 1) * rh);
        let is_today = in_month && p.cal_year == ty && p.cal_month == tm && day == td;
        if is_today {
            let cx = (cell.left + cell.right) / 2;
            let cy = (cell.top + cell.bottom) / 2;
            ellipse(hdc, rc(cx - d / 2, cy - d / 2, cx + d / 2, cy + d / 2), RED);
        }
        text(
            hdc,
            f.text,
            &day.to_string(),
            cell,
            if is_today || in_month { WHITE } else { FAINT },
            DT_CENTER,
        );
    }
    y += 6 * rh;

    // ---- footer ----
    separator(hdc, pad, w - pad, y + px(5.0, sc));
    y += px(10.0, sc);
    let fp = px(6.0, sc);
    let rh2 = px(30.0, sc);
    menu_row(
        hdc,
        p,
        rc(fp, y, w - fp, y + rh2),
        Act::OpenCalendar,
        '\u{E787}',
        "Open Calendar",
        "",
    );
    y += rh2;
    menu_row(
        hdc,
        p,
        rc(fp, y, w - fp, y + rh2),
        Act::DateSettings,
        '\u{E713}',
        "Date & Time Settings…",
        "",
    );
}

// ---------------------------------------------------------------------------
// Wi-Fi
// ---------------------------------------------------------------------------

const WIFI_MAX_ROWS: usize = 8;
const DETAIL_H: f32 = 66.0;

fn wifi_height(sc: f32, n: usize, sel: bool, joining: bool) -> i32 {
    if joining {
        return px(8.0, sc) + px(58.0, sc) + px(36.0, sc) + px(26.0, sc) + px(32.0, sc) + px(10.0, sc);
    }
    let rows = n.clamp(1, WIFI_MAX_ROWS) as i32;
    px(8.0, sc) // atas
        + px(58.0, sc) // header
        + rows * px(38.0, sc)
        + if sel { px(DETAIL_H, sc) } else { 0 }
        + px(8.0, sc)
}

/// Tombol kecil membulat. `primary` = biru, selain itu abu-abu (merah saat hover kalau `danger`).
unsafe fn button(hdc: HDC, p: &mut Panel, r: RECT, act: Act, label: &str, primary: bool, danger: bool) {
    let hot = p.hover == Some(act);
    let bg = if primary {
        if hot { BLUE_HI } else { BLUE }
    } else if hot {
        if danger { RED } else { LINE }
    } else {
        SOFT
    };
    rround(hdc, r, px(7.0, p.scale), bg);
    text(hdc, p.fonts.text, label, r, WHITE, DT_CENTER);
    p.hits.push((r, act));
}

unsafe fn draw_wifi(hdc: HDC, p: &mut Panel, w: i32) {
    let sc = p.scale;
    let f = p.fonts;
    let pad = px(8.0, sc);
    let mut y = pad;
    let joining = p.join.is_some();

    // ---- header ----
    let status = if let Some(j) = &p.join {
        format!("Password for {}", j.ssid)
    } else if p.wifi.is_none() {
        "No Wi-Fi adapter".to_string()
    } else if let Some(s) = &p.notice {
        s.clone()
    } else if let Some(s) = &p.connecting {
        format!("Connecting to {}…", s)
    } else if let Some(n) = p.nets.iter().find(|n| n.connected) {
        format!("Connected to {}", n.ssid)
    } else if p.nets.is_empty() && p.refreshes < 4 {
        "Looking for networks…".to_string()
    } else {
        "Not connected".to_string()
    };
    let inner = pad + px(10.0, sc);
    text(
        hdc,
        f.title,
        "Wi-Fi",
        rc(inner, y + px(6.0, sc), w - pad - px(50.0, sc), y + px(32.0, sc)),
        WHITE,
        DT_LEFT,
    );
    text(
        hdc,
        f.small,
        &status,
        rc(inner, y + px(32.0, sc), w - pad - px(50.0, sc), y + px(52.0, sc)),
        DIM,
        DT_LEFT,
    );
    if !joining {
        let b = px(28.0, sc);
        let scan_r = rc(w - inner - b, y + px(14.0, sc), w - inner, y + px(14.0, sc) + b);
        if p.hover == Some(Act::Scan) {
            rround(hdc, scan_r, px(6.0, sc), SOFT);
        }
        icon_center(hdc, f.icon, '\u{E72C}', scan_r, WHITE);
        p.hits.push((scan_r, Act::Scan));
    }
    y += px(58.0, sc);

    // ---- input password ----
    if joining {
        let field = rc(inner, y, w - inner, y + px(36.0, sc));
        rround(hdc, field, px(8.0, sc), SOFT);
        let fr = rc(field.left + px(12.0, sc), field.top, field.right - px(12.0, sc), field.bottom);
        if p.pw.is_empty() {
            text(hdc, f.text, "Password", fr, FAINT, DT_LEFT);
        } else {
            let dots = "•".repeat(p.pw.chars().count().min(28));
            text(hdc, f.text, &format!("{}|", dots), fr, WHITE, DT_LEFT);
        }
        y += px(36.0, sc);
        let hint_r = rc(inner, y, w - inner, y + px(26.0, sc));
        match &p.notice {
            Some(n) => text(hdc, f.small, n, hint_r, RED, DT_LEFT),
            None => text(hdc, f.small, "Enter to join · Esc to cancel", hint_r, FAINT, DT_LEFT),
        }
        y += px(26.0, sc);
        let (bw, bh, gap) = (px(80.0, sc), px(28.0, sc), px(8.0, sc));
        let join_r = rc(w - inner - bw, y + px(2.0, sc), w - inner, y + px(2.0, sc) + bh);
        let cancel_r = rc(join_r.left - gap - bw, join_r.top, join_r.left - gap, join_r.bottom);
        button(hdc, p, cancel_r, Act::JoinCancel, "Cancel", false, false);
        button(hdc, p, join_r, Act::JoinOk, "Join", true, false);
        return;
    }

    // ---- daftar jaringan ----
    let rh = px(38.0, sc);
    let n = p.nets.len().min(WIFI_MAX_ROWS);
    let sel_idx = p.sel_index();
    if n == 0 {
        let msg = if p.wifi.is_none() {
            "Turn on Wi-Fi or plug in an adapter"
        } else if p.refreshes < 4 {
            "Searching…"
        } else {
            "No networks found"
        };
        text(hdc, f.text, msg, rc(pad, y, w - pad, y + rh), DIM, DT_CENTER);
    }
    for i in 0..n {
        let (ssid, quality, secure, connected, sec, saved) = {
            let nt = &p.nets[i];
            (nt.ssid.clone(), nt.quality, nt.secure, nt.connected, nt.sec, !nt.profile.is_empty())
        };
        let r = rc(pad, y, w - pad, y + rh);
        let act = Act::Select(i);
        let is_sel = sel_idx == Some(i);
        let hot = !is_sel && p.hover == Some(act);
        if is_sel {
            rround(hdc, r, px(6.0, sc), SOFT);
        } else if hot {
            rround(hdc, r, px(6.0, sc), BLUE);
        }

        // ikon bulat di kiri (biru = tersambung)
        let d = px(26.0, sc);
        let cx = r.left + px(10.0, sc);
        let ct = r.top + (rh - d) / 2;
        let circle = rc(cx, ct, cx + d, ct + d);
        ellipse(hdc, circle, if connected { BLUE } else { OFF });
        icon_center(hdc, f.icon, '\u{E701}', circle, WHITE);

        // bar sinyal di kanan
        let bw = px(3.0, sc);
        let bg = px(2.0, sc);
        let lit = ((quality + 24) / 25).clamp(1, 4) as i32;
        let bars_w = 4 * bw + 3 * bg;
        let bars_l = r.right - px(12.0, sc) - bars_w;
        let base = r.top + (rh + px(13.0, sc)) / 2;
        for k in 0..4i32 {
            let bh = px(4.0 + 3.0 * k as f32, sc);
            let l = bars_l + k * (bw + bg);
            fill(hdc, rc(l, base - bh, l + bw, base), if k < lit { WHITE } else { OFF });
        }
        // gembok kalau berpassword
        let mut label_r = bars_l - px(8.0, sc);
        if secure {
            let lr = rc(bars_l - px(26.0, sc), r.top, bars_l - px(6.0, sc), r.bottom);
            text(hdc, f.icon, "\u{E72E}", lr, if hot { WHITE } else { DIM }, DT_CENTER);
            label_r = lr.left;
        }

        text(
            hdc,
            if connected { f.title } else { f.text },
            &ssid,
            rc(cx + d + px(10.0, sc), r.top, label_r, r.bottom),
            WHITE,
            DT_LEFT,
        );
        p.hits.push((r, act));
        y += rh;

        // ---- detail jaringan terpilih ----
        if is_sel {
            let dl = pad + px(46.0, sc);
            let state = if connected {
                "Connected"
            } else if saved {
                "Saved"
            } else {
                "New network"
            };
            let info = format!("{} · {}% signal · {}", sec.label(), quality, state);
            text(hdc, f.small, &info, rc(dl, y, w - pad - px(10.0, sc), y + px(24.0, sc)), DIM, DT_LEFT);
            let by = y + px(26.0, sc);
            let bh = px(28.0, sc);
            let gap = px(8.0, sc);
            let mut bx = dl;
            if connected {
                let br = rc(bx, by, bx + px(110.0, sc), by + bh);
                button(hdc, p, br, Act::Disconnect, "Disconnect", true, false);
                bx = br.right + gap;
            } else if saved || sec != Sec::Other {
                let label = if saved || sec == Sec::Open { "Connect" } else { "Connect…" };
                let br = rc(bx, by, bx + px(100.0, sc), by + bh);
                button(hdc, p, br, Act::Connect(i), label, true, false);
                bx = br.right + gap;
            } else {
                let tr = rc(bx, by, w - pad - px(10.0, sc), by + bh);
                text(hdc, f.small, "Not supported here", tr, FAINT, DT_LEFT);
            }
            if saved {
                let br = rc(bx, by, bx + px(80.0, sc), by + bh);
                button(hdc, p, br, Act::Forget(i), "Forget", false, true);
            }
            y += px(DETAIL_H, sc);
        }
    }
}

// ---------------------------------------------------------------------------
// Start Menu (daftar aplikasi + pencarian)
// ---------------------------------------------------------------------------

fn start_height(sc: f32) -> i32 {
    px(8.0, sc) + px(40.0, sc) + START_ROWS as i32 * px(START_ROW_H, sc) + px(8.0, sc)
}

/// Susun baris tampilan: tanpa query = header huruf + app; dengan query = hasil saja
/// (awalan cocok dulu, lalu yang mengandung).
fn build_rows(apps: &[App], query: &str) -> Vec<Line> {
    let q = query.trim().to_lowercase();
    let mut out = Vec::new();
    if q.is_empty() {
        let mut cur: Option<char> = None;
        for (i, a) in apps.iter().enumerate() {
            let h = a.initial();
            if cur != Some(h) {
                out.push(Line::Header(h));
                cur = Some(h);
            }
            out.push(Line::App(i));
        }
    } else {
        for (i, a) in apps.iter().enumerate() {
            if a.key.starts_with(&q) {
                out.push(Line::App(i));
            }
        }
        for (i, a) in apps.iter().enumerate() {
            if !a.key.starts_with(&q) && a.key.contains(&q) {
                out.push(Line::App(i));
            }
        }
    }
    out
}

/// Muat ikon untuk baris yang sedang terlihat. Panggilan shell dilakukan di luar
/// borrow PANEL supaya aman kalau ada pesan yang masuk ke wndproc.
unsafe fn load_visible_icons() {
    let todo: Vec<(usize, Vec<u16>)> = PANEL.with(|c| {
        let b = c.borrow();
        let Some(p) = b.as_ref() else {
            return Vec::new();
        };
        if p.kind != Kind::Start {
            return Vec::new();
        }
        let n = p.rows.len();
        let first = p.top.min(n.saturating_sub(START_ROWS));
        (first..(first + START_ROWS).min(n))
            .filter_map(|ri| match p.rows[ri] {
                Line::App(ai) if !p.apps[ai].tried => Some((ai, p.apps[ai].path.clone())),
                _ => None,
            })
            .collect()
    });
    for (ai, path) in todo {
        let icon = apps::fetch_icon(&path);
        let stored = PANEL.with(|c| {
            if let Some(p) = c.borrow_mut().as_mut() {
                if let Some(a) = p.apps.get_mut(ai) {
                    a.tried = true;
                    a.icon = icon;
                    return true;
                }
            }
            false
        });
        if !stored {
            if let Some(i) = icon {
                let _ = DestroyIcon(i);
            }
        }
    }
}

unsafe fn text_width(hdc: HDC, font: HFONT, s: &str) -> i32 {
    SelectObject(hdc, font);
    let buf: Vec<u16> = s.encode_utf16().collect();
    let mut sz = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &buf, &mut sz);
    sz.cx
}

unsafe fn draw_start(hdc: HDC, p: &mut Panel, w: i32) {
    let sc = p.scale;
    let f = p.fonts;
    let pad = px(8.0, sc);

    // ---- kolom pencarian ----
    let sb = rc(pad, pad, w - pad, pad + px(32.0, sc));
    rround(hdc, sb, px(8.0, sc), SOFT);
    text(
        hdc,
        f.icon,
        "\u{E721}",
        rc(sb.left + px(6.0, sc), sb.top, sb.left + px(32.0, sc), sb.bottom),
        DIM,
        DT_CENTER,
    );
    let tx = sb.left + px(38.0, sc);
    let caret_x = if p.query.is_empty() {
        text(
            hdc,
            f.text,
            "Search apps…",
            rc(tx + px(4.0, sc), sb.top, sb.right - px(8.0, sc), sb.bottom),
            FAINT,
            DT_LEFT,
        );
        tx
    } else {
        let tw = text_width(hdc, f.text, &p.query);
        text(
            hdc,
            f.text,
            &p.query,
            rc(tx, sb.top, sb.right - px(8.0, sc), sb.bottom),
            WHITE,
            DT_LEFT,
        );
        (tx + tw + px(1.0, sc)).min(sb.right - px(10.0, sc))
    };
    fill(
        hdc,
        rc(
            caret_x,
            sb.top + px(8.0, sc),
            caret_x + px(1.0, sc).max(1),
            sb.bottom - px(8.0, sc),
        ),
        WHITE,
    );

    // ---- daftar ----
    let list_top = pad + px(40.0, sc);
    let rh = px(START_ROW_H, sc);
    let n = p.rows.len();
    let first = p.top.min(n.saturating_sub(START_ROWS));
    let first_app = p.rows.iter().position(|l| matches!(l, Line::App(_)));
    let right = w - pad - px(6.0, sc);

    if n == 0 {
        text(
            hdc,
            f.text,
            "No apps found",
            rc(pad, list_top, w - pad, list_top + rh * 2),
            DIM,
            DT_CENTER,
        );
    }

    let mut y = list_top;
    for ri in first..(first + START_ROWS).min(n) {
        let r = rc(pad, y, right, y + rh);
        let line = p.rows[ri];
        match line {
            Line::Header(ch) => {
                text(
                    hdc,
                    f.small,
                    &ch.to_string(),
                    rc(r.left + px(12.0, sc), r.top, r.right, r.bottom),
                    DIM,
                    DT_LEFT,
                );
            }
            Line::App(ai) => {
                let act = Act::Launch(ai);
                let hot = p.hover == Some(act);
                if hot {
                    rround(hdc, r, px(6.0, sc), BLUE);
                } else if !p.query.is_empty() && Some(ri) == first_app {
                    rround(hdc, r, px(6.0, sc), SOFT); // hasil teratas (dipilih oleh Enter)
                }
                let isz = px(16.0, sc);
                if let Some(ic) = p.apps[ai].icon {
                    let _ = DrawIconEx(
                        hdc,
                        r.left + px(10.0, sc),
                        r.top + (rh - isz) / 2,
                        ic,
                        isz,
                        isz,
                        0,
                        HBRUSH::default(),
                        DI_NORMAL,
                    );
                }
                text(
                    hdc,
                    f.text,
                    &p.apps[ai].name,
                    rc(r.left + px(36.0, sc), r.top, r.right - px(8.0, sc), r.bottom),
                    WHITE,
                    DT_LEFT,
                );
                p.hits.push((r, act));
            }
        }
        y += rh;
    }

    // ---- indikator scroll ----
    if n > START_ROWS {
        let track = START_ROWS as i32 * rh;
        let thumb = (track * START_ROWS as i32 / n as i32).max(px(16.0, sc));
        let max_first = (n - START_ROWS) as i32;
        let ty = list_top + (track - thumb) * first as i32 / max_first;
        let x2 = w - pad;
        rround(hdc, rc(x2 - px(3.0, sc), ty, x2, ty + thumb), px(1.0, sc), OFF);
    }
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

unsafe fn apply_size(hwnd: HWND, w: i32, h: i32, sc: f32) {
    let _ = SetWindowPos(
        hwnd,
        HWND::default(),
        0,
        0,
        w,
        h,
        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
    let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, px(24.0, sc), px(24.0, sc));
    SetWindowRgn(hwnd, rgn, TRUE);
}

unsafe fn invalidate(hwnd: HWND) {
    let _ = InvalidateRect(hwnd, None, false);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    PANEL.with(|c| {
        let mut b = c.borrow_mut();
        let Some(p) = b.as_mut() else {
            return;
        };
        let (w, fh, ch) = (p.w, p.full_h, p.cur_h);
        if w <= 0 || fh <= 0 || ch <= 0 {
            return;
        }

        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w, fh);
        let old = SelectObject(mem, bmp);

        // Hitam = "kaca" (acrylic terlihat), sama seperti menu bar.
        let full = rc(0, 0, w, fh);
        FillRect(mem, &full, HBRUSH(GetStockObject(BLACK_BRUSH).0));
        SetBkMode(mem, TRANSPARENT);

        p.hits.clear();
        match p.kind {
            Kind::Apple => draw_apple(mem, p, w),
            Kind::Calendar => draw_calendar(mem, p, w),
            Kind::Wifi => draw_wifi(mem, p, w),
            Kind::Start => draw_start(mem, p, w),
        }

        // Saat animasi, isi panel "meluncur" turun dari bawah menu bar.
        let _ = BitBlt(hdc, 0, 0, w, ch, mem, 0, fh - ch, SRCCOPY);

        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
    });

    let _ = EndPaint(hwnd, &ps);
}

unsafe fn tick_anim(hwnd: HWND) {
    let r = PANEL.with(|c| {
        let mut b = c.borrow_mut();
        let p = b.as_mut()?;
        let t = ((GetTickCount64().saturating_sub(p.anim_start)) as f32 / ANIM_MS).min(1.0);
        let e = 1.0 - (1.0 - t).powi(3); // ease-out
        p.cur_h = if t >= 1.0 {
            p.full_h
        } else {
            ((p.full_h as f32 * e) as i32).max(1)
        };
        Some((p.w, p.cur_h, p.scale, t >= 1.0))
    });
    if let Some((w, h, sc, done)) = r {
        apply_size(hwnd, w, h, sc);
        invalidate(hwnd);
        if done {
            let _ = KillTimer(hwnd, TIMER_ANIM);
        }
    }
}

unsafe fn tick_wifi(hwnd: HWND) {
    let r = PANEL.with(|c| {
        let mut b = c.borrow_mut();
        let p = b.as_mut()?;
        p.ticks = p.ticks.wrapping_add(1);
        p.refreshes = p.refreshes.saturating_add(1);
        // Scan ulang berkala supaya daftar & sinyal tetap segar selama panel terbuka.
        if p.ticks % 5 == 0 {
            if let Some(wf) = &p.wifi {
                wf.scan();
            }
        }
        let nets = p.wifi.as_ref()?.networks();
        p.nets = nets;

        // Hasil percobaan connect.
        if let Some(ssid) = p.connecting.clone() {
            p.conn_ticks += 1;
            if p.nets.iter().any(|n| n.connected && n.ssid == ssid) {
                p.connecting = None;
                p.fresh = None;
                p.notice = None;
            } else if p.conn_ticks >= 8 {
                p.connecting = None;
                if let Some(prof) = p.fresh.take() {
                    // Profile baru gagal (umumnya password salah): jangan disimpan.
                    if let Some(wf) = &p.wifi {
                        wf.forget(&prof);
                    }
                    p.say(format!("Couldn't join {} – check the password", ssid));
                } else {
                    p.say(format!("Couldn't connect to {}", ssid));
                }
                if let Some(nn) = p.wifi.as_ref().map(|wf| wf.networks()) {
                    p.nets = nn;
                }
            }
        }
        if p.notice.is_some() && p.join.is_none() && p.ticks.wrapping_sub(p.notice_at) >= 4 {
            p.notice = None;
        }
        if p.sel_index().is_none() {
            p.selected = None;
        }

        let nh = wifi_height(p.scale, p.nets.len(), p.sel_index().is_some(), p.join.is_some());
        let resized = nh != p.full_h;
        if resized {
            p.full_h = nh;
            p.cur_h = nh;
        }
        Some((p.w, p.full_h, p.scale, resized))
    });
    if let Some((w, h, sc, resized)) = r {
        if resized {
            apply_size(hwnd, w, h, sc);
        }
        invalidate(hwnd);
    }
}

/// Hitung ulang tinggi panel Wi-Fi setelah state berubah (pilih jaringan, mode password, dst).
unsafe fn relayout(hwnd: HWND) {
    let r = PANEL.with(|c| {
        let mut b = c.borrow_mut();
        let p = b.as_mut()?;
        let nh = wifi_height(p.scale, p.nets.len(), p.sel_index().is_some(), p.join.is_some());
        let resized = nh != p.full_h;
        p.full_h = nh;
        p.cur_h = nh;
        Some((p.w, nh, p.scale, resized))
    });
    if let Some((w, h, sc, resized)) = r {
        if resized {
            apply_size(hwnd, w, h, sc);
        }
        invalidate(hwnd);
    }
}

/// Teks dari clipboard (untuk paste password).
unsafe fn clipboard_text() -> Option<String> {
    if OpenClipboard(HWND::default()).is_err() {
        return None;
    }
    let mut out = None;
    if let Ok(h) = GetClipboardData(13) {
        // 13 = CF_UNICODETEXT
        let g = HGLOBAL(h.0);
        let ptr = GlobalLock(g) as *const u16;
        if !ptr.is_null() {
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)));
            let _ = GlobalUnlock(g);
        }
    }
    let _ = CloseClipboard();
    out
}

fn act_at(x: i32, y: i32) -> Option<Act> {
    PANEL.with(|c| {
        let b = c.borrow();
        let p = b.as_ref()?;
        if p.cur_h < p.full_h {
            return None; // abaikan mouse selama animasi
        }
        p.hits
            .iter()
            .find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
            .map(|(_, a)| *a)
    })
}

fn xy(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam.0 & 0xFFFF) as i16 as i32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    )
}

fn shift_month(p: &mut Panel, d: i32) {
    p.cal_month += d;
    if p.cal_month < 1 {
        p.cal_month = 12;
        p.cal_year -= 1;
    } else if p.cal_month > 12 {
        p.cal_month = 1;
        p.cal_year += 1;
    }
}

unsafe fn do_action(hwnd: HWND, act: Act) {
    match act {
        Act::Cmd(id) => {
            let bar = PANEL.with(|c| c.borrow().as_ref().map(|p| p.bar));
            let _ = DestroyWindow(hwnd);
            if let Some(bar) = bar {
                let _ = PostMessageW(bar, WM_PANEL_CMD, WPARAM(id), LPARAM(0));
            }
        }
        Act::Prev | Act::Next => {
            PANEL.with(|c| {
                if let Some(p) = c.borrow_mut().as_mut() {
                    shift_month(p, if act == Act::Next { 1 } else { -1 });
                }
            });
            invalidate(hwnd);
        }
        Act::Today => {
            let now = GetLocalTime();
            PANEL.with(|c| {
                if let Some(p) = c.borrow_mut().as_mut() {
                    p.cal_year = now.wYear as i32;
                    p.cal_month = now.wMonth as i32;
                }
            });
            invalidate(hwnd);
        }
        Act::OpenCalendar => {
            let _ = DestroyWindow(hwnd);
            crate::bar::open("outlookcal:", None, SW_SHOWNORMAL);
        }
        Act::DateSettings => {
            let _ = DestroyWindow(hwnd);
            crate::bar::open("ms-settings:dateandtime", None, SW_SHOWNORMAL);
        }
        Act::Scan => {
            PANEL.with(|c| {
                if let Some(p) = c.borrow_mut().as_mut() {
                    if let Some(wf) = &p.wifi {
                        wf.scan();
                    }
                    p.refreshes = 0;
                    p.notice = None;
                }
            });
            invalidate(hwnd);
        }
        Act::Launch(ai) => {
            let path = PANEL.with(|c| {
                c.borrow()
                    .as_ref()
                    .and_then(|p| p.apps.get(ai).map(|a| a.path.clone()))
            });
            let _ = DestroyWindow(hwnd);
            if let Some(path) = path {
                apps::launch(&path);
            }
        }
        Act::Select(i) => {
            PANEL.with(|c| {
                if let Some(p) = c.borrow_mut().as_mut() {
                    if let Some(n) = p.nets.get(i) {
                        let ssid = n.ssid.clone();
                        p.selected = if p.selected.as_deref() == Some(ssid.as_str()) {
                            None
                        } else {
                            Some(ssid)
                        };
                        p.notice = None;
                        p.hover = None;
                    }
                }
            });
            relayout(hwnd);
        }
        Act::Connect(i) => {
            PANEL.with(|c| {
                if let Some(p) = c.borrow_mut().as_mut() {
                    let Some(n) = p.nets.get(i).cloned() else {
                        return;
                    };
                    p.notice = None;
                    if !n.profile.is_empty() {
                        let ok = p.wifi.as_ref().map_or(false, |wf| wf.connect(&n.profile));
                        if ok {
                            p.connecting = Some(n.ssid.clone());
                            p.conn_ticks = 0;
                            p.fresh = None;
                        } else {
                            p.say(format!("Couldn't connect to {}", n.ssid));
                        }
                    } else {
                        match n.sec {
                            Sec::Open => {
                                let r = p.wifi.as_ref().and_then(|wf| wf.connect_new(&n.ssid, n.sec, ""));
                                match r {
                                    Some(prof) => {
                                        p.connecting = Some(n.ssid.clone());
                                        p.conn_ticks = 0;
                                        p.fresh = Some(prof);
                                    }
                                    None => p.say(format!("Couldn't connect to {}", n.ssid)),
                                }
                            }
                            Sec::Psk(..) => {
                                p.join = Some(Join { ssid: n.ssid.clone(), sec: n.sec });
                                p.pw.clear();
                            }
                            Sec::Other => {}
                        }
                    }
                }
            });
            relayout(hwnd);
        }
        Act::Disconnect => {
            PANEL.with(|c| {
                if let Some(p) = c.borrow_mut().as_mut() {
                    if let Some(wf) = &p.wifi {
                        wf.disconnect();
                    }
                    for n in p.nets.iter_mut() {
                        n.connected = false;
                    }
                    p.connecting = None;
                    p.fresh = None;
                    p.notice = None;
                }
            });
            invalidate(hwnd);
        }
        Act::Forget(i) => {
            PANEL.with(|c| {
                if let Some(p) = c.borrow_mut().as_mut() {
                    let Some(n) = p.nets.get(i).cloned() else {
                        return;
                    };
                    let ok = p.wifi.as_ref().map_or(false, |wf| wf.forget(&n.profile));
                    if ok {
                        if let Some(m) = p.nets.get_mut(i) {
                            m.profile.clear();
                            m.connected = false;
                        }
                        p.selected = None;
                        p.connecting = None;
                        p.say(format!("Forgot {}", n.ssid));
                    } else {
                        p.say(format!("Couldn't forget {}", n.ssid));
                    }
                }
            });
            relayout(hwnd);
        }
        Act::JoinOk => {
            PANEL.with(|c| {
                if let Some(p) = c.borrow_mut().as_mut() {
                    let Some((ssid, sec)) = p.join.as_ref().map(|j| (j.ssid.clone(), j.sec)) else {
                        return;
                    };
                    let sae = matches!(sec, Sec::Psk("WPA3SAE", _));
                    if p.pw.is_empty() || (!sae && p.pw.chars().count() < 8) {
                        p.say("Password must be at least 8 characters".to_string());
                        return;
                    }
                    let key = p.pw.clone();
                    let r = p.wifi.as_ref().and_then(|wf| wf.connect_new(&ssid, sec, &key));
                    match r {
                        Some(prof) => {
                            p.connecting = Some(ssid);
                            p.conn_ticks = 0;
                            p.fresh = Some(prof);
                            p.join = None;
                            p.pw.clear();
                            p.notice = None;
                        }
                        None => p.say("Couldn't join this network".to_string()),
                    }
                }
            });
            relayout(hwnd);
        }
        Act::JoinCancel => {
            PANEL.with(|c| {
                if let Some(p) = c.borrow_mut().as_mut() {
                    p.join = None;
                    p.pw.clear();
                    p.notice = None;
                }
            });
            relayout(hwnd);
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
        WM_TIMER => {
            match wparam.0 {
                TIMER_ANIM => tick_anim(hwnd),
                TIMER_WIFI => tick_wifi(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // Kehilangan fokus (klik di luar panel) = tutup.
            if (wparam.0 & 0xFFFF) == 0 && PANEL.with(|c| c.borrow().is_some()) {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wparam.0 as u16 == VK_ESCAPE.0 {
                let joining = PANEL.with(|c| c.borrow().as_ref().map_or(false, |p| p.join.is_some()));
                if joining {
                    do_action(hwnd, Act::JoinCancel); // Esc di input password = batal, bukan tutup panel
                } else {
                    let _ = DestroyWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_CHAR => {
            let code = wparam.0 as u32;

            // Mengetik password Wi-Fi (Ctrl+V = paste).
            let mut wifi_typing = false;
            let mut submit = false;
            let mut redraw = false;
            PANEL.with(|cell| {
                if let Some(p) = cell.borrow_mut().as_mut() {
                    if p.kind == Kind::Wifi && p.join.is_some() {
                        wifi_typing = true;
                        match code {
                            0x08 => redraw = p.pw.pop().is_some(),
                            0x0D => submit = true,
                            0x16 => {
                                if let Some(t) = clipboard_text() {
                                    for ch in t.chars().filter(|c| !c.is_control()) {
                                        if p.pw.chars().count() < 64 {
                                            p.pw.push(ch);
                                        }
                                    }
                                    p.notice = None;
                                    redraw = true;
                                }
                            }
                            c if c >= 0x20 && c != 0x7F => {
                                if let Some(ch) = char::from_u32(c) {
                                    if p.pw.chars().count() < 64 {
                                        p.pw.push(ch);
                                        p.notice = None;
                                        redraw = true;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            });
            if wifi_typing {
                if submit {
                    do_action(hwnd, Act::JoinOk);
                } else if redraw {
                    invalidate(hwnd);
                }
                return LRESULT(0);
            }

            // Mengetik di kolom pencarian Start Menu.
            let mut changed = false;
            let mut launch: Option<Vec<u16>> = None;
            PANEL.with(|cell| {
                if let Some(p) = cell.borrow_mut().as_mut() {
                    if p.kind != Kind::Start {
                        return;
                    }
                    match code {
                        0x08 => {
                            if p.query.pop().is_some() {
                                changed = true;
                            }
                        }
                        0x0D => {
                            let top = p.rows.iter().copied().find(|l| matches!(l, Line::App(_)));
                            if let Some(Line::App(ai)) = top {
                                launch = Some(p.apps[ai].path.clone());
                            }
                        }
                        c if c >= 0x20 && c != 0x7F => {
                            if let Some(ch) = char::from_u32(c) {
                                p.query.push(ch);
                                changed = true;
                            }
                        }
                        _ => {}
                    }
                    if changed {
                        p.rows = build_rows(&p.apps, &p.query);
                        p.top = 0;
                        p.hover = None;
                    }
                }
            });
            if let Some(path) = launch {
                let _ = DestroyWindow(hwnd);
                apps::launch(&path);
            } else if changed {
                load_visible_icons();
                invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lparam);
            let new_hover = act_at(x, y);
            let (changed, need_track) = PANEL.with(|c| match c.borrow_mut().as_mut() {
                Some(p) => {
                    let ch = p.hover != new_hover;
                    p.hover = new_hover;
                    let nt = !p.tracking;
                    p.tracking = true;
                    (ch, nt)
                }
                None => (false, false),
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
            if changed {
                invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            PANEL.with(|c| {
                if let Some(p) = c.borrow_mut().as_mut() {
                    p.hover = None;
                    p.tracking = false;
                }
            });
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lparam);
            if let Some(act) = act_at(x, y) {
                do_action(hwnd, act);
            }
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
            let mut changed = false;
            PANEL.with(|c| {
                if let Some(p) = c.borrow_mut().as_mut() {
                    match p.kind {
                        Kind::Calendar => {
                            shift_month(p, if delta > 0 { -1 } else { 1 });
                            changed = true;
                        }
                        Kind::Start => {
                            let step = ((delta as i32 / 120).abs().max(1) * 3) as usize;
                            let max = p.rows.len().saturating_sub(START_ROWS);
                            p.top = p.top.min(max);
                            p.top = if delta > 0 {
                                p.top.saturating_sub(step)
                            } else {
                                (p.top + step).min(max)
                            };
                            p.hover = None;
                            changed = true;
                        }
                        _ => {}
                    }
                }
            });
            if changed {
                load_visible_icons();
                invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let p = PANEL.with(|c| c.borrow_mut().take());
            if let Some(p) = p {
                let _ = KillTimer(hwnd, TIMER_ANIM);
                let _ = KillTimer(hwnd, TIMER_WIFI);
                let _ = DeleteObject(p.fonts.text);
                let _ = DeleteObject(p.fonts.small);
                let _ = DeleteObject(p.fonts.title);
                let _ = DeleteObject(p.fonts.icon);
                let _ = PostMessageW(p.bar, WM_PANEL_CLOSED, WPARAM(0), LPARAM(0));
                // `p.wifi` ikut di-drop di sini -> handle WLAN ditutup.
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}