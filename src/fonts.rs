//! Memuat font Instrument Sans dari dalam .exe (private untuk proses ini,
//! tidak perlu diinstal di Windows). Lisensi font: SIL OFL, boleh di-embed.

use std::ffi::c_void;

use windows::Win32::Graphics::Gdi::AddFontMemResourceEx;

/// Nama family untuk CreateFontIndirectW.
pub const FACE: &str = "Instrument Sans";

static REGULAR: &[u8] = include_bytes!("../assets/InstrumentSans-Regular.ttf");
static BOLD: &[u8] = include_bytes!("../assets/InstrumentSans-Bold.ttf");

/// Panggil sekali di awal program, sebelum membuat font apa pun.
pub fn load() {
    unsafe {
        for data in [REGULAR, BOLD] {
            let mut n = 0u32;
            let _ = AddFontMemResourceEx(data.as_ptr() as *const c_void, data.len() as u32, None, &mut n);
        }
    }
}