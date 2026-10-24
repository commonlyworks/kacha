//! Dock bergaya macOS di tengah bawah layar.
//!
//! - Jendela dock selalu persis seukuran ikon + margin. Saat hover, ikon membesar dan dock ikut
//!   melebar/meninggi (jendela di-resize tiap frame, region membulat dipasang ulang), jadi tidak
//!   ada ikon yang keluar dari dock dan tidak ada area acrylic yang menganggur.
//! - Label nama aplikasi = jendela acrylic kecil terpisah (seukuran pil labelnya).
//! - Autohide: dock terbuka penuh hanya saat desktop kosong (tidak ada jendela aplikasi yang
//!   tidak minimized). Kalau ada aplikasi terbuka, dock tersembunyi dan muncul saat kursor
//!   menyentuh tepi paling bawah layar (jendela "hotspot" tipis), lalu sembunyi lagi.
//! - Ikon diambil 128/256px dari shell, dibuat mipmap, lalu di-resample bilinear sendiri
//!   (bukan StretchBlt/AlphaBlend scaling) supaya tajam saat zoom.
//! - Pinned dari %AppData%\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar
//!   (urutan dari Taskband\Favorites bila terbaca, kalau tidak urut nama file).
//! - Timer animasi (16 ms) hanya hidup saat ada animasi; mati saat idle.
//! - Tidak ada thread latar belakang; tidak ada borrow RefCell saat memanggil Win32.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;

use windows::core::{w, IUnknown, Interface, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, BOOL, COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, TRUE,
    WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DwmSetWindowAttribute, DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR,
    DWMWA_CLOAKED, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWM_SYSTEMBACKDROP_TYPE,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::{FILE_FLAGS_AND_ATTRIBUTES, WIN32_FIND_DATAW};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IBindCtx, IPersistFile,
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_BINARY};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, IShellLinkW, SHCreateItemFromParsingName, SHGetFileInfoW,
    ShellExecuteW, ShellLink, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SIIGBF,
    SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::settings as conf;

// ---------------------------------------------------------------------------
// Pengaturan (silakan ubah)
// ---------------------------------------------------------------------------

/// Warna latar dock saat TIDAK glass (Windows 10 / Windows 11 lama), format 0x00BBGGRR.
/// Abu-abu gelap (RGB 38,38,42), sengaja bukan hitam. Ubah sesuka hati.
const SOLID_BG: u32 = 0x002A_2626;
// Ukuran ikon, zoom, jarak, dan margin dock: dari settings.rs (diubah user lewat "Kacha Settings…").
fn cfg_icon() -> f32 {
    conf::val(conf::DOCK_ICON) as f32
}
fn cfg_zoom() -> f32 {
    conf::val(conf::DOCK_ZOOM) as f32 / 100.0
}
fn cfg_gap() -> f32 {
    conf::val(conf::DOCK_SPACING) as f32
}
/// Margin kiri/kanan dock ke ikon.
fn cfg_pad_x() -> f32 {
    conf::val(conf::DOCK_PAD) as f32
}
/// Margin atas dock ke ikon.
fn cfg_pad_top() -> f32 {
    cfg_pad_x() * 7.0 / 8.0
}
fn cfg_bottom() -> f32 {
    conf::val(conf::DOCK_BOTTOM) as f32
}
const ZOOM_RANGE: f32 = 2.2;
/// Ruang di bawah ikon (tempat titik merah "sedang berjalan").
const DOT_AREA: f32 = 10.0;
const RADIUS: f32 = 18.0;
/// Radius sudut bawaan DWM (Windows 11) untuk DWMWCP_ROUND, dalam px pada 96 DPI.
const DWM_RADIUS: f32 = 8.0;
const ANIM_TAU_MS: f32 = 55.0;
const SLIDE_TAU_MS: f32 = 70.0;
/// Tinggi hotspot di tepi bawah layar (px pada 96 DPI).
const HOTSPOT_H: f32 = 4.0;
/// Jeda sebelum dock disembunyikan setelah kursor pergi (ms).
const HIDE_DELAY_MS: u32 = 450;

// ---------------------------------------------------------------------------

const TIMER_ANIM: usize = 1;
const TIMER_HIDE: usize = 2;
const WM_MOUSELEAVE: u32 = 0x02A3;

struct Level {
    w: i32,
    h: i32,
    /// BGRA premultiplied, top-down.
    px: Vec<u32>,
}

enum Icon {
    None,
    /// Mipmap: level terbesar dulu.
    Pix(Vec<Level>),
    Hicon(HICON),
}

impl Drop for Icon {
    fn drop(&mut self) {
        unsafe {
            if let Icon::Hicon(i) = self {
                let _ = DestroyIcon(*i);
            }
        }
    }
}

struct Item {
    name: String,
    name_w: i32,
    lnk: Option<Vec<u16>>,
    exe_l: String,
    exe_name_l: String,
    stem_l: String,
    pinned: bool,
    hwnd: Option<HWND>,
    icon: Icon,
    cur: f32,
}

struct RunWin {
    hwnd: HWND,
    exe: String,
    exe_l: String,
    name_l: String,
}

enum TipCmd {
    Hide,
    Show { x: i32, y: i32, w: i32, h: i32 },
}

struct Dock {
    hwnd: HWND,
    tip: HWND,
    hot: HWND,
    scale: f32,
    com: bool,
    font: HFONT,
    fetch: i32,
    scratch: HBITMAP,
    scratch_bits: *mut u32,
    scratch_px: i32,
    win_x: i32,
    shown_y: i32,
    cur_y: i32,
    win_w: i32,
    win_h: i32,
    /// Posisi/ukuran jendela yang terakhir benar-benar diterapkan (x, y, w, h).
    applied: (i32, i32, i32, i32),
    /// Posisi X kursor (layar) terakhir; dock di-anchor ke sini supaya ikon di bawah kursor
    /// tetap di bawah kursor ketika dock melebar.
    anchor_x: i32,
    /// 0 = dock ukuran istirahat, 1 = ukuran penuh saat hover (animasi halus).
    grow: f32,
    /// true = sudut dibulatkan DWM (blur ikut terpotong rapi); false = fallback SetWindowRgn.
    dwm_round: bool,
    /// true = latar glass (Windows 11 22H2+); false = warna solid SOLID_BG.
    glass: bool,
    tip_glass: bool,
    items: Vec<Item>,
    /// Posisi kursor dalam koordinat LAYAR (bukan klien), karena jendela berubah ukuran.
    mouse: Option<(i32, i32)>,
    tracking: bool,
    animating: bool,
    last_tick: u64,
    hover: Option<usize>,
    // autohide
    desktop_empty: bool,
    hot_on: bool,
    slide: f32,
    visible: bool,
    // tip
    tip_text: String,
    tip_key: Option<(usize, i32, i32, i32, i32)>,
    // layout
    lefts: Vec<i32>,
    sizes: Vec<i32>,
    sep_x: Option<i32>,
    icon_bottom: i32,
}

thread_local! {
    static DOCK: RefCell<Option<Dock>> = RefCell::new(None);
    static BUSY: Cell<bool> = Cell::new(false);
    static FULLSCREEN: Cell<bool> = Cell::new(false);
}

// ---------------------------------------------------------------------------
// Helper umum
// ---------------------------------------------------------------------------

fn px(v: f32, sc: f32) -> i32 {
    (v * sc).round() as i32
}

