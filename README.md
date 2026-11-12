# Terra Studio

Aplikasi desktop Windows (Rust) untuk kreator YouTube: ide video dan kalender, kerangka naskah, paket SEO, konsep thumbnail, ide Shorts, serta analisis data ekspor YouTube Studio. Fitur AI memakai Claude lewat API key Anthropic milik pengguna sendiri.

## Cara kerja

- `TerraStudio.exe` menjalankan server lokal di `127.0.0.1` (port 47815 sampai 47818) dan membuka tampilan di jendela Edge/Chrome mode aplikasi.
- Tampilan HTML ada di `assets/` dan ditanam ke dalam exe saat build.
- Pengaturan, data Studio, dan hasil AI terakhir disimpan di `%APPDATA%\TerraStudio`.
- Panggilan ke Claude memakai Messages API secara langsung (`ureq`).

## Build

```sh
# di Windows
cargo build --release

# cross-compile dari Linux (butuh mingw-w64)
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

Hasilnya `target/.../release/TerraStudio.exe`.

## Panduan pengguna

Lihat [READ-ME-FIRST.txt](READ-ME-FIRST.txt).
