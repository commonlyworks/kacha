# kaca-shell

Menu bar gaya macOS untuk Windows 10, dengan **taskbar bawaan dimatikan**. Pure Rust (crate `windows`, Win32 langsung, tanpa framework UI).

## Fitur (v0.2)

- Taskbar bawaan Windows disembunyikan saat app start, dikembalikan saat app keluar / panic.
- Menu bar acrylic di atas layar, didaftarkan sebagai AppBar: jendela lain tidak menutupinya dan area kerja otomatis mulai di bawahnya. Turun otomatis saat ada aplikasi fullscreen.
- **Kiri:** logo (menu "Apple" versi Windows) + nama aplikasi yang sedang aktif (tebal).
- **Kanan:** volume, Wi-Fi, baterai (ikon + persen, disembunyikan jika PC tanpa baterai), jam dan tanggal.
- Hover highlight pada tiap item.

### Interaksi

| Item | Klik kiri | Lainnya |
|---|---|---|
| Logo | Menu: About This PC, System Settings, Start Menu, Task Manager, Sleep, Restart, Shut Down, Lock Screen, Quit | |
| Volume | Mute / unmute | Scroll mouse di atas ikon = volume naik/turun. Klik kanan = pengaturan suara |
| Wi-Fi | Buka daftar jaringan Windows | Klik kanan = pengaturan Wi-Fi |
| Baterai | Pengaturan battery saver | |
| Jam | Pengaturan tanggal & waktu | |

## Cara menjalankan

1. Install Rust: https://rustup.rs (toolchain `stable-x86_64-pc-windows-msvc`).
2. Install **Visual Studio Build Tools** dengan workload "Desktop development with C++".
3. Ekstrak zip, buka terminal di folder yang berisi `Cargo.toml`, lalu:

   ```
   cargo build --release
   target\release\kaca-shell.exe
   ```

   Atau cukup `cargo run --release`. Build pertama 2-5 menit.

## Keluar / mengembalikan taskbar

- **Ctrl + Alt + Q**, atau logo -> *Quit Kaca Shell*. Taskbar kembali.
- Kalau app crash / di-kill dan taskbar masih hilang: jalankan `kaca-shell.exe --restore`, atau Task Manager -> Run new task -> `explorer.exe`.
- Buka Task Manager dulu sebelum tes pertama sebagai cadangan.

## Pengaturan tampilan

Di bagian atas `src/bar.rs`:

- `BAR_HEIGHT` : tinggi bar (px pada 96 DPI).
- `TINT` : warna & kepekatan kaca, format `0xAABBGGRR`.
- `ACCENT_STATE` : `4` = acrylic, `3` = blur biasa (coba kalau acrylic bermasalah).

Acrylic butuh Windows 10 1803+ dan *Settings -> Personalization -> Colors -> Transparency effects* aktif. Jika tidak aktif, bar jadi warna solid.

## Yang belum ada (jujur)

- **Ikon system tray aplikasi lain** (Discord, Steam, dll) tidak tampil: Windows tidak punya API publik untuk membacanya, dan tray ikut hilang bersama taskbar. Ini perlu pendekatan khusus (milestone tersendiri).
- **Daftar aplikasi berjalan / pindah jendela**: tidak ada sampai dock dibuat. Sementara pakai Alt+Tab / Win+Tab.
- Level volume dan kekuatan sinyal Wi-Fi belum ditampilkan, baru ikon. Control Center belum ada.
- Menu logo memakai menu Windows biasa (tema terang), belum bergaya macOS.
- Hanya monitor utama.
- Start menu yang dipakai tombol "Start Menu" adalah Start menu asli Windows (posisinya bisa agak aneh karena taskbar tersembunyi).

## Struktur

```
src/main.rs      entry point, single instance, panic hook
src/taskbar.rs   sembunyikan / kembalikan / jaga taskbar bawaan
src/bar.rs       menu bar: window, acrylic, AppBar, gambar GDI, klik & menu
src/status.rs    baterai, jam, nama aplikasi aktif
```

Versi `windows` dikunci `=0.58.0` karena API crate ini sering berubah antar versi.

## Rencana berikutnya

1. Dock bawah (ikon pinned apps, aplikasi berjalan, zoom).
2. Control Center (slider volume/brightness, Wi-Fi, Bluetooth).
3. Widget cuaca.
4. Tray icon aplikasi lain.