fn xy(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam.0 & 0xFFFF) as i16 as i32,
        ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    )
}

fn file_name_of(s: &str) -> &str {
    s.rsplit(|c| c == '\\' || c == '/').next().unwrap_or(s)
}

fn stem_of(s: &str) -> String {
    match s.rfind('.') {
        Some(i) if i > 0 => s[..i].to_string(),
        _ => s.to_string(),
    }
}

fn capitalize(s: &str) -> String {
    let mut ch = s.chars();
    match ch.next() {
        Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
        None => String::new(),
    }
}

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

unsafe fn measure(font: HFONT, s: &str) -> i32 {
    let hdc = GetDC(HWND::default());
    let old = SelectObject(hdc, font);
    let buf: Vec<u16> = s.encode_utf16().collect();
    let mut sz = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &buf, &mut sz);
    SelectObject(hdc, old);
    ReleaseDC(HWND::default(), hdc);
    sz.cx
}

unsafe fn fill(hdc: HDC, r: RECT, color: u32) {
    let b = CreateSolidBrush(COLORREF(color));
    FillRect(hdc, &r, b);
    let _ = DeleteObject(b);
}

unsafe fn rround(hdc: HDC, r: RECT, radius: i32, color: u32) {
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

/// Titik biru langit dengan gradasi (lingkaran konsentris gelap -> terang).
unsafe fn sky_dot(hdc: HDC, cx: i32, cy: i32, r: i32) {
    const LAYERS: [(f32, u32); 4] = [
        (1.00, 0x00A8_7F2F),
        (0.80, 0x00D4_A84F),
        (0.58, 0x00EB_CE87),
        (0.34, 0x00FF_EFCD),
    ];
    for (f, color) in LAYERS {
        let rad = ((r as f32 * f).round() as i32).max(1);
        disc(hdc, cx, cy, rad, color);
    }
}

unsafe fn draw_label(hdc: HDC, font: HFONT, s: &str, r: RECT) {
    SelectObject(hdc, font);
    SetTextColor(hdc, COLORREF(0x00FF_FFFF));
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    let mut rect = r;
    DrawTextW(
        hdc,
        &mut buf,
        &mut rect,
        DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
    );
}

unsafe fn invalidate(hwnd: HWND) {
    let _ = InvalidateRect(hwnd, None, false);
}

/// Minta DWM membulatkan sudut jendela (Windows 11). Blur acrylic ikut terpotong mengikuti
/// bentuk ini, tidak seperti SetWindowRgn yang diabaikan oleh blur. Mengembalikan false di Windows 10.
unsafe fn round_corners(hwnd: HWND) -> bool {
    let pref = DWMWCP_ROUND;
    let ok = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE,
        &pref as *const DWM_WINDOW_CORNER_PREFERENCE as *const c_void,
        size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
    )
    .is_ok();
    if ok {
        // Tanpa border sistem; kita gambar garis tepi sendiri.
        let none: u32 = 0xFFFF_FFFE;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &none as *const u32 as *const c_void,
            size_of::<u32>() as u32,
        );
    }
    ok
}

#[repr(C)]
struct DwmMargins {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

#[link(name = "dwmapi")]
extern "system" {
    fn DwmExtendFrameIntoClientArea(hwnd: HWND, margins: *const DwmMargins) -> i32;
}

/// Acrylic resmi Windows 11 (22H2+, build 22621+). Berbeda dengan accent policy di bar.rs yang
/// selalu memblur seluruh persegi jendela, backdrop ini dipotong DWM mengikuti sudut bulat
/// jendela. Mengembalikan false kalau tidak didukung (dock memakai warna solid).
unsafe fn enable_backdrop(hwnd: HWND) -> bool {
    let dark: i32 = 1;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE,
        &dark as *const i32 as *const c_void,
        size_of::<i32>() as u32,
    );
    let t = DWMSBT_TRANSIENTWINDOW;
    let ok = DwmSetWindowAttribute(
        hwnd,
        DWMWA_SYSTEMBACKDROP_TYPE,
        &t as *const DWM_SYSTEMBACKDROP_TYPE as *const c_void,
        size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
    )
    .is_ok();
    if ok {
        let m = DwmMargins { left: -1, right: -1, top: -1, bottom: -1 };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &m);
    }
    ok
}

/// Bentuk jendela = persegi membulat seukuran jendela.
unsafe fn apply_round(hwnd: HWND, w: i32, h: i32, radius: i32) {
    let r = radius.min(h / 2).min(w / 2).max(1);
    let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, r * 2, r * 2);
    SetWindowRgn(hwnd, rgn, TRUE); // region dimiliki sistem setelah ini
}

// ---------------------------------------------------------------------------
// Ikon: ambil besar, mipmap, resample bilinear
// ---------------------------------------------------------------------------

fn half_level(l: &Level) -> Level {
    let nw = l.w / 2;
    let nh = l.h / 2;
    let mut out = vec![0u32; (nw * nh) as usize];
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 4];
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let p = l.px[((y * 2 + dy) * l.w + x * 2 + dx) as usize];
                for c in 0..4 {
                    acc[c] += (p >> (c * 8)) & 0xFF;
                }
            }
            let mut v = 0u32;
            for c in 0..4 {
                v |= ((acc[c] + 2) / 4) << (c * 8);
            }
            out[(y * nw + x) as usize] = v;
        }
    }
    Level { w: nw, h: nh, px: out }
}

/// Konversi HBITMAP (32-bit) menjadi mipmap premultiplied.
unsafe fn bitmap_to_levels(bmp: HBITMAP) -> Option<Vec<Level>> {
    let mut bm = BITMAP::default();
    let got = GetObjectW(
        bmp,
        size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut BITMAP as *mut c_void),
    );
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight);
    let mut bi = BITMAPINFO::default();
    bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = w;
    bi.bmiHeader.biHeight = -h; // top-down
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = 0; // BI_RGB
    let mut data = vec![0u32; (w * h) as usize];
    let hdc = GetDC(HWND::default());
    let lines = GetDIBits(
        hdc,
        bmp,
        0,
        h as u32,
        Some(data.as_mut_ptr() as *mut c_void),
        &mut bi,
        DIB_RGB_COLORS,
    );
    ReleaseDC(HWND::default(), hdc);
    if lines == 0 {
        return None;
    }

    // Normalisasi alpha ke premultiplied.
    let mut any_alpha = false;
    let mut non_premul = false;
    for &p in &data {
        let a = p >> 24;
        if a != 0 {
            any_alpha = true;
        }
        let m = (p & 0xFF).max((p >> 8) & 0xFF).max((p >> 16) & 0xFF);
        if m > a {
            non_premul = true;
        }
    }
    if !any_alpha {
        for p in data.iter_mut() {
            *p |= 0xFF00_0000;
        }
    } else if non_premul {
        for p in data.iter_mut() {
            let a = *p >> 24;
            let b = ((*p & 0xFF) * a + 127) / 255;
            let g = (((*p >> 8) & 0xFF) * a + 127) / 255;
            let r = (((*p >> 16) & 0xFF) * a + 127) / 255;
            *p = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }

    let mut levels = vec![Level { w, h, px: data }];
    loop {
        let last = levels.last().unwrap();
        if last.w / 2 < 48 || last.h / 2 < 48 {
            break;
        }
        let next = half_level(last);
        levels.push(next);
    }
    Some(levels)
}

