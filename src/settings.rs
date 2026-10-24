//! Pengaturan tampilan Kacha (menu bar + dock) yang bisa diubah user.
//!
//! - Semua nilai disimpan sebagai angka bulat di satu array (`Values`), diakses lewat indeks.
//! - Disimpan di `%AppData%\Kacha\settings.ini` (baris `key=value`).
//! - Jendela Settings (settings_ui.rs) mengubah nilai, menyimpan, lalu mengirim `WM_APPLY`
//!   ke jendela menu bar supaya perubahan langsung diterapkan.

use std::cell::Cell;
use std::path::PathBuf;

use windows::Win32::UI::WindowsAndMessaging::WM_USER;

/// Dikirim ke jendela menu bar setelah pengaturan berubah.
pub const WM_APPLY: u32 = WM_USER + 40;

pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub min: i32,
    pub max: i32,
    pub def: i32,
    pub unit: &'static str,
    /// true = saklar on/off (nilai 0 atau 1).
    pub toggle: bool,
}

const fn slider(key: &'static str, label: &'static str, min: i32, max: i32, def: i32, unit: &'static str) -> Field {
    Field { key, label, min, max, def, unit, toggle: false }
}

const fn switch(key: &'static str, label: &'static str, def: i32) -> Field {
    Field { key, label, min: 0, max: 1, def, unit: "", toggle: true }
}

pub const N: usize = 13;

// Indeks field (urutan = urutan di jendela Settings).
pub const BAR_HEIGHT: usize = 0;
pub const BAR_FONT: usize = 1;
pub const BAR_ICON: usize = 2;
pub const BAR_SPACING: usize = 3;
pub const BAR_PADDING: usize = 4;
pub const BAR_LOGO: usize = 5;
pub const BAR_OPACITY: usize = 6;
pub const DOCK_ICON: usize = 7;
pub const DOCK_ZOOM: usize = 8;
pub const DOCK_SPACING: usize = 9;
pub const DOCK_PAD: usize = 10;
pub const DOCK_BOTTOM: usize = 11;
pub const DOCK_AUTOHIDE: usize = 12;

/// Jumlah field menu bar (kolom kiri); sisanya milik dock.
pub const BAR_FIELDS: usize = 7;

pub const FIELDS: [Field; N] = [
    slider("bar_height", "Bar height", 24, 48, 32, " px"),
    slider("bar_font", "Text size", 11, 20, 15, " px"),
    slider("bar_icon", "Status icon size", 11, 22, 15, " px"),
    slider("bar_spacing", "Item spacing", 8, 36, 18, " px"),
    slider("bar_padding", "Edge padding", 6, 32, 14, " px"),
    slider("bar_logo", "Logo size", 14, 28, 18, " px"),
    slider("bar_opacity", "Background opacity", 10, 100, 60, "%"),
    slider("dock_icon", "Icon size", 24, 64, 30, " px"),
    slider("dock_zoom", "Hover zoom", 100, 200, 145, "%"),
    slider("dock_spacing", "Icon spacing", 0, 16, 5, " px"),
    slider("dock_pad", "Side padding", 4, 24, 8, " px"),
    slider("dock_bottom", "Bottom margin", 0, 32, 8, " px"),
    switch("dock_autohide", "Auto-hide dock", 1),
];

pub type Values = [i32; N];

thread_local! {
    static CFG: Cell<Values> = Cell::new(defaults());
}

pub fn defaults() -> Values {
    let mut v = [0; N];
    for i in 0..N {
        v[i] = FIELDS[i].def;
    }
    v
}

pub fn get() -> Values {
    CFG.with(|c| c.get())
}

pub fn val(i: usize) -> i32 {
    CFG.with(|c| c.get()[i])
}

pub fn flag(i: usize) -> bool {
    val(i) != 0
}

pub fn set(i: usize, v: i32) {
    let v = v.clamp(FIELDS[i].min, FIELDS[i].max);
    CFG.with(|c| {
        let mut a = c.get();
        a[i] = v;
        c.set(a);
    });
}

pub fn reset() {
    CFG.with(|c| c.set(defaults()));
}

/// Tint acrylic menu bar, format 0xAABBGGRR.
pub fn bar_tint() -> u32 {
    let a = (val(BAR_OPACITY) * 255 / 100) as u32;
    (a << 24) | 0x0018_1818
}

/// True kalau ada pengaturan dock yang berbeda (dock perlu dibuat ulang).
pub fn dock_changed(a: &Values, b: &Values) -> bool {
    (BAR_FIELDS..N).any(|i| a[i] != b[i])
}

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|b| PathBuf::from(b).join("Kacha").join("settings.ini"))
}

pub fn load() {
    let Some(p) = path() else {
        return;
    };
    let Ok(txt) = std::fs::read_to_string(&p) else {
        return;
    };
    let mut v = defaults();
    for line in txt.lines() {
        let Some((k, val)) = line.split_once('=') else {
            continue;
        };
        let (k, val) = (k.trim(), val.trim());
        if let (Some(i), Ok(n)) = (FIELDS.iter().position(|f| f.key == k), val.parse::<i32>()) {
            v[i] = n.clamp(FIELDS[i].min, FIELDS[i].max);
        }
    }
    CFG.with(|c| c.set(v));
}

pub fn save() {
    let Some(p) = path() else {
        return;
    };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let v = get();
    let mut s = String::new();
    for (i, f) in FIELDS.iter().enumerate() {
        s.push_str(&format!("{}={}\n", f.key, v[i]));
    }
    let _ = std::fs::write(p, s);
}
