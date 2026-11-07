# Oracle Cloud Always Free üzerine kurulum

Toplam süre: ~30–45 dk. Maliyet: 0 TL. Bu adımları **sizin** yapmanız gerekiyor; hesap açma ve kart doğrulaması kişisel işlemler.

## 1. Bölge seçimi (geri alınamaz)

Kayıt sırasında seçilen "home region" kalıcıdır ve ücretsiz sunucu yalnızca orada açılabilir.

2026-11-07'de bu bilgisayardan ölçülen TCP bağlantı süreleri (en iyi değer, 3 denemeden):

| Bölge | ms |
|---|---|
| **eu-frankfurt-1 (Frankfurt)** | **57** |
| eu-paris-1 | 66 |
| eu-zurich-1 | 67 |
| eu-milan-1 | 72 |
| uk-london-1 | 72 |
| eu-marseille-1 | 74 |
| eu-amsterdam-1 | 77 |
| il-jerusalem-1 / me-jeddah-1 / me-dubai-1 | 119 / 122 / 160 |

**Öneri: Frankfurt.** Bu ölçüm tek bir internet hattından yapıldı; arkadaşlarınızın hattında sonuç farklı olabilir. Herkes kendi bilgisayarında şunu çalıştırıp sonuçları karşılaştırabilir (ms cinsinden bağlantı süresi verir):

```bash
curl.exe -o NUL -s -w "%{time_connect}\n" https://objectstorage.eu-frankfurt-1.oraclecloud.com
```

Ses gecikmesine etkisi kabaca: konuşan → sunucu → dinleyen ≈ bu sürenin bir katı (yarısı gidiş, yarısı dönüş).

## 2. Hesap

1. https://www.oracle.com/cloud/free/ → kayıt. Kart doğrulaması gerekir; Always Free kaynaklar için ücret çekilmez.
2. **Önerilen:** Hesap açıldıktan sonra *Billing → Upgrade to Pay As You Go*. Free tier sınırında kaldıkça ücret çıkmaz ve "atıl sunucuyu geri alma" politikası uygulanmaz (bu sunucu çok az kaynak kullandığı için bu önemli).
3. Yükselttiyseniz hemen *Billing → Budgets* altından **1 USD'lik bütçe alarmı** kurun.

## 3. Sanal makine

*Compute → Instances → Create instance*

- **Image:** Canonical Ubuntu 24.04 (Minimal yeterli)
- **Shape:** `VM.Standard.A1.Flex` (Ampere ARM) — 1 OCPU, 6 GB RAM.
  "Out of capacity" hatası alırsanız: `VM.Standard.E2.1.Micro` (AMD, 1 GB). Bu sunucu ~10 MB RAM kullandığı için o da yeterli.
- **Networking:** Public IPv4 atansın.
- **SSH key:** "Generate a key pair" → özel anahtarı indirin ve saklayın.

Oluşunca *Public IP address*'i not edin (aşağıda `SUNUCU_IP`).

> Sabit IP: Varsayılan "ephemeral" IP, makine silinmedikçe değişmez. Garanti için *Networking → Reserved Public IPs*'ten ücretsiz bir reserved IP alıp makineye bağlayabilirsiniz.

> **Yeni konsolda public IP düğmesi kilitliyse:** *Create instance* formunun içinden yeni VCN oluşturunca "Automatically assign public IPv4 address" açılamıyor. Çözüm: önce *Networking → Virtual cloud networks → Start VCN Wizard → Create VCN with Internet Connectivity* ile `corddis-vcn`'i oluşturun, sonra formda *Select existing virtual cloud network* → `corddis-vcn` → **public** subnet seçin; düğme açılır.
>
> **"Out of capacity for shape VM.Standard.A1.Flex":** *Previous* ile başa dönüp başka bir Availability domain (AD 2, AD 3) seçin. Formda geri dönmek SSH anahtarını değiştirmez; ilk indirdiğiniz anahtar geçerli kalır. (2026-11-07'de AD 1 doluydu, AD 2'de açıldı.)

## 4. Firewall — iki katman da açılmalı

**a) Oracle konsolu:** *Networking → Virtual cloud networks → corddis-vcn → Subnets → public subnet → Security → Default Security List → Security rules → Add Ingress Rules*

| Source CIDR | Protokol | Port |
|---|---|---|
| 0.0.0.0/0 | TCP | 443 |
| 0.0.0.0/0 | UDP | 7777 |

**b) Makinenin içi:** Oracle'ın Ubuntu imajı kendi iptables kurallarıyla gelir ve konsolda açtığınız portları yine de engeller. Bu en sık takılınan adım.