/// Resample bilinear level -> s x s ke buffer BGRA (stride dalam piksel).
unsafe fn blit_scaled(lv: &Level, s: i32, dst: *mut u32, stride: i32) {
    let sx = lv.w as f32 / s as f32;
    let sy = lv.h as f32 / s as f32;
    let mut cols: Vec<(usize, usize, u32)> = Vec::with_capacity(s as usize);
    for x in 0..s {
        let f = ((x as f32 + 0.5) * sx - 0.5).max(0.0);
        let x0 = f.floor() as i32;
        let wx = ((f - x0 as f32) * 256.0) as u32;
        let a = x0.min(lv.w - 1) as usize;
        let b = (x0 + 1).min(lv.w - 1) as usize;
        cols.push((a, b, wx.min(256)));
    }
    for y in 0..s {
        let f = ((y as f32 + 0.5) * sy - 0.5).max(0.0);
        let y0 = f.floor() as i32;
        let wy = (((f - y0 as f32) * 256.0) as u32).min(256);
        let ra = (y0.min(lv.h - 1) * lv.w) as usize;
        let rb = ((y0 + 1).min(lv.h - 1) * lv.w) as usize;
        let row = dst.add((y * stride) as usize);
        for (x, &(xa, xb, wx)) in cols.iter().enumerate() {
            let p00 = lv.px[ra + xa];
            let p01 = lv.px[ra + xb];
            let p10 = lv.px[rb + xa];
            let p11 = lv.px[rb + xb];
            let mut v = 0u32;
            for c in 0..4 {
                let sh = c * 8;
                let t = (((p00 >> sh) & 0xFF) * (256 - wx) + ((p01 >> sh) & 0xFF) * wx) * (256 - wy)
                    + (((p10 >> sh) & 0xFF) * (256 - wx) + ((p11 >> sh) & 0xFF) * wx) * wy;
                v |= ((t + 32768) >> 16).min(255) << sh;
            }
            *row.add(x) = v;
        }
    }
}

unsafe fn load_icon(path_w: &[u16], fetch: i32) -> Icon {
    let r: windows::core::Result<IShellItemImageFactory> =
        SHCreateItemFromParsingName(PCWSTR(path_w.as_ptr()), None::<&IBindCtx>);
    if let Ok(f) = r {
        let flags = SIIGBF(SIIGBF_ICONONLY.0 | SIIGBF_BIGGERSIZEOK.0);
        if let Ok(bmp) = f.GetImage(SIZE { cx: fetch, cy: fetch }, flags) {
            if !bmp.is_invalid() {
                let levels = bitmap_to_levels(bmp);
                let _ = DeleteObject(bmp);
                if let Some(l) = levels {
                    return Icon::Pix(l);
                }
            }
        }
    }
    let mut info = SHFILEINFOW::default();
    let r = SHGetFileInfoW(
        PCWSTR(path_w.as_ptr()),
        FILE_FLAGS_AND_ATTRIBUTES(0),
        Some(&mut info),
        size_of::<SHFILEINFOW>() as u32,
        SHGFI_ICON | SHGFI_LARGEICON,
    );
    if r != 0 && !info.hIcon.is_invalid() {
        Icon::Hicon(info.hIcon)
    } else {
        Icon::None
    }
}

// ---------------------------------------------------------------------------
// Shortcut & pinned
// ---------------------------------------------------------------------------

unsafe fn resolve_lnk(path_w: &[u16]) -> String {
    let r: windows::core::Result<IShellLinkW> =
        CoCreateInstance(&ShellLink, None::<&IUnknown>, CLSCTX_INPROC_SERVER);
    let Ok(link) = r else {
        return String::new();
    };
    let Ok(file) = link.cast::<IPersistFile>() else {
        return String::new();
    };
    if file.Load(PCWSTR(path_w.as_ptr()), STGM_READ).is_err() {
        return String::new();
    }
    let mut buf = [0u16; 520];
    let mut fd = WIN32_FIND_DATAW::default();
    if link.GetPath(&mut buf, &mut fd, 0).is_err() {
        return String::new();
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

unsafe fn read_favorites() -> Vec<u8> {
    let sub = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Taskband");
    let val = w!("Favorites");
    let mut size = 0u32;
    let r = RegGetValueW(
        HKEY_CURRENT_USER,
        sub,
        val,
        RRF_RT_REG_BINARY,
        None,
        None,
        Some(&mut size),
    );
    if r.0 != 0 || size == 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; size as usize];
    let r = RegGetValueW(
        HKEY_CURRENT_USER,
        sub,
        val,
        RRF_RT_REG_BINARY,
        None,
        Some(buf.as_mut_ptr() as *mut c_void),
        Some(&mut size),
    );
    if r.0 != 0 {
        return Vec::new();
    }
    buf.truncate(size as usize);
    buf
}

fn find_utf16(hay: &[u8], needle: &str) -> Option<usize> {
    let nb: Vec<u8> = needle.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    if nb.is_empty() || hay.len() < nb.len() {
        return None;
    }
    hay.windows(nb.len()).position(|w| w == nb.as_slice())
}

unsafe fn load_pinned(font: HFONT, fetch: i32) -> Vec<Item> {
    let mut found: Vec<(usize, Item)> = Vec::new();
    let Some(appdata) = std::env::var_os("APPDATA") else {
        return Vec::new();
    };
    let dir = PathBuf::from(appdata)
        .join("Microsoft\\Internet Explorer\\Quick Launch\\User Pinned\\TaskBar");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let fav = read_favorites();

    for e in rd.flatten() {
        let p = e.path();
        let is_lnk = p
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| x.eq_ignore_ascii_case("lnk"))
            .unwrap_or(false);
        if !is_lnk {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()) else {
            continue;
        };
        let fname = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let mut lnk: Vec<u16> = p.as_os_str().encode_wide().collect();
        lnk.push(0);

        let target = resolve_lnk(&lnk);
        let exe_l = target.to_lowercase();
        let exe_name_l = file_name_of(&exe_l).to_string();
        let order = find_utf16(&fav, &fname)
            .or_else(|| find_utf16(&fav, &stem))
            .unwrap_or(usize::MAX);

        let icon = load_icon(&lnk, fetch);
        let item = Item {
            name_w: measure(font, &stem),
            stem_l: stem.to_lowercase(),
            name: stem,
            lnk: Some(lnk),
            exe_l,
            exe_name_l,
            pinned: true,
            hwnd: None,
            icon,
            cur: 1.0,
        };
        found.push((order, item));
    }

    found.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.name.to_lowercase().cmp(&b.1.name.to_lowercase()))
    });
    found.into_iter().map(|(_, it)| it).collect()
}

unsafe fn make_running_item(r: &RunWin, font: HFONT, fetch: i32) -> Item {
    let name = capitalize(&stem_of(file_name_of(&r.exe)));
    let mut pw: Vec<u16> = r.exe.encode_utf16().collect();
    pw.push(0);
    let icon = load_icon(&pw, fetch);
    Item {
        name_w: measure(font, &name),
        name,
        lnk: None,
        exe_l: r.exe_l.clone(),
        exe_name_l: r.name_l.clone(),
        stem_l: stem_of(&r.name_l),
        pinned: false,
        hwnd: Some(r.hwnd),
        icon,
        cur: 1.0,
    }
}

// ---------------------------------------------------------------------------
// Enumerasi jendela running
// ---------------------------------------------------------------------------

