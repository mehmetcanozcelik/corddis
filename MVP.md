# Corddis — MVP Dokümanı

> Arkadaş grubu için, oyun oynarken FPS'e ve ping'e dokunmayan, sıfır maliyetli, Windows sesli/yazılı sohbet uygulaması.
> Tarih: 2026-11-07 · Durum: Taslak v1

---

## 1. Özet

| Konu | Karar |
|---|---|
| Kullanıcı sayısı | 2–5 kişi, hepsi Türkiye'de |
| MVP özellikleri | Sesli kanal (açık mikrofon + otomatik ses algılama, mikrofon kapatma/mute), yazılı sohbet |
| İstemci | **Rust + egui**, tek `.exe`, WebView/Electron yok |
| Sunucu | **Rust** (tokio), tek binary, **Oracle Cloud Always Free** üzerinde |
| Ses | Opus 48 kHz mono, 20 ms çerçeve, ~24–32 kbps, sunucu üzerinden UDP relay |
| Sohbet | WebSocket (TLS), SQLite'ta saklanır |
| Maliyet | **0 TL** (Oracle için yalnızca kart doğrulaması gerekiyor) |

**En önemli kural:** Uygulama oyun açıkken arka planda *görünmez* olmalı. Hedefler §3'te sayılarla tanımlı; her sürüm bu sayılarla test edilir.

---

## 2. Önce dürüst bir not: hazır alternatif

Bu ihtiyacı birebir karşılayan hazır ve ücretsiz bir çözüm var: **Mumble** (istemci) + **Murmur** (sunucu). Açık kaynak, Opus kullanıyor, düşük gecikmeli, PTT ve yazılı sohbeti var, Oracle Free üzerinde de çalışır. Arayüzü eski görünüyor ve Discord gibi hissettirmiyor.

Kendi uygulamanızı yazmanın anlamlı olduğu durumlar: arayüzü ve özellikleri tam olarak siz belirlemek istiyorsanız, öğrenmek istiyorsanız ya da ileride ekleyeceğiniz özellikler (ekran paylaşımı, soundboard vb.) üzerinde tam kontrol istiyorsanız. Bu doküman kendiniz yazacağınızı varsayıyor. İsterseniz ilk hafta Mumble kurup gecikme ve FPS için **referans ölçüm** olarak kullanabilirsiniz.

---

## 3. Performans hedefleri (kabul kriterleri)

Bu sayılar MVP'nin "bitti" tanımıdır. Ölçüm yöntemi §11'de.

