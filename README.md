# Corddis

Arkadaş grubu için oyun dostu sesli/yazılı sohbet. Tasarım ve yol haritası: [MVP.md](MVP.md).

## Kullanım (arkadaşlar için)

1. [Releases](https://github.com/mehmetcanozcelik/corddis/releases/latest) sayfasından **`corddis.exe`**'yi indirin. Kurulum yok; istediğiniz klasöre koyup çalıştırın.
2. Windows ilk açılışta *"Windows bilgisayarınızı korudu"* diyebilir: **Ek bilgi → Yine de çalıştır**. (Exe imzasız olduğu için; imza sertifikası ücretli.)
3. Size birebir mesajla gönderilen `corddis://...` davet bağlantısını yapıştırıp **Bağlan**'a basın. Bağlantı bu bilgisayarda şifreli saklanır, bir daha sorulmaz. **Davet bağlantınızı kimseyle paylaşmayın**: şifre gibidir.
4. Sol taraftan bir **ses kanalına** tıklayın. Mikrofon açık kalır, yalnızca konuşurken ses gider.

| | |
|---|---|
| 🎤 | Mikrofonu aç/kapat. Oyun içindeyken: **Ctrl+Shift+M** (kapatınca/açınca kısa bir ses çıkar) |
| 🎧 | Sağır modu (kimseyi duymazsınız, konuşamazsınız) |
| ⚙ | Mikrofon/kulaklık seçimi, ses algılama eşiği, gürültü engelleme, otomatik ses seviyesi, bildirim sesleri |
| Sağ tık (kişi) | O kişinin ses düzeyi (%0–300) |
| Ayrıl | Sesli kanaldan çık |
| Pencereyi kapat (✕) | Uygulama **tepside** (saatin yanı) çalışmaya devam eder, sesli kanaldan düşmezsiniz. Tepsi ikonuna sol tık: pencereyi aç; sağ tık: mikrofon / çıkış |

**Sesim gitmiyor / boşuna açılıyor:** ⚙ → Ses algılama. Konuşurken çubuk sarı çizgiyi geçmeli, sessizken geçmemeli; eşiği buna göre kaydırın.

**Arkadaşım kısık geliyor:** Onun ⚙ ayarlarında *Otomatik ses seviyesi* açık olmalı (varsayılan açık; kısık mikrofonu +24 dB'e kadar yükseltir). Ayrıca o kişiye sağ tıklayıp %300'e kadar açabilirsiniz.

Ayarlar `%APPDATA%\Corddis\config.json` dosyasında durur.

## Geliştirme

### Gereksinimler (Windows)

- Rust (stable, `rustup`)
- Visual Studio 2022 Build Tools, "C++ ile masaüstü geliştirme" iş yükü (libopus derlemesi için)
- CMake

### Derleme ve test

```bash
cargo test --workspace
cargo build --release
```

Çıktılar (`target/release/`):

| Dosya | Ne |
|---|---|
| `corddis.exe` | Arayüz. C çalışma zamanı içine gömülü; tek dosya olarak dağıtılabilir. |
| `corddis-cli.exe` | Komut satırı araçları: `devices`, `loopback` (ses hattı testi), `chat` (arayüzsüz istemci), `set-invite` |
| `corddis-server.exe` | Sunucu (gerçek kurulum Linux'ta: [deploy/setup-oracle.md](deploy/setup-oracle.md)) |

### Yerelde uçtan uca deneme

```bash
target/release/corddis-server user add Ali --host 127.0.0.1 --port 8443
target/release/corddis-server run --bind 127.0.0.1:8443 --udp-port 7777
target/release/corddis.exe
```

Aynı bilgisayarda ikinci bir hesapla denemek için `CORDDIS_CONFIG_DIR` ortam değişkeniyle ayrı bir ayar klasörü verin.

### Depo yapısı

| Klasör | İçerik |
|---|---|
| `crates/proto` | Ortak tanımlar: ses sabitleri, UDP paket formatı + şifreleme, WebSocket mesajları, davet bağlantısı |
| `crates/client` | İstemci kütüphanesi (ses hattı, ağ, model, arayüz) + `corddis` ve `corddis-cli` |
| `crates/server` | Sunucu: TLS + WebSocket, SQLite, UDP ses relay'i, yönetim komutları. Uçtan uca testler `crates/server/tests` |
| `deploy` | systemd servisi, yönetim ve yedek betikleri, Oracle kurulum rehberi |

## Lisanslar

Uygulama ikonu, [egui/eframe](https://github.com/emilk/egui) projesinin varsayılan ikonudur (MIT veya Apache-2.0).
Bildirim sesleri kodda üretilir; dosya kullanılmaz.
