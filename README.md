# Corddis

Arkadaş grubu için oyun dostu sesli/yazılı sohbet. Tasarım ve yol haritası: [MVP.md](MVP.md).

## Gereksinimler (Windows)

- Rust (stable, `rustup`)
- Visual Studio 2022 Build Tools, "C++ ile masaüstü geliştirme" iş yükü (libopus derlemesi için)
- CMake

## Derleme ve test

```bash
cargo test --workspace
cargo build --release
```

Çıktılar: `target/release/corddis.exe`, `target/release/corddis-server.exe`.

## Şu an denenebilenler (M1)

```bash
target/release/corddis.exe devices
target/release/corddis.exe loopback
target/release/corddis.exe loopback --loss 10 --jitter 40
```

`loopback` kendi sesini tüm ses hattından geçirip kulaklığa verir: gürültü engelleme →
ses algılama → Opus → sahte ağ → jitter buffer → Opus → kulaklık. Her 2 saniyede bir
VAD oranı, upload, kayıp/FEC/PLC, işlem süreleri, CPU ve RAM yazdırır.
Tüm seçenekler için: `corddis.exe` (argümansız).

## Sunucu + sohbet + ses (M2, M3)

Yerelde denemek için:

```bash
target/release/corddis-server user add Ali --host 127.0.0.1 --port 8443
target/release/corddis-server run --bind 127.0.0.1:8443 --udp-port 7777
target/release/corddis.exe chat "<yukarıda çıkan corddis:// bağlantısı>"
```

`chat` içinde `/katil Oyun` ile sesli kanala girilir; ses otomatik başlar. Diğer komutlar:
`/mute`, `/unmute`, `/sagir`, `/seviye <ad> <0-200>`, `/ses` (ping, paketler, CPU/RAM), `/kim`, `/yardim`.

Gerçek kurulum: [deploy/setup-oracle.md](deploy/setup-oracle.md).

## Depo yapısı

| Klasör | İçerik |
|---|---|
| `crates/proto` | Ortak tanımlar: ses sabitleri, UDP paket formatı + şifreleme, WebSocket mesajları |
| `crates/client` | Windows istemcisi (şimdilik komut satırı) |
| `crates/server` | Sunucu: TLS + WebSocket, SQLite, yönetim komutları. Uçtan uca testler `crates/server/tests` |
| `deploy` | systemd servisi, yönetim betiği, Oracle kurulum rehberi |