| Metrik | Hedef | Discord (karşılaştırma için, yaklaşık) |
|---|---|---|
| RAM (sesli kanalda, pencere tepside) | **< 60 MB** | 300–600 MB |
| CPU (kanalda, kimse konuşmuyorken) | **< %1** (tek çekirdek; mikrofon + gürültü engelleme sürekli dinliyor) | %1–3 |
| CPU (2 kişi aynı anda konuşurken) | **< %2** (tek çekirdek) | %3–6 |
| GPU (pencere küçültülmüş/tepside) | **%0, hiç frame çizilmiyor** | Sürekli düşük kullanım |
| Upload (konuşurken) | **~50 kbps** (32 kbps ses + IP/UDP/şifreleme başlıkları; M1'de ölçüldü: 46–54 kbps) | ~64+ kbps |
| Upload/Download (kimse konuşmuyorken) | **~0** (yalnızca 15 sn'de bir keepalive) | Sürekli trafik |
| Ağızdan kulağa gecikme | **< 150 ms** | 100–200 ms |
| Oyunda FPS kaybı (ortalama ve %1 low) | **Ölçüm hatası sınırında (< %1)** | — |
| Oyun ping'ine etkisi | **Ölçülemeyecek kadar küçük** | — |

**Ping neden etkilenmez?** Oyun ping'ini bozan şey hattın dolmasıdır (bufferbloat). ~50 kbps ses akışı, 20 Mbps'lik bir upload hattının %0,25'i; hattı dolduramaz. Asıl risk, uygulamanın arka planda başka bir şey indirmesi (güncelleme, resim önizleme vb.). Bu yüzden MVP'de **arka planda hiçbir indirme yok**.

---

## 4. Mimari

```
   ┌──────────── Oracle Cloud Free VM (Frankfurt/Milano vb.) ────────────┐
   │  corddis-server (tek Rust binary, ~10–20 MB RAM)                    │
   │   ├─ TCP 443  : WebSocket (TLS) → giriş, kanallar, sohbet, durum    │
   │   ├─ UDP 7777 : Ses relay (paketi alır, kanaldaki diğerlerine yollar)│
   │   └─ SQLite   : kullanıcılar, kanallar, mesaj geçmişi               │
   └─────────────────────────────────────────────────────────────────────┘
          ▲ TLS (kontrol + sohbet)              ▲ UDP (yalnızca ses)
          │                                     │
   ┌──────┴─────────────────────────────────────┴──────┐
   │  corddis.exe (Windows istemcisi, Rust)            │
   │   ├─ Ses thread'i (WASAPI, MMCSS önceliği)        │
   │   ├─ Ağ thread'leri (tokio, 1–2 worker)           │
   │   ├─ Global mute kısayolu (RegisterHotKey)        │
   │   └─ UI (egui) — yalnızca pencere açıkken çizer   │
   └───────────────────────────────────────────────────┘
```

### 4.1 Neden P2P değil de sunucu üzerinden ses (relay)?

| | P2P mesh | Sunucu relay (seçilen) |
|---|---|---|
| Gecikme | En düşük (TR→TR doğrudan) | +~40–60 ms (TR → Frankfurt → TR) |
| Upload | Konuşan kişi N−1 kopya yollar | Herkes **tek** kopya yollar |
| NAT/CGNAT sorunu | Hole punching gerekir, bazı TR ISS'lerinde (CGNAT, mobil hat) çalışmaz | **Her zaman çalışır** |
| Kodlama karmaşıklığı | Yüksek (STUN/TURN, ICE benzeri mantık) | Düşük |

5 kişide relay'in ek gecikmesi 150 ms hedefinin içinde kalıyor; Discord da sesi kendi sunucuları üzerinden taşıyor. Bu yüzden MVP'de relay kullanılıyor. P2P (hole punching + relay'e geri düşme) **Faz 2** konusu.

### 4.2 Neden WebRTC değil de özel UDP protokolü?

WebRTC kütüphaneleri büyük (ICE, DTLS, SRTP, RTCP, simulcast…) ve 5 kişilik, sunucu relay'li bir sistemde bunların çoğuna ihtiyaç yok. Özel protokol şunları sağlıyor:
- Daha az RAM/CPU, daha küçük binary.
- Her byte üzerinde kontrol (gereksiz RTCP raporu, keepalive vb. yok).
- Anlaşılır ve hata ayıklaması kolay bir kod tabanı.

Bedeli: şifreleme ve paket formatını kendimiz tanımlıyoruz (§7). İleride tarayıcı/mobil istemci gerekirse WebRTC'ye geçiş seçeneği açık kalıyor (`str0m` crate'i).

---

## 5. Teknoloji seçimleri

### 5.1 İstemci (Windows)

| Parça | Seçim | Gerekçe |
|---|---|---|
| Dil | Rust (stable) | GC yok, öngörülebilir bellek, tek statik `.exe` |
| UI | `egui` + `eframe` (**glow** backend) | Hafif. Yalnızca olay olunca çizer (reactive mode); tepsideyken 0 frame |
| Ses I/O | `cpal` (WASAPI, shared mode) | Windows'un doğal ses API'si |
| Codec | `opus` / `audiopus` (libopus) | Ses için endüstri standardı |
| Gürültü engelleme + ses algılama | `nnnoiseless` (RNNoise'un saf Rust portu) | Açık mikrofonda varsayılan olarak açık. Hem gürültüyü (klavye, fan) temizler hem de "bu konuşma mı?" olasılığı verir; ses algılama (VAD) buna dayanır |
| Yeniden örnekleme | `rubato` (yalnızca cihaz 48 kHz değilse) | |
| Ağ | `tokio` (worker sayısı 1–2 ile sınırlı), `tokio-tungstenite` + `rustls` | |
| Şifreleme | `chacha20poly1305` | Hızlı, AES-NI gerektirmez |
| Mute kısayolu | `windows` crate: `RegisterHotKey` (ör. `Ctrl+Shift+M`) | Oyun odaktayken de mikrofonu kapatıp açar. Klavye hook'u **kullanılmaz**; bu yüzden anti-cheat riski yok |
| Tepsi | `tray-icon` | |
| Ayarlar | `%APPDATA%\Corddis\config.toml` (`serde` + `toml`) | |

**Neden Tauri değil?** Tauri, WebView2 (Chromium tabanlı) kullanıyor; tek başına 80–150 MB RAM ve GPU süreci getiriyor. egui ile 30–60 MB bandındayız.

### 5.2 Sunucu

| Parça | Seçim |
|---|---|
| Dil/runtime | Rust + `tokio` |
| WebSocket | `axum` (ws) veya `tokio-tungstenite` |
| TLS | `rustls` + **self-signed sertifika, istemcide sertifika parmak izi sabitlenmiş (pinning)**. Domain gerekmez. |
| Veritabanı | SQLite (`rusqlite`, WAL modu) |
| Yönetim | CLI alt komutları: `corddis-server user add <isim>` → davet token'ı üretir |
| Deploy | `aarch64-unknown-linux-gnu` için `cargo zigbuild` ile derle → `scp` → `systemd` servisi |

### 5.3 Depo yapısı

```
corddis/
├─ Cargo.toml            # workspace
├─ crates/
│  ├─ proto/             # ortak tipler: WS mesajları, UDP paket formatı, sabitler
│  ├─ client/            # corddis.exe
│  │  └─ src/{main.rs, audio/, net/, ptt.rs, ui/, config.rs}
│  └─ server/            # corddis-server
│     └─ src/{main.rs, ws.rs, voice_relay.rs, db.rs, admin.rs}
├─ deploy/
│  ├─ corddis.service    # systemd
│  └─ setup-oracle.md    # sunucu kurulum adımları
└─ MVP.md
```

---

## 6. Ses hattı (en kritik kısım)

```
Mikrofon ─► WASAPI capture (10 ms) ─► [mute kontrolü] ─► RNNoise ─► [VAD kapısı] ─► Opus encode (20 ms)
        ─► şifrele ─► UDP ─► sunucu ─► UDP ─► şifre çöz ─► jitter buffer (konuşmacı başına)
        ─► Opus decode / PLC ─► kişi bazlı ses seviyesi ─► mix + soft-clip ─► WASAPI render
```

| Parametre | Değer | Not |
|---|---|---|
| Örnekleme | 48 kHz, mono | |
| Çerçeve | 20 ms | 10 ms daha az gecikme verir ama paket sayısını ikiye katlar; 20 ms dengeli |
| Bitrate | 32 kbps VBR (ayarlardan 16–64) | Konuşma için 24–32 kbps yeterli |
| Opus modu | `VOIP`, complexity 5 | CPU/kalite dengesi |
| In-band FEC | Açık, beklenen kayıp %5 | Kayıp paketi bir sonrakinden kurtarır |
| DTX | Açık | Sessizlikte paket yollamaz |
| Jitter buffer | 40 ms, ağdaki oynamaya göre 120 ms'ye kadar uyarlanır (≈ 20 ms + 3 × jitter) | Konuşmacı başına ayrı; hedef yalnızca konuşma başlarında değişir |
| Cihaz buffer'ı | ~10 ms | WASAPI shared mode |

**Kural: Mikrofon mute'luysa veya VAD sessizlik diyorsa encoder'a hiç veri gitmez, ağa hiç paket çıkmaz.** Mikrofon açık dursa bile konuşulmayan anlarda trafik sıfırdır. Konuşma bitince 3 "sessizlik" çerçevesi yollanır ki karşı taraf yumuşak kapansın.

**Ses algılama (VAD), mikrofon sürekli açık olduğu için:**
- Karar, RNNoise'un konuşma olasılığı ile ayarlanabilir bir ses seviyesi eşiğinin birlikte geçilmesine bağlı. Böylece klavye/fare tıklaması tek başına kanalı açmaz.
- Açılma hızlı (ilk konuşma çerçevesinde), kapanma gecikmeli (~300 ms "hangover"), böylece kelime sonları kesilmez.
- Ayarlarda canlı seviye göstergesi ve eşik kaydırıcısı; kullanıcı kendi ortamına göre ayarlar.
- **Mute:** Mikrofon tamamen kapatılır (yakalama durur değil, veriler atılır; böylece açınca anında çalışır). Mute durumu diğerlerine kanaldaki ikonla gösterilir.
- **Deafen:** Hem gelen ses hem mikrofon kapanır.

**Gecikme bütçesi (tahmini):** yakalama 10 + çerçeve 20 + encode ~1 + ağ TR→DE→TR ~70–90 + jitter 40 + decode/çalma ~10 ≈ **150 ms civarı.** Oracle'da Türkiye'ye en yakın bölgeyi seçmek bu sayıyı doğrudan düşürür (§10).

**Eko engelleme (AEC):** MVP'de yok; FPS oyuncuları kulaklık kullanıyor varsayımı. Hoparlör kullanan olursa Faz 2'de `webrtc-audio-processing` eklenir.

---

## 7. Protokoller

### 7.1 Kontrol kanalı — WebSocket (TLS, JSON)

`serde` ile tanımlı, `type` alanlı JSON mesajları. Kontrol trafiği zaten çok seyrek olduğu için JSON'un ek yükü önemsiz.

İstemci → Sunucu:
```jsonc
{ "type": "auth",        "token": "…" }
{ "type": "join_voice",  "channel_id": 1 }
{ "type": "leave_voice" }
{ "type": "send_msg",    "channel_id": 2, "text": "gg" }
{ "type": "history",     "channel_id": 2, "before_id": 1234, "limit": 50 }
{ "type": "set_state",   "muted": true, "deafened": false }
```

Sunucu → İstemci:
```jsonc
{ "type": "auth_ok",     "user_id": 3, "udp_session": 918273, "udp_key": "<base64 32 byte>", "udp_port": 7777 }
{ "type": "snapshot",    "channels": [...], "users": [...], "voice_members": {...} }
{ "type": "msg",         "id": 1235, "channel_id": 2, "user_id": 3, "text": "gg", "ts": 1762500000 }
{ "type": "voice_join",  "channel_id": 1, "user_id": 4 }
{ "type": "voice_leave", "channel_id": 1, "user_id": 4 }
{ "type": "user_state",  "user_id": 4, "muted": true, "deafened": false }
{ "type": "error",       "code": "bad_token" }
```

- WebSocket ping her 30 sn'de bir; `TCP_NODELAY` açık.
- Bağlantı koparsa üstel geri çekilmeyle (1, 2, 4 … en fazla 30 sn) yeniden bağlanma ve sesli kanala otomatik dönüş.

### 7.2 Ses kanalı — UDP (ikili format)

Her bağlantıda sunucu, istemciye özel 32 byte'lık bir `udp_key` üretip TLS üzerinden yollar. UDP paketleri bu anahtarla ChaCha20-Poly1305 ile şifrelenir. Sunucu paketi çözer, kanaldaki her alıcı için o alıcının anahtarıyla yeniden şifreleyip yollar (5 kişide CPU maliyeti önemsiz).

```
İstemci → Sunucu
┌─────┬──────┬────────────┬─────────┬──────────────────────────┬─────────┐
│ ver │ type │ session u32│ seq u32 │ şifreli Opus çerçevesi   │ tag 16B │
│ 1B  │ 1B   │ 4B         │ 4B      │ ~60–80B                  │         │
└─────┴──────┴────────────┴─────────┴──────────────────────────┴─────────┘
  başlık (10 byte) = AEAD "associated data"; nonce = yön (1B) ‖ 0 (3B) ‖ seq (u64, 8B)

Sunucu → İstemci
│ ver │ type │ speaker_id u16 │ seq u32 │ counter u64 │ şifreli Opus │ tag │
  başlık (16 byte); nonce = yön (1B) ‖ 0 (3B) ‖ counter (8B)
```

- `seq` konuşmacıya aittir (jitter buffer bunu kullanır). İki konuşmacı aynı alıcıya aynı `seq` ile ulaşabileceği için sunucu→istemci yönünde nonce olarak **kullanılamaz**; sunucu her alıcı için ayrı, sürekli artan bir `counter` tutar ve nonce onunla üretilir. Uygulaması `crates/proto/src/voice.rs`'de.

- `type`: `0x01` ses, `0x02` keepalive, `0x03` konuşma-bitti.
- İstemci başlığındaki `seq` bir **paket sayacıdır**: her pakette artar, bağlantı boyunca sıfırlanmaz (yeni bağlantı = yeni anahtar). Sunucu bununla replay koruması yapar (64 paketlik kayan pencere).
- **Ses sıra numarası** şifreli içeriğin ilk 4 byte'ıdır (`ses_seq u32 | opus`) ve yalnızca ses çerçevelerinde artar. Böylece keepalive'lar alıcının jitter buffer'ında boşluk oluşturmaz. Sunucu bunu okuyup alıcıya giden başlığın `seq` alanına koyar.
- Keepalive: sesli kanaldayken 5 sn'de bir, ~40 byte. Sunucu aynı sayaçla geri yollar; istemci bundan ping ölçer.
- Mute'lu kullanıcının sesi sunucuda da kesilir; sağır kullanıcıya ses hiç gönderilmez (bant genişliği harcanmaz).
- Sunucu, istemcinin UDP adresini ilk geçerli paketten öğrenir; IP/port değişirse (NAT yenilemesi) güncellenir.

---

## 8. Oyun performansı için uygulama kuralları

Bu bölüm kod incelemesinde kontrol listesi olarak kullanılmalı.

**GPU**
- [ ] egui reactive mode: yalnızca giriş, yeni mesaj veya "konuşuyor" göstergesi değiştiğinde yeniden çizim.
- [ ] Pencere küçültülmüş/tepsideyken **hiç** `request_repaint` yok.
- [ ] Animasyon, gölge, blur, GIF yok. Konuşuyor göstergesi basit bir renk değişimi.
- [ ] VSync açık (çizim olursa bile ekran hızını geçmesin).
- [ ] Kapatma düğmesi uygulamayı tepsiye gönderir; ses tepsideyken de devam eder.

**CPU**
- [ ] Ses thread'i MMCSS `"Pro Audio"` görevine kaydolur (`AvSetMmThreadCharacteristicsW`). İş çok küçük olduğu için oyundan zaman çalmaz ama ses kesilmez.
- [ ] tokio runtime en fazla 2 worker thread.
- [ ] Hiçbir yerde busy-loop/polling yok; her şey olay tabanlı (channel, `await`).
- [ ] `timeBeginPeriod` **kullanılmaz** (sistem zamanlayıcısını bozar).
- [ ] RNNoise yalnızca sesli kanaldayken ve mute kapalıyken çalışır; mute'tayken veya kanalda değilken hiç çalışmaz.

**RAM**
- [ ] Mesaj geçmişi bellekte kanal başına en fazla son 200 mesaj; fazlası istenince sunucudan çekilir.
- [ ] Resim önizleme yok (MVP'de linkler düz metin).
- [ ] `release` profilinde `lto = true`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.

**Ağ**
- [ ] Sessizlikte sıfır ses paketi (VAD + DTX).
- [ ] Arka planda otomatik güncelleme indirmesi yok (MVP'de güncelleme elle).
- [ ] Telemetri, analitik, harici istek yok. Uygulama yalnızca kendi sunucunuzla konuşur.

**Mute kısayolu**
- [ ] `RegisterHotKey` ile yalnızca bir tuş kombinasyonu kaydedilir (varsayılan `Ctrl+Shift+M`). Low-level klavye/fare hook'u yok.
- [ ] Kombinasyon oyunda kullanılan tuşlarla çakışmamalı; ayarlardan değiştirilebilir, kayıt başarısız olursa (başka uygulama almışsa) kullanıcıya söylenir.
- [ ] Mute/unmute'ta kısa bir bildirim sesi çalar (oyundayken ekrana bakmadan durumu bilmek için).

---

## 9. MVP kapsamı

### Kapsamda
1. **Hesap:** Admin, sunucuda CLI ile kullanıcı ekler ve davet token'ı üretir. Kullanıcı ilk açılışta sunucu adresini ve token'ı girer; token yerel olarak Windows DPAPI ile şifrelenip saklanır.
2. **Kanallar:** Sabit birkaç kanal (ör. `#genel` yazılı, `🔊 Oyun` ve `🔊 AFK` sesli). Kanal oluşturma/silme yalnızca CLI'dan.
3. **Sesli kanal:** Katıl/ayrıl, açık mikrofon + otomatik ses algılama (eşik ayarlı), mute (buton + global kısayol), deafen, kişi bazlı ses seviyesi (%0–200), konuşanı gösterme.
4. **Yazılı sohbet:** Mesaj gönderme, geçmişi kaydırarak yükleme, okunmamış sayacı, link'i tarayıcıda açma.
5. **Ayarlar:** Giriş/çıkış cihazı seçimi, mute kısayolu, giriş seviyesi göstergesi + ses algılama eşiği, gürültü engelleme aç/kapa, bitrate.
6. **Sistem:** Tepsi ikonu, Windows ile başlat (opsiyonel), kopma sonrası otomatik yeniden bağlanma, tek örnek (ikinci açılış ilk pencereyi öne getirir).
7. **Sesli bildirimler:** Biri kanala girdi/çıktı, mute/unmute (küçük gömülü WAV'lar).

### Kapsam dışı (Faz 2+)
- Ekran paylaşımı / video
- P2P ses
- Dosya/resim yükleme
- Oyun içi overlay (anti-cheat riski nedeniyle önerilmiyor)
- Otomatik güncelleme
- Mobil/web istemci
- Uçtan uca şifreleme (MVP'de sunucu ses paketlerini çözüyor; sunucu sizin olduğu için kabul edilebilir)
- Eko engelleme (AEC)

---

## 10. Sunucu kurulumu (Oracle Cloud Always Free)

1. **Bölge seçimi (kritik, sonradan değişmez):** Kayıt olurken seçilen "home region" kalıcıdır ve Always Free kaynaklar yalnızca orada açılabilir. Kayıttan önce Türkiye'den Oracle bölgelerine ping ölçün (Frankfurt, Milano, Marsilya, Zürih vb.) ve en düşüğünü seçin.
2. **VM:**
   - Tercih: `VM.Standard.A1.Flex` (ARM) — 1 OCPU / 6 GB yeterli fazlasıyla.
   - Popüler bölgelerde ARM kapasitesi sık sık "Out of capacity" hatası veriyor. Alternatif: `VM.Standard.E2.1.Micro` (AMD, 1 GB RAM). Bu sunucu 20 MB altında RAM kullanacağı için o da yeterli.
   - İmaj: Ubuntu 24.04 (minimal).
3. **Atıl kaynak geri alma riski:** Oracle, Always Free hesaplarda 7 gün boyunca çok düşük kullanımda kalan VM'leri geri alabiliyor; bu sunucu doğası gereği çok az kaynak kullanacak. En temiz çözüm hesabı **Pay As You Go**'ya yükseltmek: free tier limitleri içinde kaldıkça ücret çıkmaz ve geri alma politikası uygulanmaz. Yükseltirseniz **mutlaka bütçe alarmı kurun** (ör. 1 USD).
4. **Firewall (en sık takılınan yer — iki katman var):**
   - OCI konsolu → VCN → Security List: `TCP 443` ve `UDP 7777` için ingress kuralı.
   - VM içinde Oracle'ın Ubuntu imajı kendi `iptables` kurallarıyla geliyor; portları burada da açın ve kalıcı yapın (`netfilter-persistent save`).
5. **Servis:** Binary `/opt/corddis/`, veritabanı `/var/lib/corddis/`, `systemd` ile `Restart=always`, ayrı kullanıcı (`corddis`). 443'e bağlanabilmesi için `AmbientCapabilities=CAP_NET_BIND_SERVICE`.
6. **Sertifika:** İlk çalıştırmada sunucu self-signed sertifika üretir ve SHA-256 parmak izini loglar. Bu parmak izi, sunucu adresiyle birlikte arkadaşlara verilen "davet bağlantısının" içinde yer alır, ör. `corddis://1.2.3.4:443?fp=AB12…&token=…`. Bu bağlantı tek seferlik ve kişiye özeldir; kanaldan değil, birebir mesajla paylaşılır.
7. **Yedek:** Günlük `sqlite3 .backup` cron'u (aynı VM'de, son 7 gün).

---

## 11. Test ve ölçüm planı

| Ne | Nasıl |
|---|---|
| FPS etkisi | **PresentMon** veya **CapFrameX** ile aynı oyunda aynı sahnede 3'er dakikalık 3 kayıt: (a) uygulama kapalı, (b) kanalda sessiz, (c) kanalda sürekli konuşma. Ortalama FPS ve %1 low karşılaştırılır. Hedef: fark < %1. |
| RAM/CPU | Görev Yöneticisi yetersiz; **Process Explorer** ile "Private Bytes" ve CPU geçmişi, 30 dk'lık oturum. Bellek sızıntısı için saatlik oturumda RAM sabit kalmalı. |
| GPU | Görev Yöneticisi → GPU sütunu; pencere tepsideyken %0 olmalı. |
| Ağ | **Resource Monitor** → Network; sessizken ~0, konuşurken ~50 kbps up. |
| Gecikme | Yerel loopback modu (kendi sesini 2 sn sonra değil, anında duy) + iki bilgisayar arasında "alkış testi": bir telefonla iki tarafın sesini aynı anda kaydedip dalga formundan farkı ölç. |
| Kötü ağ | Windows'ta **clumsy** aracıyla %5 paket kaybı + 30 ms jitter; ses anlaşılır kalmalı. |
| Ses algılama | Oyun oynarken (klavye/fare sesi, kulaklıktan sızan oyun sesi) 10 dk kayıt: sahte açılma sayısı az olmalı, konuşma başları kesilmemeli. |
| Mute kısayolu | Oynadığınız oyunlar tam ekrandayken kısayolun çalıştığını doğrulayın. |

---

## 12. Yol haritası (kilometre taşları)

Her adım çalışan, test edilebilir bir şey üretir.

| # | Kilometre taşı | Çıktı / "Bitti" tanımı |
|---|---|---|
| M0 | Workspace kurulumu | `cargo build` ile 3 crate derleniyor; CI yok, yerel yeterli |
| M1 | Yerel ses döngüsü | Mikrofon → Opus → decode → kulaklık; gecikme ölçüldü, CPU < %1 |
| M2 | Sunucu iskeleti | WS + TLS + token girişi + SQLite + `user add` CLI; Oracle'a deploy edildi |
| M3 | Ses relay | 2 istemci sunucu üzerinden konuşabiliyor; jitter buffer + FEC çalışıyor |
| M4 | Ses algılama + mute/deafen + kısayol | Oyun odaktayken mute kısayolu çalışıyor; sessizlikte ağ trafiği sıfır |
| M5 | Yazılı sohbet | Mesaj, geçmiş, okunmamış sayacı |
| M6 | Arayüz + ayarlar + tepsi | §9 kapsamının tamamı; tepside GPU %0 |
| M7 | Performans turu | §3 ve §11'deki tüm testler geçti; 5 kişiyle 1 akşamlık oyun oturumu |
| M8 | Dağıtım | Tek `corddis.exe` + davet bağlantısı; kurulum talimatı |

**Durum (2026-11-07):** M0 ✅ · M1 ✅ (`corddis loopback`) · M2 ✅ — Oracle Frankfurt'a kuruldu (ARM A1, 1 OCPU / 6 GB, AD 2; sunucu süreci ~5 MB RAM). İnternet testi: ping 57 ms, jitter 1,4 ms, kayıp 0. Test için arayüz yerine `corddis chat "<davet>"`. Yerel testte sunucu 8 MB RAM kullandı; bu bilgisayardan en yakın Oracle bölgesi Frankfurt (57 ms). · M3 ✅ (yerel): iki istemci aynı makinede sunucu üzerinden konuştu, 749/749 paket kayıpsız, jitter tamponu 40 ms. İstemci 13,8 MB RAM; sürekli konuşup dinlerken CPU %2,7 (tek çekirdek) — §3'teki %2 hedefinin biraz üstünde, M7'de ele alınacak (seçenekler: Opus complexity 5→3, gürültü engellemeyi isteğe bağlı yapmak). Gerçek internet üzerinden (Oracle) henüz denenmedi. İlk ölçüm: RAM 11–12 MB (working set), sürekli konuşmada CPU ~%0,8 (tek çekirdek), çerçeve başına işlem süresi RNNoise ~45 µs + Opus encode ~330 µs + decode ~80 µs, %10 sahte kayıpta kayıpların yarısından fazlası FEC ile kurtarıldı.

**Not:** `.exe` imzasız olacağı için Windows SmartScreen ilk açılışta "Bilinmeyen yayımcı" uyarısı gösterecek ("Ek bilgi → Yine de çalıştır"). Kod imzalama sertifikası ücretli olduğu için MVP'de yok; arkadaş grubu için sorun değil.

---

## 13. Riskler

| Risk | Etki | Önlem |
|---|---|---|
| Oracle ARM kapasitesi yok | Sunucu açılamaz | AMD Micro VM yeterli (§10) |
| Oracle VM'i geri alıyor | Sunucu kaybolur | Pay As You Go + bütçe alarmı; günlük yedek; deploy betiği ile 10 dk'da yeniden kurulum |
| Açık mikrofon arka plan sesi taşıyor | Klavye/oda sesi arkadaşlara gidiyor | RNNoise + eşik; herkes kendi eşiğini ayarlar; tek tuşla mute |
| Seçilen bölgeye gecikme yüksek | > 150 ms ses gecikmesi | Kayıttan önce ping ölçümü; Faz 2'de P2P |
| egui'de Türkçe karakter/emoji | Görüntü sorunu | Gömülü bir font (ör. Noto Sans) + emoji fontu; M6'da test |
| Ses cihazı değişince (kulaklık takıldı) kopma | Ses gider | Cihaz değişikliği olayını dinle, akışı yeniden aç |

---

## 14. Faz 2 fikirleri (öncelik sırasıyla)

1. P2P ses (UDP hole punching, olmazsa relay'e düşme) — gecikmeyi ~40–60 ms azaltır.
2. Ekran paylaşımı — Windows Graphics Capture + donanım encoder (NVENC/AMF/QSV), yalnızca oyun dışında kullanılmak üzere; varsayılan 720p30.
3. Dosya/resim paylaşımı (sunucuda boyut limiti ile).
4. Basit otomatik güncelleme (yalnızca uygulama açılışında kontrol, oyun sırasında asla).
5. Soundboard, kanal yönetimi arayüzü, emoji tepkileri.
