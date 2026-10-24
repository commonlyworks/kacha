//! Data status sistem untuk menu bar: baterai, jam/tanggal, nama aplikasi aktif.

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, FALSE, HWND};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Battery {
    pub present: bool,
    pub percent: u8,
    pub charging: bool,
}

pub fn battery() -> Battery {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe {
        if GetSystemPowerStatus(&mut s).is_err() {
            return Battery::default();
        }
    }
    // BatteryFlag 128 = tidak ada baterai, BatteryLifePercent 255 = tidak diketahui.
    let present = s.BatteryFlag != 128 && s.BatteryLifePercent <= 100;
    Battery {
        present,
        percent: s.BatteryLifePercent.min(100),
        charging: s.ACLineStatus == 1,
    }
}

/// Tahun sekarang (untuk teks copyright).
pub fn current_year() -> u16 {
    unsafe { GetLocalTime() }.wYear
}

pub fn clock_text() -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let t = unsafe { GetLocalTime() };
    format!(
        "{} {} {}  {:02}:{:02}",
        DAYS[t.wDayOfWeek as usize % 7],
        t.wDay,
        MONTHS[(t.wMonth as usize + 11) % 12],
        t.wHour,
        t.wMinute
    )
}

/// Nama aplikasi yang sedang aktif (seperti nama app tebal di menu bar macOS).
/// `None` = jangan ubah (misalnya fokus sedang di menu bar kita sendiri).
pub fn foreground_app(own: HWND) -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return Some("Desktop".to_string());
        }
        if hwnd == own {
            return None;
        }

        let mut class_buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut class_buf).max(0) as usize;
        let class = String::from_utf16_lossy(&class_buf[..n]);
        match class.as_str() {
            "#32768" | "KachaPanel" => return None, // popup menu / panel kita
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" => {
                return Some("Desktop".to_string())
            }
            _ => {}
        }

        let mut title_buf = [0u16; 128];
        let tn = GetWindowTextW(hwnd, &mut title_buf).max(0) as usize;
        let title = String::from_utf16_lossy(&title_buf[..tn]);

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let stem = process_stem(pid).unwrap_or_default();

        Some(friendly_name(&stem, &title))
    }
}

fn process_stem(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid).ok()?;
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let res = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);
        res.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let file = path.rsplit('\\').next()?;
        let stem = match file.rsplit_once('.') {
            Some((a, _)) => a,
            None => file,
        };
        Some(stem.to_string())
    }
}

fn friendly_name(stem: &str, title: &str) -> String {
    let known = match stem.to_ascii_lowercase().as_str() {
        "chrome" => "Google Chrome",
        "msedge" => "Microsoft Edge",
        "firefox" => "Firefox",
        "code" => "Visual Studio Code",
        "devenv" => "Visual Studio",
        "explorer" => "File Explorer",
        "windowsterminal" => "Terminal",
        "cmd" => "Command Prompt",
        "powershell" | "pwsh" => "PowerShell",
        "notepad" => "Notepad",
        "taskmgr" => "Task Manager",
        "discord" => "Discord",
        "spotify" => "Spotify",
        "startmenuexperiencehost" | "searchhost" | "searchapp" => "Start",
        // Aplikasi UWP dibungkus ApplicationFrameHost: pakai judul jendela.
        "applicationframehost" => "",
        "" => "",
        _ => "?",
    };
    if known == "?" {
        let mut chars = stem.chars();
        return match chars.next() {
            Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            None => title.to_string(),
        };
    }
    if known.is_empty() {
        return shorten(title, 28);
    }
    known.to_string()
}

fn shorten(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}