unsafe fn is_app_window(h: HWND) -> bool {
    if !IsWindowVisible(h).as_bool() {
        return false;
    }
    if GetWindowLongPtrW(h, GWLP_HWNDPARENT) != 0 {
        return false;
    }
    let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TOOLWINDOW.0 != 0 {
        return false;
    }
    if ex & WS_EX_NOACTIVATE.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0 {
        return false;
    }
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(
        h,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut c_void,
        size_of::<u32>() as u32,
    )
    .is_ok()
        && cloaked != 0
    {
        return false;
    }
    if GetWindowTextLengthW(h) <= 0 {
        return false;
    }
    let mut cls = [0u16; 64];
    let n = GetClassNameW(h, &mut cls).max(0) as usize;
    let c = String::from_utf16_lossy(&cls[..n.min(64)]);
    if c == "Progman" || c == "WorkerW" {
        return false;
    }
    true
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let list = &mut *(lparam.0 as *mut Vec<HWND>);
    if is_app_window(hwnd) {
        list.push(hwnd);
    }
    TRUE
}

unsafe fn exe_path(pid: u32) -> Option<String> {
    let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
        .is_ok();
    let _ = CloseHandle(h);
    if ok {
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    } else {
        None
    }
}

/// (daftar satu-entri-per-exe, desktop_kosong). Desktop kosong = tidak ada jendela
/// aplikasi yang tidak minimized.
unsafe fn enum_running() -> (Vec<RunWin>, bool) {
    let mut wins: Vec<HWND> = Vec::new();
    let _ = EnumWindows(Some(enum_cb), LPARAM(&mut wins as *mut Vec<HWND> as isize));
    let me = GetCurrentProcessId();
    let mut out: Vec<RunWin> = Vec::new();
    let mut any_open = false;
    for h in wins {
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == 0 || pid == me {
            continue;
        }
        let Some(exe) = exe_path(pid) else {
            continue;
        };
        if !IsIconic(h).as_bool() {
            any_open = true;
        }
        let exe_l = exe.to_lowercase();
        if out.iter().any(|r| r.exe_l == exe_l) {
            continue;
        }
        out.push(RunWin {
            hwnd: h,
            name_l: file_name_of(&exe_l).to_string(),
            exe,
            exe_l,
        });
    }
    (out, !any_open)
}

fn pinned_matches(it: &Item, r: &RunWin) -> bool {
    if !it.exe_l.is_empty() {
        it.exe_l == r.exe_l || (!it.exe_name_l.is_empty() && it.exe_name_l == r.name_l)
    } else {
        !it.stem_l.is_empty() && stem_of(&r.name_l) == it.stem_l
    }
}

// ---------------------------------------------------------------------------
// Geometri & layout
// ---------------------------------------------------------------------------

/// Ukuran dock saat ISTIRAHAT (tanpa zoom) untuk jumlah item tertentu + posisinya.
unsafe fn geometry(sc: f32, n: usize, sep: bool) -> (i32, i32, i32, i32) {
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let b = cfg_icon() * sc;
    let gap = cfg_gap() * sc;
    let nn = n.max(1) as f32;
    let w = (2.0 * cfg_pad_x() * sc + nn * b + (nn - 1.0) * gap + if sep { (cfg_gap() + 1.0) * sc } else { 0.0 })
        .ceil() as i32;
    let w = w.min(sw - px(8.0, sc) * 2).max(1);
    let h = (cfg_pad_top() * sc + b + DOT_AREA * sc).ceil() as i32;
    let x = (sw - w) / 2;
    let y = sh - px(cfg_bottom(), sc) - h;
    (w, h, x, y)
}

/// Petakan koordinat "istirahat" -> koordinat "ter-zoom" (linear per segmen).
fn map_zoom(u: f32, pts: &[(f32, f32)]) -> f32 {
    if pts.is_empty() {
        return u;
    }
    let first = pts[0];
    let last = pts[pts.len() - 1];
    if u <= first.0 {
        return first.1 + (u - first.0);
    }
    if u >= last.0 {
        return last.1 + (u - last.0);
    }
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if u <= b.0 {
            let span = b.0 - a.0;
            let t = if span > 0.0 { (u - a.0) / span } else { 0.0 };
            return a.1 + t * (b.1 - a.1);
        }
    }
    last.1
}

impl Dock {
    /// Radius sudut dock (px) yang dipakai untuk menggambar garis tepi.
    fn corner_px(&self) -> i32 {
        if self.dwm_round {
            px(DWM_RADIUS, self.scale)
        } else {
            px(RADIUS, self.scale)
        }
    }

    /// Radius label (px) bila DWM yang membulatkan; None = pil penuh (fallback region).
    fn tip_corner(&self) -> Option<i32> {
        if self.dwm_round {
            Some(px(DWM_RADIUS, self.scale))
        } else {
            None
        }
    }

    fn sep_index(&self) -> Option<usize> {
        match self.items.iter().position(|i| !i.pinned) {
            Some(k) if k > 0 => Some(k),
            _ => None,
        }
    }

    /// Layout istirahat: (lebar total, kiri tiap ikon relatif ke kiri dock).
    fn rest_layout(&self) -> (f32, Vec<f32>) {
        let sc = self.scale;
        let n = self.items.len();
        let b = cfg_icon() * sc;
        let gap = cfg_gap() * sc;
        let sx = (cfg_gap() + 1.0) * sc;
        let fu = self.sep_index();
        let mut lefts = Vec::with_capacity(n);
        let mut x = cfg_pad_x() * sc;
        for i in 0..n {
            if i > 0 {
                x += gap;
            }
            if Some(i) == fu {
                x += sx;
            }
            lefts.push(x);
            x += b;
        }
        (x + cfg_pad_x() * sc, lefts)
    }

    unsafe fn targets(&self) -> Vec<f32> {
        let n = self.items.len();
        let mx = match self.mouse {
            Some((x, _)) => x as f32,
            None => return vec![1.0; n],
        };
        let sc = self.scale;
        let slot = (cfg_icon() + cfg_gap()) * sc;
        let b = cfg_icon() * sc;
        let sw = GetSystemMetrics(SM_CXSCREEN) as f32;
        let (rest_w, rl) = self.rest_layout();
        let rest_x = (sw - rest_w) / 2.0;
        rl.iter()
            .map(|l| {
                let c = rest_x + l + b / 2.0;
                let d = (mx - c).abs() / slot;
                if d >= ZOOM_RANGE {
                    1.0
                } else {
                    let f = 0.5 * (1.0 + (std::f32::consts::PI * d / ZOOM_RANGE).cos());
                    1.0 + (cfg_zoom() - 1.0) * f
                }
            })
            .collect()
    }

