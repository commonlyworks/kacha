//! Mematikan (menyembunyikan) taskbar bawaan Windows dan mengembalikannya.
//!
//! Caranya:
//!  1. Simpan state taskbar yang asli (ABM_GETSTATE).
//!  2. Set taskbar ke auto-hide (ABM_SETSTATE) supaya work area kembali penuh.
//!  3. Sembunyikan jendela Shell_TrayWnd (monitor utama) dan
//!     Shell_SecondaryTrayWnd (monitor lain) dengan SW_HIDE.
//!  4. Saat keluar, kembalikan state dan tampilkan lagi.

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Shell::{SHAppBarMessage, APPBARDATA};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetSystemMetrics, GetWindowRect, IsWindowVisible, PostMessageW,
    SetWindowPos, ShowWindow, HWND_TOPMOST, SM_CXSCREEN, SWP_NOACTIVATE, SWP_NOSIZE, SW_HIDE,
    SW_SHOWNA,
};

const ABM_GETSTATE: u32 = 4;
const ABM_SETSTATE: u32 = 10;
const ABS_AUTOHIDE: isize = 1;
const ABS_ALWAYSONTOP: isize = 2;
const BM_CLICK: u32 = 0x00F5;

static ORIGINAL_STATE: AtomicU32 = AtomicU32::new(ABS_ALWAYSONTOP as u32);
static STATE_SAVED: AtomicBool = AtomicBool::new(false);

/// Semua jendela taskbar (utama + monitor tambahan).
fn taskbars() -> Vec<HWND> {
    let mut list = Vec::new();
    unsafe {
        if let Ok(h) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) {
            list.push(h);
        }
        let mut prev = HWND::default();
        while let Ok(h) = FindWindowExW(
            HWND::default(),
            prev,
            w!("Shell_SecondaryTrayWnd"),
            PCWSTR::null(),
        ) {
            list.push(h);
            prev = h;
        }
    }
    list
}

unsafe fn appbar_message(msg: u32, hwnd: HWND, lparam: isize) -> usize {
    let mut abd = APPBARDATA {
        cbSize: size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        lParam: LPARAM(lparam),
        ..Default::default()
    };
    SHAppBarMessage(msg, &mut abd)
}

/// Matikan taskbar bawaan.
pub fn hide() {
    let bars = taskbars();
    let Some(&main_bar) = bars.first() else {
        return;
    };
    unsafe {
        // Simpan state asli hanya sekali.
        if !STATE_SAVED.swap(true, Ordering::SeqCst) {
            let state = appbar_message(ABM_GETSTATE, main_bar, 0) as u32;
            ORIGINAL_STATE.store(state, Ordering::SeqCst);
        }
        // Auto-hide supaya area kerja jendela lain jadi layar penuh.
        appbar_message(ABM_SETSTATE, main_bar, ABS_AUTOHIDE);
        for h in bars {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
}

/// Dipanggil berkala: kalau Windows memunculkan taskbar lagi, sembunyikan lagi.
pub fn enforce_hidden() {
    unsafe {
        for h in taskbars() {
            if IsWindowVisible(h).as_bool() {
                let _ = ShowWindow(h, SW_HIDE);
            }
        }
    }
}

/// Kembalikan taskbar seperti semula.
pub fn restore() {
    let bars = taskbars();
    let Some(&main_bar) = bars.first() else {
        return;
    };
    let original = if STATE_SAVED.load(Ordering::SeqCst) {
        ORIGINAL_STATE.load(Ordering::SeqCst) as isize
    } else {
        ABS_ALWAYSONTOP
    };
    unsafe {
        appbar_message(ABM_SETSTATE, main_bar, original);
        for h in bars {
            let _ = ShowWindow(h, SW_SHOWNA);
        }
    }
}

/// Untuk `kacha.exe --restore` (misalnya setelah app crash / di-kill).
pub fn restore_force() {
    STATE_SAVED.store(false, Ordering::SeqCst);
    restore();
}

/// Buka/tutup popup ikon tray tersembunyi (tombol panah ^) milik Windows.
/// Dilakukan dengan meng-klik tombol chevron di taskbar asli (yang disembunyikan).
pub fn toggle_overflow() {
    unsafe {
        let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()) else {
            return;
        };
        let Ok(notify) = FindWindowExW(tray, HWND::default(), w!("TrayNotifyWnd"), PCWSTR::null())
        else {
            return;
        };
        let Ok(button) = FindWindowExW(notify, HWND::default(), w!("Button"), PCWSTR::null())
        else {
            return;
        };
        let _ = PostMessageW(button, BM_CLICK, WPARAM(0), LPARAM(0));
    }
}

/// Geser popup tray ke bawah tombol panah di menu bar (rata kanan ke `anchor_right`).
/// Return true kalau popup sedang terlihat.
pub fn place_overflow(anchor_right: i32, top: i32) -> bool {
    unsafe {
        let Ok(win) = FindWindowW(w!("NotifyIconOverflowWindow"), PCWSTR::null()) else {
            return false;
        };
        if !IsWindowVisible(win).as_bool() {
            return false;
        }
        let mut rc = RECT::default();
        let _ = GetWindowRect(win, &mut rc);
        let width = rc.right - rc.left;
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let x = (anchor_right - width).clamp(0, (screen_w - width).max(0));
        let _ = SetWindowPos(win, HWND_TOPMOST, x, top, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE);
        true
    }
}