```bash
ssh -i <indirilen-anahtar> ubuntu@SUNUCU_IP
sudo iptables -L INPUT --line-numbers     # REJECT satırının numarasına bakın (genelde 5 veya 6)
sudo iptables -I INPUT 5 -p tcp --dport 443 -m state --state NEW -j ACCEPT
sudo iptables -I INPUT 5 -p udp --dport 7777 -j ACCEPT
sudo netfilter-persistent save
```

Kurallar REJECT satırından **önce** olmalı; `5` yerine kendi REJECT satır numaranızı yazın.

## 5. Sunucuyu derleme

En basit yol, sunucuda derlemek (A1'de ~3–5 dk; 1 GB'lık AMD makinede yavaş olabilir, o durumda alttaki "Windows'tan" yoluna bakın).

```bash
sudo apt update && sudo apt install -y build-essential git sqlite3
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source ~/.cargo/env
# Kodu sunucuya kopyalayın (git deposu varsa git clone, yoksa Windows'tan scp ile):
#   ssh -i <anahtar> ubuntu@SUNUCU_IP mkdir -p corddis
#   scp -r -i <anahtar> crates deploy .cargo Cargo.toml Cargo.lock ubuntu@SUNUCU_IP:~/corddis/
cd ~/corddis && cargo build --release -p corddis-server
```

**Windows'tan çapraz derleme (doğrulanmadı):** `zig` ve `cargo-zigbuild` kurulup `cargo zigbuild --release -p corddis-server --target aarch64-unknown-linux-gnu` ile ARM için derlenebilir. AMD makinede hedef `x86_64-unknown-linux-gnu` olur. Bu yol henüz denenmedi.

## 6. Kurulum

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin corddis
sudo mkdir -p /opt/corddis
sudo cp ~/corddis/target/release/corddis-server /opt/corddis/
sudo cp ~/corddis/deploy/corddis.service /etc/systemd/system/
sudo install -m 755 ~/corddis/deploy/corddis-admin /usr/local/bin/
sudo systemctl daemon-reload
sudo systemctl enable --now corddis
sudo journalctl -u corddis -n 20      # "dinliyor" ve "sertifika parmak izi" satırlarını görmelisiniz
```

## 7. Kullanıcı ekleme

```bash
corddis-admin user add Ali --host SUNUCU_IP
```

Çıkan `corddis://...` bağlantısını yalnızca Ali'ye, birebir mesajla gönderin. Bağlantı şifre gibidir; gruba atmayın. Kaybolursa veya sızarsa:

```bash
corddis-admin user reset Ali --host SUNUCU_IP   # eski bağlantı anında geçersiz olur
```

Diğer komutlar: `corddis-admin user list`, `user remove <isim>`, `channel list`, `channel add <isim> text|voice`, `channel remove <isim>`.

## 8. Bağlantı testi (Windows)

```bash
corddis.exe chat "corddis://SUNUCU_IP:443?fp=...&token=..."
```

`Bağlandın: Ali` görmelisiniz. Hata alırsanız:

| Hata | Sebep |
|---|---|
| zaman aşımı / bağlanılamadı | 4. adım: konsol veya iptables kuralı eksik |
| parmak izi eşleşmiyor | Sunucunun sertifikası değişmiş (veri klasörü silinmiş) → yeni davet üretin |
| davet bağlantısı geçersiz | Token yanlış veya `user reset` yapılmış |

## 9. Yedek

`/var/lib/corddis` içindeki `cert.der` + `key.der` kaybolursa **tüm davet bağlantıları geçersiz olur**; veritabanı kaybolursa kullanıcılar ve mesajlar gider. Günlük yedek (son 7 gün, `/var/backups/corddis`):

```bash
sudo install -m 755 ~/corddis/deploy/corddis-backup /usr/local/sbin/
echo '0 4 * * * root /usr/local/sbin/corddis-backup' | sudo tee /etc/cron.d/corddis-backup
sudo /usr/local/sbin/corddis-backup     # bir kez elle çalıştırıp deneyin
```

Sertifika dosyalarının bir kopyasını ayrıca kendi bilgisayarınızda da saklayın. Bu yedekler aynı makinede durduğu için makine silinirse onlar da gider.

## 10. Güncelleme

```bash
cd ~/corddis && git pull   # veya yeni kodu scp ile kopyalayın
cargo build --release -p corddis-server
sudo install -m 755 target/release/corddis-server /opt/corddis/ && sudo systemctl restart corddis
```

Yeniden başlatma ~1 sn sürer; bağlı istemciler kopar. Otomatik yeniden bağlanma M6'da gelecek.