    /// Hit-test dalam koordinat KLIEN jendela dock.
    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        if y < 0 || y >= self.win_h {
            return None;
        }
        let half = (cfg_gap() * self.scale / 2.0) as i32;
        for i in 0..self.items.len().min(self.lefts.len()).min(self.sizes.len()) {
            if x >= self.lefts[i] - half && x < self.lefts[i] + self.sizes[i] + half {
                return Some(i);
            }
        }
        None
    }

    /// Posisi Y jendela dock sekarang (memperhitungkan slide autohide).
    fn y_now(&self) -> i32 {
        let travel = (self.win_h + px(cfg_bottom(), self.scale) + 2) as f32;
        self.shown_y + ((1.0 - self.slide) * travel).round() as i32
    }

    /// Hitung ukuran dock dari ukuran ikon saat ini (cur), lalu susun ikon. Dock = ikon + margin,
    /// jadi melebar/meninggi mengikuti zoom. Dock di-anchor ke kursor supaya ikon yang di-hover
    /// tidak "lari" dari kursor.
    unsafe fn relayout(&mut self) {
        let sc = self.scale;
        let n = self.items.len();
        let b = cfg_icon() * sc;
        let gap = cfg_gap() * sc;
        let sx = (cfg_gap() + 1.0) * sc;
        let fu = self.sep_index();
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);

        // Susunan ter-zoom, kiri = 0 (tepi kiri dock).
        let mut zl: Vec<f32> = Vec::with_capacity(n);
        let mut zs: Vec<f32> = Vec::with_capacity(n);
        let mut sep: Option<f32> = None;
        let mut x = cfg_pad_x() * sc;
        let mut max_s = b;
        for i in 0..n {
            let prev_right = x;
            if i > 0 {
                x += gap;
            }
            if Some(i) == fu {
                x += sx;
                sep = Some(prev_right + (gap + sx) / 2.0);
            }
            let s = b * self.items[i].cur;
            zl.push(x);
            zs.push(s);
            x += s;
            if s > max_s {
                max_s = s;
            }
        }
        let total = x + cfg_pad_x() * sc;

        let edge = px(8.0, sc);
        let (rest_w, rl) = self.rest_layout();
        let rest_h = cfg_pad_top() * sc + b + DOT_AREA * sc;
        // Ukuran penuh saat hover: TETAP (tidak bergantung posisi kursor), cukup untuk zoom maksimum.
        let full_w = rest_w + ((cfg_zoom() - 1.0) * b * ZOOM_RANGE).ceil() + 2.0;
        let full_h = cfg_pad_top() * sc + b * cfg_zoom() + DOT_AREA * sc;
        let g = self.grow.clamp(0.0, 1.0);
        let lw = rest_w + (full_w - rest_w) * g;
        let lh = rest_h + (full_h - rest_h) * g;
        let w = (lw.max(total).ceil() as i32).min(sw - edge * 2).max(1);
        let h = lh.max(cfg_pad_top() * sc + max_s + DOT_AREA * sc).ceil() as i32;

        // Jendela SELALU di tengah layar (tidak bergerak mengikuti kursor).
        let left = (sw - w) / 2;

        // Ikon digeser di DALAM dock supaya ikon di bawah kursor tidak lari.
        let rest_x = (sw as f32 - rest_w) / 2.0;
        let u = self.anchor_x as f32 - rest_x;
        let mut pts: Vec<(f32, f32)> = Vec::with_capacity(n * 2);
        for k in 0..n.min(rl.len()) {
            pts.push((rl[k], zl[k]));
            pts.push((rl[k] + b, zl[k] + zs[k]));
        }
        let z = map_zoom(u, &pts);
        let slack = (w as f32 - total).max(0.0);
        let shift = (self.anchor_x as f32 - z - left as f32).clamp(0.0, slack);

        self.win_w = w;
        self.win_h = h;
        self.win_x = left;
        self.shown_y = sh - px(cfg_bottom(), sc) - h;
        self.icon_bottom = h - px(DOT_AREA, sc);

        self.lefts.clear();
        self.sizes.clear();
        self.sep_x = sep.map(|v| (v + shift).round() as i32);
        for i in 0..n {
            let s = (zs[i].round() as i32).min(self.scratch_px);
            self.lefts.push((zl[i] + shift).round() as i32);
            self.sizes.push(s);
        }

        let y = self.y_now();
        self.hover = match self.mouse {
            Some((mx, my)) => self.hit(mx - self.win_x, my - y),
            None => None,
        };
    }

    /// Perintah untuk jendela label (hanya saat dock terbuka penuh dan ada hover).
    fn tip_update(&mut self) -> Option<TipCmd> {
        let sc = self.scale;
        let want = if self.slide >= 0.98 && self.visible {
            match self.hover {
                Some(h) if h < self.items.len() && !self.items[h].name.is_empty() => {
                    let lw = self.items[h].name_w + px(22.0, sc);
                    let lh = px(24.0, sc);
                    let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
                    let c = self.win_x + self.lefts[h] + self.sizes[h] / 2;
                    let x = (c - lw / 2).clamp(0, (sw - lw).max(0));
                    let y = self.shown_y - px(6.0, sc) - lh;
                    Some((h, x, y, lw, lh))
                }
                _ => None,
            }
        } else {
            None
        };
        match want {
            Some((h, x, y, w, hh)) => {
                let key = (h, x, y, w, hh);
                if self.tip_key == Some(key) {
                    None
                } else {
                    self.tip_key = Some(key);
                    self.tip_text = self.items[h].name.clone();
                    Some(TipCmd::Show { x, y, w, h: hh })
                }
            }
            None => {
                if self.tip_key.is_some() {
                    self.tip_key = None;
                    Some(TipCmd::Hide)
                } else {
                    None
                }
            }
        }
    }

    unsafe fn draw(&self, hdc: HDC) {
        let sc = self.scale;

        // Garis tepi tipis.
        let pen = CreatePen(PS_SOLID, 1, COLORREF(0x0055_5555));
        let op = SelectObject(hdc, pen);
        let ob = SelectObject(hdc, GetStockObject(NULL_BRUSH));
        let rr = self.corner_px().min(self.win_h / 2).min(self.win_w / 2).max(1) * 2;
        let _ = RoundRect(hdc, 0, 0, self.win_w, self.win_h, rr, rr);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        let _ = DeleteObject(pen);

        // Pemisah pinned | running.
        if let Some(x) = self.sep_x {
            let hh = (cfg_icon() * sc * 0.8) as i32;
            fill(
                hdc,
                RECT {
                    left: x,
                    top: self.icon_bottom - hh,
                    right: x + 1,
                    bottom: self.icon_bottom,
                },
                0x0066_6666,
            );
        }

        let src = CreateCompatibleDC(hdc);
        let old = SelectObject(src, self.scratch);
        let bf = BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1,
        };
        let dot_r = ((3.0 * sc).round() as i32).max(3);
        for (i, it) in self.items.iter().enumerate() {
            let (Some(&l), Some(&s)) = (self.lefts.get(i), self.sizes.get(i)) else {
                break;
            };
            if s <= 0 {
                continue;
            }
            let t = self.icon_bottom - s;
            match &it.icon {
                Icon::Pix(levels) => {
                    // Level terkecil yang masih >= ukuran target (kalau tidak ada: yang terbesar).
                    let lv = levels
                        .iter()
                        .rev()
                        .find(|lv| lv.w >= s && lv.h >= s)
                        .unwrap_or(&levels[0]);
                    blit_scaled(lv, s, self.scratch_bits, self.scratch_px);
                    let _ = AlphaBlend(hdc, l, t, s, s, src, 0, 0, s, s, bf);
                }
                Icon::Hicon(ic) => {
                    let _ = DrawIconEx(hdc, l, t, *ic, s, s, 0, HBRUSH::default(), DI_NORMAL);
                }
                Icon::None => {
                    rround(
                        hdc,
                        RECT {
                            left: l,
                            top: t,
                            right: l + s,
                            bottom: t + s,
                        },
                        px(8.0, sc),
                        0x0044_4444,
                    );
                }
            }
            if it.hwnd.is_some() {
                let cx = l + s / 2;
                let cy = self.icon_bottom + (self.win_h - self.icon_bottom) / 2;
                sky_dot(hdc, cx, cy, dot_r);
            }
        }
        SelectObject(src, old);
        let _ = DeleteDC(src);
    }
}

