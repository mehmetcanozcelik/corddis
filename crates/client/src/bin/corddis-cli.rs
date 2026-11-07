//! Corddis komut satırı araçları: cihaz listesi, ses hattı testi, arayüzsüz sohbet.

use anyhow::{bail, Context, Result};
use corddis_client::{audio, chat, loopback, AudioOptions};

const HELP: &str = "\
corddis-cli — Corddis komut satırı araçları (arayüz: corddis.exe)

Kullanım:
  corddis-cli devices                     Ses cihazlarını listeler
  corddis-cli loopback [ses] [loopback]   Kendi sesini tüm ses hattından geçirip dinlersin
  corddis-cli chat \"<davet>\" [ses]        Sunucuya bağlan: yazılı sohbet + sesli kanal

Ses seçenekleri (loopback ve chat):
  --input <ad>          Mikrofon (adın bir parçası yeterli; varsayılan: sistem varsayılanı)
  --output <ad>         Kulaklık/hoparlör (varsayılan: sistem varsayılanı)
  --no-denoise          Gürültü engellemeyi kapat (ses algılama yine çalışır)
  --no-agc              Otomatik ses seviyesini kapat
  --threshold <0..1>    Ses algılama hassasiyeti, konuşma olasılığı eşiği (varsayılan 0.5)
  --min-db <dBFS>       Bu seviyenin altındaki ses konuşma sayılmaz (varsayılan -50)
  --bitrate <bps>       Opus bitrate (varsayılan 32000)

Yalnızca loopback:
  --loss <yüzde>        Sahte ağda paket kaybı, ör. 5 (varsayılan 0)
  --jitter <ms>         Sahte ağda rastgele gecikme oynaması, ör. 30 (varsayılan 0)
  --jitter-target <n>   Jitter buffer hedefi, çerçeve sayısı (varsayılan 2 = 40 ms)
  --seconds <n>         Enter beklemeden n saniye sonra kapan (ölçüm için)

Kulaklık kullan; hoparlörde mikrofon sesi geri besleme yapar.
";

fn main() {
    if let Err(e) = run() {
        eprintln!("hata: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = if args.is_empty() { String::new() } else { args.remove(0) };
    match cmd.as_str() {
        "devices" => audio::device::list_devices(),
        "loopback" => {
            let mut lo = loopback::Options::default();
            let mut audio = AudioOptions::default();
            parse_options(args, &mut audio, Some(&mut lo))?;
            lo.audio = audio;
            loopback::run(lo)
        }
        "chat" => {
            if args.is_empty() {
                bail!("davet bağlantısı gerekli: corddis-cli chat \"corddis://...\"");
            }
            let invite = args.remove(0);
            let mut audio = AudioOptions::default();
            parse_options(args, &mut audio, None)?;
            chat::run(&invite, audio)
        }
        "set-invite" => {
            // Arayüzün davet ekranını atlamak için (test/kurulum). CORDDIS_CONFIG_DIR'e uyar.
            let invite = args.first().context("davet bağlantısı gerekli")?;
            invite.parse::<corddis_proto::invite::Invite>()?;
            let mut cfg = corddis_client::config::Config::load();
            cfg.set_invite(Some(invite));
            cfg.save();
            println!("Davet kaydedildi.");
            Ok(())
        }
        _ => {
            print!("{HELP}");
            Ok(())
        }
    }
}

/// Seçenekleri tek geçişte okur. `lo` yoksa (chat) loopback'e özel seçenekler reddedilir.
fn parse_options(args: Vec<String>, a: &mut AudioOptions, mut lo: Option<&mut loopback::Options>) -> Result<()> {
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let mut val = || it.next().with_context(|| format!("{flag} bir değer bekliyor"));
        match (flag.as_str(), lo.as_deref_mut()) {
            ("--input", _) => a.input = Some(val()?),
            ("--output", _) => a.output = Some(val()?),
            ("--no-denoise", _) => a.capture.denoise = false,
            ("--no-agc", _) => a.capture.agc = false,
            ("--threshold", _) => a.capture.vad_prob = val()?.parse().context("--threshold")?,
            ("--min-db", _) => a.capture.vad_min_db = val()?.parse().context("--min-db")?,
            ("--bitrate", _) => a.capture.bitrate = val()?.parse().context("--bitrate")?,
            ("--loss", Some(o)) => o.loss_pct = val()?.parse().context("--loss")?,
            ("--jitter", Some(o)) => o.jitter_ms = val()?.parse().context("--jitter")?,
            ("--jitter-target", Some(o)) => o.jitter_target = val()?.parse().context("--jitter-target")?,
            ("--seconds", Some(o)) => o.seconds = Some(val()?.parse().context("--seconds")?),
            (other, _) => bail!("bilinmeyen seçenek: {other}\n\n{HELP}"),
        }
    }
    Ok(())
}
