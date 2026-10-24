//! Daftar aplikasi untuk Start Menu buatan kita (tahap 1: aplikasi desktop).
//!
//! - Sumber: file .lnk di folder Start Menu (%ProgramData% dan %AppData%), rekursif.
//! - Ikon diambil dari Windows lewat SHGetFileInfoW, hanya untuk baris yang tampil.
//! - Semua dipindai saat panel dibuka dan dibebaskan saat panel ditutup.

use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, ShellExecuteW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON, SW_SHOWNORMAL};

/// Kata kunci shortcut yang bukan aplikasi (dicocokkan dengan nama huruf kecil).
const JUNK: [&str; 12] = [
    "uninstall",
    "readme",
    "read me",
    "release notes",
    "website",
    "web site",
    "documentation",
    "license",
    "manual",
    "changelog",
    "what's new",
    "visit ",
];

pub struct App {
    pub name: String,
    /// Nama huruf kecil, dipakai untuk urutan dan pencarian.
    pub key: String,
    /// Path file .lnk (UTF-16, diakhiri 0).
    pub path: Vec<u16>,
    pub icon: Option<HICON>,
    /// Sudah dicoba dimuat (supaya yang gagal tidak dicoba terus).
    pub tried: bool,
}

impl App {
    /// Huruf header: huruf pertama (kapital) atau '#' untuk angka/simbol.
    pub fn initial(&self) -> char {
        match self.name.chars().next() {
            Some(c) if c.is_alphabetic() => c.to_uppercase().next().unwrap_or(c),
            _ => '#',
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(i) = self.icon.take() {
            unsafe {
                let _ = DestroyIcon(i);
            }
        }
    }
}

/// Pindai Start Menu, urut alfabet, tanpa duplikat.
pub fn scan() -> Vec<App> {
    let mut out: Vec<App> = Vec::new();
    for var in ["ProgramData", "APPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            let root = PathBuf::from(base).join(r"Microsoft\Windows\Start Menu\Programs");
            walk(&root, 0, &mut out);
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out.dedup_by(|a, b| a.key == b.key);
    out
}

fn walk(dir: &Path, depth: u32, out: &mut Vec<App>) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            let is_startup = p
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.eq_ignore_ascii_case("startup"))
                .unwrap_or(false);
            if !is_startup {
                walk(&p, depth + 1, out);
            }
            continue;
        }
        let is_lnk = p
            .extension()
            .and_then(|x| x.to_str())
            .map(|x| x.eq_ignore_ascii_case("lnk"))
            .unwrap_or(false);
        if !is_lnk {
            continue;
        }
        let Some(name) = p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()) else {
            continue;
        };
        let key = name.to_lowercase();
        if JUNK.iter().any(|j| key.contains(j)) {
            continue;
        }
        let mut path: Vec<u16> = p.as_os_str().encode_wide().collect();
        path.push(0);
        out.push(App {
            name,
            key,
            path,
            icon: None,
            tried: false,
        });
    }
}

/// Ambil ikon kecil (16px) dari Windows untuk file .lnk. Pemanggil bertanggung jawab
/// memanggil DestroyIcon (App melakukannya lewat Drop).
pub fn fetch_icon(path: &[u16]) -> Option<HICON> {
    unsafe {
        let mut info = SHFILEINFOW::default();
        let r = SHGetFileInfoW(
            PCWSTR(path.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON,
        );
        if r != 0 && !info.hIcon.is_invalid() {
            Some(info.hIcon)
        } else {
            None
        }
    }
}

/// Jalankan shortcut (.lnk).
pub fn launch(path: &[u16]) {
    unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}
