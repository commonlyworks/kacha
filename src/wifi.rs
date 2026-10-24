//! Wrapper tipis untuk Native Wi-Fi API (wlanapi): daftar jaringan, scan, connect,
//! disconnect, dan forget. Handle hanya dibuka selama panel Wi-Fi terbuka,
//! jadi tidak ada biaya saat idle.

use std::ffi::c_void;
use std::ptr::null_mut;

use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::{BOOL, HANDLE};
use windows::Win32::NetworkManagement::WiFi::*;

const FLAG_CONNECTED: u32 = 0x1; // WLAN_AVAILABLE_NETWORK_CONNECTED
const FLAG_HAS_PROFILE: u32 = 0x2; // WLAN_AVAILABLE_NETWORK_HAS_PROFILE

/// Jenis keamanan jaringan, cukup untuk membuat profile baru dari panel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sec {
    Open,
    /// (authentication, encryption) untuk profile XML.
    Psk(&'static str, &'static str),
    /// Enterprise / WEP / lainnya: tidak bisa dibuat dari panel (yang sudah tersimpan tetap bisa connect).
    Other,
}

impl Sec {
    pub fn label(self) -> &'static str {
        match self {
            Sec::Open => "Open",
            Sec::Psk("WPA3SAE", _) => "WPA3",
            Sec::Psk("WPA2PSK", _) => "WPA2",
            Sec::Psk(..) => "WPA",
            Sec::Other => "Secured",
        }
    }
}

fn classify(auth: i32, cipher: i32, secure: bool) -> Sec {
    let enc = if cipher == 2 { "TKIP" } else { "AES" };
    match auth {
        1 if !secure => Sec::Open, // DOT11_AUTH_ALGO_80211_OPEN
        4 => Sec::Psk("WPAPSK", enc),
        7 => Sec::Psk("WPA2PSK", enc),
        9 => Sec::Psk("WPA3SAE", "AES"),
        _ => Sec::Other,
    }
}

#[derive(Clone)]
pub struct Net {
    pub ssid: String,
    /// 0..=100
    pub quality: u32,
    pub secure: bool,
    pub connected: bool,
    pub sec: Sec,
    /// Nama profile (UTF-16, diakhiri 0). Kosong = jaringan belum pernah disimpan.
    pub profile: Vec<u16>,
}

pub struct Wifi {
    handle: HANDLE,
    guid: GUID,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn profile_xml(ssid: &str, sec: Sec, key: &str) -> Option<String> {
    let hex: String = ssid.bytes().map(|b| format!("{:02X}", b)).collect();
    let name = esc(ssid);
    let (auth, enc, shared) = match sec {
        Sec::Open => ("open", "none", String::new()),
        Sec::Psk(a, e) => {
            let hexkey = key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit());
            let kind = if hexkey { "networkKey" } else { "passPhrase" };
            (
                a,
                e,
                format!(
                    "<sharedKey><keyType>{}</keyType><protected>false</protected><keyMaterial>{}</keyMaterial></sharedKey>",
                    kind,
                    esc(key)
                ),
            )
        }
        Sec::Other => return None,
    };
    Some(format!(
        "<?xml version=\"1.0\"?><WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\
<name>{name}</name><SSIDConfig><SSID><hex>{hex}</hex><name>{name}</name></SSID></SSIDConfig>\
<connectionType>ESS</connectionType><connectionMode>auto</connectionMode>\
<MSM><security><authEncryption><authentication>{auth}</authentication><encryption>{enc}</encryption><useOneX>false</useOneX></authEncryption>{shared}</security></MSM>\
</WLANProfile>"
    ))
}

impl Wifi {
    /// `None` kalau tidak ada adapter Wi-Fi / service WLAN mati.
    pub fn open() -> Option<Wifi> {
        unsafe {
            let mut ver = 0u32;
            let mut handle = HANDLE::default();
            if WlanOpenHandle(2, None, &mut ver, &mut handle) != 0 {
                return None;
            }
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = null_mut();
            if WlanEnumInterfaces(handle, None, &mut list) != 0 || list.is_null() {
                WlanCloseHandle(handle, None);
                return None;
            }
            let guid = if (*list).dwNumberOfItems > 0 {
                Some((*list).InterfaceInfo[0].InterfaceGuid)
            } else {
                None
            };
            WlanFreeMemory(list as *const c_void);
            match guid {
                Some(guid) => Some(Wifi { handle, guid }),
                None => {
                    WlanCloseHandle(handle, None);
                    None
                }
            }
        }
    }

    /// Minta driver scan ulang (asynchronous, hasilnya muncul beberapa detik kemudian).
    pub fn scan(&self) {
        unsafe {
            let _ = WlanScan(self.handle, &self.guid, None, None, None);
        }
    }