// ---------------------------------------------------------------------------
// API publik
// ---------------------------------------------------------------------------

pub unsafe fn create(scale: f32) -> bool {
    if DOCK.with(|c| c.borrow().is_some()) {
        return true;
    }
    let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
    let fail = |com: bool| {
        if com {
            CoUninitialize();
        }
        false
    };

    let Ok(hm) = GetModuleHandleW(PCWSTR::null()) else {
        return fail(com);
    };
    let hinstance: HINSTANCE = hm.into();
    let cursor = LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default();

    type WndProcFn = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

    for (name, proc) in [
        (w!("KachaDock"), wndproc as WndProcFn),
        (w!("KachaDockTip"), tip_proc as WndProcFn),
        (w!("KachaDockHot"), hot_proc as WndProcFn),
    ] {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: hinstance,
            lpszClassName: name,
            hCursor: cursor,
            ..Default::default()
        };
        RegisterClassW(&wc);
    }

    let font = make_font(crate::fonts::FACE, px(13.0, scale), 600);
    let fetch = if cfg_icon() * cfg_zoom() * scale <= 64.0 { 128 } else { 256 };
    let items = load_pinned(font, fetch);

    // Buffer kerja untuk resample ikon.
    let spx = (cfg_icon() * cfg_zoom() * scale).ceil() as i32 + 2;
    let mut bi = BITMAPINFO::default();
    bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = spx;
    bi.bmiHeader.biHeight = -spx;
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = 0;
    let mut bits: *mut c_void = std::ptr::null_mut();
    let scratch = match CreateDIBSection(HDC::default(), &bi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
        Ok(b) if !bits.is_null() => b,
        _ => {
            let _ = DeleteObject(font);
            return fail(com);
        }
    };

    let (w, h, x, y) = geometry(scale, items.len(), false);

    let ex = WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
    let Ok(hwnd) = CreateWindowExW(
        ex,
        w!("KachaDock"),
        w!("Kacha Dock"),
        WS_POPUP,
        x,
        y,
        w,
        h,
        HWND::default(),
        HMENU::default(),
        hinstance,
        None,
    ) else {
        let _ = DeleteObject(scratch);
        let _ = DeleteObject(font);
        return fail(com);
    };
    let tip = CreateWindowExW(
        ex,
        w!("KachaDockTip"),
        w!("Kacha Dock Tip"),
        WS_POPUP,
        0,
        0,
        10,
        10,
        HWND::default(),
        HMENU::default(),
        hinstance,
        None,
    )
    .unwrap_or_default();
    let dwm_round = round_corners(hwnd);
    let backdrop = dwm_round && enable_backdrop(hwnd);
    let mut tip_backdrop = false;
    if !tip.is_invalid() && round_corners(tip) {
        tip_backdrop = enable_backdrop(tip);
    }
    let hot = CreateWindowExW(
        ex | WS_EX_LAYERED,
        w!("KachaDockHot"),
        w!("Kacha Dock Hotspot"),
        WS_POPUP,
        0,
        0,
        10,
        2,
        HWND::default(),
        HMENU::default(),
        hinstance,
        None,
    )
    .unwrap_or_default();

    let mut dock = Dock {
        hwnd,
        tip,
        hot,
        scale: scale,
        com,
        font,
        fetch,
        scratch,
        scratch_bits: bits as *mut u32,
        scratch_px: spx,
        win_x: x,
        shown_y: y,
        cur_y: y,
        win_w: w,
        win_h: h,
        applied: (x, y, w, h),
        anchor_x: GetSystemMetrics(SM_CXSCREEN) / 2,
        grow: 0.0,
        dwm_round,
        glass: backdrop,
        tip_glass: tip_backdrop,
        items,
        mouse: None,
        tracking: false,
        animating: false,
        last_tick: 0,
        hover: None,
        desktop_empty: false,
        hot_on: false,
        slide: 0.0,
        visible: false,
        tip_text: String::new(),
        tip_key: None,
        lefts: Vec::new(),
        sizes: Vec::new(),
        sep_x: None,
        icon_bottom: 0,
    };
    dock.relayout();
    DOCK.with(|c| *c.borrow_mut() = Some(dock));

    if !hot.is_invalid() {
        // Alpha 1 (hampir transparan) tapi tetap bisa menerima mouse.
        let _ = SetLayeredWindowAttributes(hot, COLORREF(0), 3, LWA_ALPHA);
    }
    commit_geometry();
    if !hot.is_invalid() {
        let _ = ShowWindow(hot, SW_SHOWNOACTIVATE);
    }

    refresh();
    kick();
    true
}

pub unsafe fn destroy() {
    let hs = DOCK.with(|c| c.borrow().as_ref().map(|d| (d.hwnd, d.tip, d.hot)));
    if let Some((h, t, o)) = hs {
        if !t.is_invalid() {
            let _ = DestroyWindow(t);
        }
        if !o.is_invalid() {
            let _ = DestroyWindow(o);
        }
        let _ = DestroyWindow(h);
    }
}