    /// Daftar jaringan: yang tersambung dulu, lalu urut sinyal terkuat.
    pub fn networks(&self) -> Vec<Net> {
        let mut out: Vec<Net> = Vec::new();
        unsafe {
            let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = null_mut();
            if WlanGetAvailableNetworkList(self.handle, &self.guid, 0, None, &mut list) != 0
                || list.is_null()
            {
                return out;
            }
            let n = (*list).dwNumberOfItems as usize;
            let base = (*list).Network.as_ptr();
            for i in 0..n {
                let nw = &*base.add(i);
                let len = (nw.dot11Ssid.uSSIDLength as usize).min(32);
                if len == 0 {
                    continue; // SSID tersembunyi
                }
                let ssid = String::from_utf8_lossy(&nw.dot11Ssid.ucSSID[..len]).to_string();

                let mut profile: Vec<u16> = Vec::new();
                if nw.dwFlags & FLAG_HAS_PROFILE != 0 {
                    let name = &nw.strProfileName;
                    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                    profile.extend_from_slice(&name[..end]);
                    profile.push(0);
                }
                let connected = nw.dwFlags & FLAG_CONNECTED != 0;
                let quality = nw.wlanSignalQuality;
                let secure = nw.bSecurityEnabled.as_bool();
                let sec = classify(
                    nw.dot11DefaultAuthAlgorithm.0,
                    nw.dot11DefaultCipherAlgorithm.0,
                    secure,
                );

                // Satu SSID bisa muncul beberapa kali (beberapa AP / band): gabungkan.
                if let Some(e) = out.iter_mut().find(|e| e.ssid == ssid) {
                    if e.profile.is_empty() {
                        e.profile = profile;
                    }
                    e.connected |= connected;
                    e.quality = e.quality.max(quality);
                } else {
                    out.push(Net {
                        ssid,
                        quality,
                        secure,
                        connected,
                        sec,
                        profile,
                    });
                }
            }
            WlanFreeMemory(list as *const c_void);
        }
        out.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then(b.quality.cmp(&a.quality))
        });
        out
    }

    /// Sambung ke jaringan yang sudah punya profile tersimpan.
    pub fn connect(&self, profile: &[u16]) -> bool {
        if profile.is_empty() {
            return false;
        }
        unsafe {
            let params = WLAN_CONNECTION_PARAMETERS {
                wlanConnectionMode: wlan_connection_mode_profile,
                strProfile: PCWSTR(profile.as_ptr()),
                pDot11Ssid: null_mut(),
                pDesiredBssidList: null_mut(),
                dot11BssType: dot11_BSS_type_any,
                dwFlags: 0,
            };
            WlanConnect(self.handle, &self.guid, &params, None) == 0
        }
    }

    /// Buat profile baru (open / WPA / WPA2 / WPA3 personal) lalu sambung.
    /// Mengembalikan nama profile yang dibuat supaya bisa dibatalkan kalau gagal.
    pub fn connect_new(&self, ssid: &str, sec: Sec, key: &str) -> Option<Vec<u16>> {
        let xml = wide(&profile_xml(ssid, sec, key)?);
        let name = wide(ssid);
        unsafe {
            let mut reason = 0u32;
            // 0 = profile semua user; kalau ditolak, coba profile per-user (WLAN_PROFILE_USER = 2).
            let mut r = WlanSetProfile(
                self.handle,
                &self.guid,
                0,
                PCWSTR(xml.as_ptr()),
                PCWSTR::null(),
                BOOL(1),
                None,
                &mut reason,
            );
            if r != 0 {
                r = WlanSetProfile(
                    self.handle,
                    &self.guid,
                    2,
                    PCWSTR(xml.as_ptr()),
                    PCWSTR::null(),
                    BOOL(1),
                    None,
                    &mut reason,
                );
            }
            if r != 0 {
                return None;
            }
        }
        if self.connect(&name) {
            Some(name)
        } else {
            self.forget(&name);
            None
        }
    }

    /// Putuskan koneksi Wi-Fi saat ini.
    pub fn disconnect(&self) -> bool {
        unsafe { WlanDisconnect(self.handle, &self.guid, None) == 0 }
    }

    /// Hapus profile tersimpan (lupakan jaringan).
    pub fn forget(&self, profile: &[u16]) -> bool {
        if profile.is_empty() {
            return false;
        }
        unsafe {
            WlanDeleteProfile(self.handle, &self.guid, PCWSTR(profile.as_ptr()), None) == 0
        }
    }
}

impl Drop for Wifi {
    fn drop(&mut self) {
        unsafe {
            WlanCloseHandle(self.handle, None);
        }
    }
}