/// Turun ke belakang saat ada aplikasi fullscreen.
pub unsafe fn set_fullscreen(on: bool) {
    FULLSCREEN.with(|f| f.set(on));
    let hs = DOCK.with(|c| c.borrow().as_ref().map(|d| (d.hwnd, d.tip, d.hot)));
    if let Some((h, t, o)) = hs {
        let after = if on { HWND_BOTTOM } else { HWND_TOPMOST };
        for win in [h, t, o] {
            if !win.is_invalid() {
                let _ = SetWindowPos(
                    win,
                    after,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }
}

/// True kalau jendela ini milik Poch (supaya Kacha tidak rebutan posisi paling atas dengannya).
unsafe fn is_poch(h: HWND) -> bool {
    let mut buf = [0u16; 32];
    let n = GetClassNameW(h, &mut buf);
    n > 0 && String::from_utf16_lossy(&buf[..n as usize]) == "PochWindow"
}

/// Kalau ada jendela topmost milik proses lain (overlay screen recorder, dll.) menutupi
/// menu bar / dock / hotspot, naikkan lagi. Dilewati saat ada aplikasi fullscreen.
pub unsafe fn keep_on_top(bar: HWND) {
    if FULLSCREEN.with(|f| f.get()) {
        return;
    }
    let hs = DOCK.with(|c| c.borrow().as_ref().map(|d| (d.hwnd, d.hot)));
    let Some((dock, hot)) = hs else {
        return;
    };
    let my_thread = GetCurrentThreadId();
    let covered = |win: HWND| -> bool {
        if win.is_invalid() {
            return false;
        }
        let mut p = GetWindow(win, GW_HWNDPREV).unwrap_or_default();
        let mut guard = 0;
        while !p.is_invalid() && guard < 128 {
            if IsWindowVisible(p).as_bool()
                && GetWindowThreadProcessId(p, None) != my_thread
                && !is_poch(p)
            {
                return true;
            }
            p = GetWindow(p, GW_HWNDPREV).unwrap_or_default();
            guard += 1;
        }
        false
    };
    for win in [bar, hot, dock] {
        if covered(win) {
            let _ = SetWindowPos(
                win,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

pub unsafe fn reposition() {
    commit_geometry();
}

pub unsafe fn refresh() {
    if BUSY.with(|b| b.replace(true)) {
        return;
    }
    refresh_inner();
    BUSY.with(|b| b.set(false));
}

// ---------------------------------------------------------------------------
// Refresh & geometri (tanpa borrow saat memanggil Win32)
// ---------------------------------------------------------------------------

unsafe fn kick() {
    let h = DOCK.with(|c| c.borrow().as_ref().map(|d| d.hwnd));
    if let Some(h) = h {
        ensure_timer(h);
    }
}

unsafe fn refresh_inner() {
    if !DOCK.with(|c| c.borrow().is_some()) {
        return;
    }
    let (running, empty) = enum_running();

    let a = DOCK.with(|c| {
        let mut b = c.borrow_mut();
        let d = b.as_mut()?;
        let mut matched = vec![false; running.len()];
        let mut changed = false;

        let empty_changed = d.desktop_empty != empty;
        d.desktop_empty = empty;

        for it in d.items.iter_mut() {
            if !it.pinned {
                continue;
            }
            let mut first: Option<usize> = None;
            for (k, r) in running.iter().enumerate() {
                if pinned_matches(it, r) {
                    matched[k] = true;
                    if first.is_none() {
                        first = Some(k);
                    }
                }
            }
            let new_h = first.map(|k| running[k].hwnd);
            if new_h.is_some() != it.hwnd.is_some() {
                changed = true;
            }
            it.hwnd = new_h;
        }

        let before = d.items.len();
        d.items.retain_mut(|it| {
            if it.pinned {
                return true;
            }
            match running.iter().position(|r| r.exe_l == it.exe_l) {
                Some(k) if !matched[k] => {
                    matched[k] = true;
                    it.hwnd = Some(running[k].hwnd);
                    true
                }
                _ => false,
            }
        });
        if d.items.len() != before {
            changed = true;
        }

        let sw = GetSystemMetrics(SM_CXSCREEN) as f32;
        let per = (cfg_icon() + cfg_gap()) * d.scale;
        let max_items = (((sw * 0.92 - 2.0 * cfg_pad_x() * d.scale) / per).floor() as usize).max(4);
        let mut add: Vec<usize> = (0..running.len()).filter(|&k| !matched[k]).collect();
        add.truncate(max_items.saturating_sub(d.items.len()));

        d.relayout();
        Some((add, changed, empty_changed, d.font, d.fetch))
    });
    let Some((add, mut changed, empty_changed, font, fetch)) = a else {
        return;
    };

    let mut fresh: Vec<Item> = Vec::new();
    for &k in &add {
        fresh.push(make_running_item(&running[k], font, fetch));
    }
    if !fresh.is_empty() {
        changed = true;
        DOCK.with(|c| {
            if let Some(d) = c.borrow_mut().as_mut() {
                for it in fresh {
                    d.items.push(it);
                }
            }
        });
    }

    if changed {
        commit_geometry();
    }
    if empty_changed {
        kick(); // mulai animasi buka/tutup autohide
    }
}

/// Hotspot selalu selebar layar di tepi bawah. Saat dock terbuka, tingginya sampai ke bawah dock
/// (menutup celah margin) supaya kursor bisa naik dari tepi layar ke dock tanpa dock menutup.
unsafe fn place_hot(hot: HWND, dock_visible: bool, sc: f32) {
    if hot.is_invalid() {
        return;
    }
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let base = px(HOTSPOT_H, sc).max(2);
    let hh = if dock_visible { px(cfg_bottom(), sc).max(base) } else { base };
    let _ = SetWindowPos(
        hot,
        HWND::default(),
        0,
        sh - hh,
        sw,
        hh,
        SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
}

/// Hitung ulang ukuran/posisi jendela (dock, hotspot) dan terapkan di luar borrow.
unsafe fn commit_geometry() {
    let r = DOCK.with(|c| {
        let mut b = c.borrow_mut();
        let d = b.as_mut()?;
        d.relayout();
        let y = d.y_now();
        d.cur_y = y;
        d.applied = (d.win_x, y, d.win_w, d.win_h);
        Some((d.hwnd, d.hot, d.win_x, y, d.win_w, d.win_h, d.visible, d.scale, d.dwm_round))
    });
    let Some((hwnd, hot, x, y, w, h, vis, sc, dwm)) = r else {
        return;
    };
    let _ = SetWindowPos(
        hwnd,
        HWND::default(),
        x,
        y,
        w,
        h,
        SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS,
    );
    if !dwm {
        apply_round(hwnd, w, h, px(RADIUS, sc));
    }
    place_hot(hot, vis, sc);
    invalidate(hwnd);
}

// ---------------------------------------------------------------------------
// Animasi (zoom + slide autohide)
// ---------------------------------------------------------------------------

unsafe fn ensure_timer(hwnd: HWND) {
    let start = DOCK.with(|c| match c.borrow_mut().as_mut() {
        Some(d) if !d.animating => {
            d.animating = true;
            d.last_tick = GetTickCount64();
            true
        }
        _ => false,
    });
    if start {
        SetTimer(hwnd, TIMER_ANIM, 16, None);
    }
}

struct TickOut {
    hwnd: HWND,
    tip: HWND,
    hot: HWND,
    /// (x, y, w, h, ukuran_berubah) bila jendela dock perlu dipindah/di-resize.
    place: Option<(i32, i32, i32, i32, bool)>,
    dwm: bool,
    vis: Option<bool>,
    moving: bool,
    tipcmd: Option<TipCmd>,
    sc: f32,
}

unsafe fn tick() {
    let r = DOCK.with(|c| {
        let mut b = c.borrow_mut();
        let d = b.as_mut()?;
        let now = GetTickCount64();
        let dt = now.saturating_sub(d.last_tick).clamp(1, 64) as f32;
        d.last_tick = now;

        // zoom
        let k = 1.0 - (-dt / ANIM_TAU_MS).exp();
        let targets = d.targets();
        let mut moving = false;
        for (it, t) in d.items.iter_mut().zip(targets.iter()) {
            let diff = *t - it.cur;
            if diff.abs() < 0.004 {
                it.cur = *t;
            } else {
                it.cur += diff * k;
                moving = true;
            }
        }

        // dock tumbuh ke ukuran penuh saat kursor di atasnya, kembali ke istirahat saat pergi
        let gt = if d.mouse.is_some() { 1.0 } else { 0.0 };
        let gd = gt - d.grow;
        if gd.abs() < 0.004 {
            d.grow = gt;
        } else {
            d.grow += gd * k;
            moving = true;
        }

        // autohide
        let want = !conf::flag(conf::DOCK_AUTOHIDE) || d.desktop_empty || d.mouse.is_some() || d.hot_on;
        let target = if want { 1.0 } else { 0.0 };
        let sd = target - d.slide;
        if sd.abs() < 0.004 {
            d.slide = target;
        } else {
            d.slide += sd * (1.0 - (-dt / SLIDE_TAU_MS).exp());
            moving = true;
        }

        d.relayout();
        if !moving {
            d.animating = false;
        }

        let y = d.y_now();
        d.cur_y = y;
        let now = (d.win_x, y, d.win_w, d.win_h);
        let place = if now != d.applied {
            let resized = (now.2, now.3) != (d.applied.2, d.applied.3);
            d.applied = now;
            Some((now.0, now.1, now.2, now.3, resized))
        } else {
            None
        };
        let want_vis = d.slide > 0.001;
        let vis = if want_vis != d.visible {
            d.visible = want_vis;
            Some(want_vis)
        } else {
            None
        };
        let tipcmd = d.tip_update();
        Some(TickOut {
            hwnd: d.hwnd,
            tip: d.tip,
            hot: d.hot,
            place,
            dwm: d.dwm_round,
            vis,
            moving,
            tipcmd,
            sc: d.scale,
        })
    });
    let Some(o) = r else {
        return;
    };

    if let Some(v) = o.vis {
        if v {
            let _ = ShowWindow(o.hwnd, SW_SHOWNOACTIVATE);
            place_hot(o.hot, true, o.sc);
        } else {
            let _ = ShowWindow(o.hwnd, SW_HIDE);
            if !o.tip.is_invalid() {
                let _ = ShowWindow(o.tip, SW_HIDE);
            }
            place_hot(o.hot, false, o.sc);
        }
    }
    if let Some((x, y, w, h, resized)) = o.place {
        let flags = if resized {
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOCOPYBITS
        } else {
            SWP_NOZORDER | SWP_NOACTIVATE
        };
        let _ = SetWindowPos(o.hwnd, HWND::default(), x, y, w, h, flags);
        if resized && !o.dwm {
            // Fallback (Windows 10): region membulat dipasang ulang setiap ukuran berubah.
            apply_round(o.hwnd, w, h, px(RADIUS, o.sc));
        }
    }
    if let Some(cmd) = o.tipcmd {
        if !o.tip.is_invalid() {
            match cmd {
                TipCmd::Show { x, y, w, h } => {
                    let _ = SetWindowPos(
                        o.tip,
                        HWND_TOPMOST,
                        x,
                        y,
                        w,
                        h,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                    if !o.dwm {
                        apply_round(o.tip, w, h, h);
                    }
                    invalidate(o.tip);
                }
                TipCmd::Hide => {
                    let _ = ShowWindow(o.tip, SW_HIDE);
                }
            }
        }
    }
    if o.vis != Some(false) {
        invalidate(o.hwnd);
    }
    if !o.moving {
        let _ = KillTimer(o.hwnd, TIMER_ANIM);
    }
}

// ---------------------------------------------------------------------------
// Gambar & aksi
// ---------------------------------------------------------------------------

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    DOCK.with(|c| {
        let b = c.borrow();
        let Some(d) = b.as_ref() else {
            return;
        };
        let (w, h) = (d.win_w, d.win_h);
        if w <= 0 || h <= 0 {
            return;
        }
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp);

        let full = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        if d.glass {
            // Hitam = "kaca" (backdrop terlihat).
            FillRect(mem, &full, HBRUSH(GetStockObject(BLACK_BRUSH).0));
        } else {
            fill(mem, full, SOLID_BG);
        }
        SetBkMode(mem, TRANSPARENT);

        d.draw(mem);

        let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);

        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
    });

    let _ = EndPaint(hwnd, &ps);
}

unsafe fn paint_tip(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let info = DOCK.with(|c| c.borrow().as_ref().map(|d| (d.font, d.tip_text.clone(), d.tip_corner(), d.tip_glass)));
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    if let (Some((font, text, corner, glass)), true) = (info, w > 0 && h > 0) {
        let mem = CreateCompatibleDC(hdc);
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp);
        if glass {
            FillRect(mem, &rc, HBRUSH(GetStockObject(BLACK_BRUSH).0));
        } else {
            fill(mem, rc, SOLID_BG);
        }
        SetBkMode(mem, TRANSPARENT);

        let pen = CreatePen(PS_SOLID, 1, COLORREF(0x0055_5555));
        let op = SelectObject(mem, pen);
        let ob = SelectObject(mem, GetStockObject(NULL_BRUSH));
        let rd = match corner {
            Some(c) => c.min(h / 2).max(1) * 2,
            None => h,
        };
        let _ = RoundRect(mem, 0, 0, w, h, rd, rd);
        SelectObject(mem, ob);
        SelectObject(mem, op);
        let _ = DeleteObject(pen);

        draw_label(mem, font, &text, rc);
        let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
    }
    let _ = EndPaint(hwnd, &ps);
}

unsafe fn activate(h: Option<HWND>, lnk: Option<Vec<u16>>) {
    if let Some(h) = h {
        if IsWindow(h).as_bool() {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
            return;
        }
    }
    if let Some(p) = lnk {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(p.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Kursor di tepi bawah layar -> buka dock dan tahan selama kursor masih di hotspot.
unsafe fn on_hot_move() {
    let h = DOCK.with(|c| {
        c.borrow_mut().as_mut().map(|d| {
            d.hot_on = true;
            d.hwnd
        })
    });
    if let Some(h) = h {
        let _ = KillTimer(h, TIMER_HIDE);
        ensure_timer(h);
    }
}

/// Kursor meninggalkan hotspot -> jadwalkan sembunyi (dibatalkan kalau kursor masuk ke dock).
unsafe fn on_hot_leave() {
    let h = DOCK.with(|c| c.borrow().as_ref().map(|d| d.hwnd));
    if let Some(h) = h {
        SetTimer(h, TIMER_HIDE, HIDE_DELAY_MS, None);
    }
}

unsafe extern "system" fn hot_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tme);
            on_hot_move();
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            on_hot_leave();
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe extern "system" fn tip_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint_tip(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
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
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER => {
            match wparam.0 {
                TIMER_ANIM => tick(),
                TIMER_HIDE => {
                    let _ = KillTimer(hwnd, TIMER_HIDE);
                    DOCK.with(|c| {
                        if let Some(d) = c.borrow_mut().as_mut() {
                            d.hot_on = false;
                        }
                    });
                    ensure_timer(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lparam);
            let (need_track, moved) = DOCK.with(|c| match c.borrow_mut().as_mut() {
                Some(d) => {
                    // Jendela berubah ukuran saat zoom, jadi simpan posisi kursor di koordinat layar.
                    let sp = (x + d.win_x, y + d.cur_y);
                    let moved = d.mouse != Some(sp);
                    d.mouse = Some(sp);
                    d.anchor_x = sp.0;
                    let nt = !d.tracking;
                    d.tracking = true;
                    (nt, moved)
                }
                None => (false, false),
            });
            let _ = KillTimer(hwnd, TIMER_HIDE); // kursor di dock: batalkan jadwal sembunyi
            if need_track {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
            }
            if moved {
                ensure_timer(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            DOCK.with(|c| {
                if let Some(d) = c.borrow_mut().as_mut() {
                    d.mouse = None;
                    d.tracking = false;
                }
            });
            SetTimer(hwnd, TIMER_HIDE, HIDE_DELAY_MS, None);
            ensure_timer(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy(lparam);
            let act = DOCK.with(|c| -> Option<(Option<HWND>, Option<Vec<u16>>)> {
                let b = c.borrow();
                let d = b.as_ref()?;
                let i = d.hit(x, y)?;
                let it = d.items.get(i)?;
                Some((it.hwnd, it.lnk.clone()))
            });
            if let Some((h, l)) = act {
                activate(h, l);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let d = DOCK.with(|c| c.borrow_mut().take());
            if let Some(d) = d {
                let _ = KillTimer(hwnd, TIMER_ANIM);
                let _ = KillTimer(hwnd, TIMER_HIDE);
                let _ = DeleteObject(d.font);
                let _ = DeleteObject(d.scratch);
                let com = d.com;
                drop(d);
                if com {
                    CoUninitialize();
                }
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}